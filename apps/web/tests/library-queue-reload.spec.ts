import { expect, test, type Page } from '@playwright/test';

const sessionA = 'A'.repeat(43);
const sessionB = 'E'.repeat(43);
const queuePrefix = 'nntune.library.queue.v1.';
const catalogue = [
	{ id: 'first', title: 'First Song' },
	{ id: 'second', title: 'Second Song', streamUrl: '/api/demo/audio' },
	{ id: 'third', title: 'Third Song' }
];
const queueRows = (page: Page) => page.getByTestId('queue-item');
const currentRow = (page: Page) => page.locator('[data-testid="queue-item"][aria-current="true"]');
const listings = (page: Page) => page.getByTestId('library-item');
const libraryRow = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`);
const deferred = () => Promise.withResolvers<void>();
const storedQueue = (page: Page) => page.evaluate(
	(prefix) => Object.keys(sessionStorage).filter((key) => key.startsWith(prefix))
		.map((key) => JSON.parse(sessionStorage.getItem(key)!)), queuePrefix
);

async function mockLibrary(page: Page) {
	let binding = sessionA;
	await page.route('**/v1/me/likes/session', (route) => route.fulfill({ json: { session_binding: binding } }));
	await page.route('**/api/library', (route) => route.fulfill({
		headers: { 'x-nntune-session-binding': binding }, json: catalogue
	}));
	return { replaceAccount() { binding = sessionB; } };
}

async function addTracks(page: Page) {
	await page.goto('/library');
	await expect(listings(page)).toHaveCount(3);
	for (const { id } of catalogue) await libraryRow(page, id).click();
	await expect(queueRows(page)).toHaveCount(3);
}

test('Library restores arranged order and selection only after authorization', async ({ page }) => {
	const playback: string[] = [];
	await page.route(/\/v1\/plays\/|\/api\/demo\/audio/, (route) => {
		playback.push(new URL(route.request().url()).pathname);
		return route.abort();
	});
	await mockLibrary(page);
	await addTracks(page);
	await page.getByRole('button', { name: 'Move Third Song up', exact: true }).click();
	await page.getByRole('button', { name: 'Select Second Song', exact: true }).click();
	await page.getByRole('button', { name: 'Remove First Song from queue', exact: true }).click();
	await expect(queueRows(page)).toHaveText([/Third Song/, /Second Song/]);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'second');
	await expect(page.getByTestId('audio-player')).toHaveAttribute('src', '/api/demo/audio');
	expect(await storedQueue(page)).toEqual([{ orderIds: ['third', 'second'], currentId: 'second' }]);

	const verifying = deferred(), release = deferred();
	await page.addInitScript(() => {
		const play = HTMLMediaElement.prototype.play;
		HTMLMediaElement.prototype.play = function () {
			document.documentElement.dataset.playCalled = 'true';
			return play.call(this);
		};
	});
	await page.route('**/v1/me/likes/session', async (route) => {
		verifying.resolve();
		await release.promise;
		await route.fulfill({ json: { session_binding: sessionA } });
	}, { times: 1 });
	playback.length = 0;
	await page.reload();
	await verifying.promise;
	try {
		await expect(queueRows(page)).toHaveCount(0);
	} finally {
		release.resolve();
	}
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveText([/Third Song/, /Second Song/]);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'second');
	await expect(page.locator('audio[src], video[src], source[src]')).toHaveCount(0);
	await expect(page.locator('html')).not.toHaveAttribute('data-play-called', 'true');
	expect(playback).toEqual([]);

	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
	expect(await storedQueue(page)).toEqual([{ orderIds: [], currentId: null }]);
	await page.reload();
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveCount(0);
});

test('a consumed share link respects a restored selection and clear', async ({ page }) => {
	await mockLibrary(page);
	const link = '/library?track=first&t=0';
	await page.goto(link);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'first');
	await libraryRow(page, 'third').click();
	await page.getByRole('button', { name: 'Select Third Song', exact: true }).click();
	await page.reload();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'third');

	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
	await page.reload();
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveCount(0);
	expect(await storedQueue(page)).toEqual([{ orderIds: [], currentId: null }]);

	await page.goto('/library');
	await page.goto(link);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'first');
});

test('share verification preserves a pending route navigation and consumes the Library entry', async ({ page }) => {
	await mockLibrary(page);
	const verifying = deferred(), releaseVerification = deferred();
	const loadingDestination = deferred(), releaseDestination = deferred();
	await page.route('**/v1/me/likes/session', async (route) => {
		verifying.resolve();
		await releaseVerification.promise;
		await route.fulfill({ json: { session_binding: sessionA } });
	}, { times: 1 });
	await page.route('**/v1/me/subscription', async (route) => {
		loadingDestination.resolve();
		await releaseDestination.promise;
		await route.fulfill({ json: {} });
	});

	const link = '/library?track=first&t=0';
	try {
		await page.goto(link);
		await verifying.promise;
		await expect(queueRows(page)).toHaveCount(0);
		await page.evaluate(() => {
			const link = document.createElement('a');
			link.href = '/account/security';
			link.dataset.sveltekitPreloadData = 'off';
			link.textContent = 'Open account security';
			document.body.append(link);
		});
		await page.getByRole('link', { name: 'Open account security', exact: true }).click();
		await loadingDestination.promise;
		releaseVerification.resolve();
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'first');
		await expect(page).toHaveURL(link);
		await libraryRow(page, 'third').click();
		await page.getByRole('button', { name: 'Select Third Song', exact: true }).click();
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'third');
	} finally {
		releaseVerification.resolve();
		releaseDestination.resolve();
	}
	await expect(page).toHaveURL('/account/security');
	await expect(page.getByRole('heading', { name: 'Account security' })).toBeVisible();
	await page.goBack();
	await expect(page).toHaveURL(link);
	await page.reload();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'third');
});

test('a failed catalogue load leaves the saved queue unread until Retry', async ({ page }) => {
	await mockLibrary(page);
	await addTracks(page);
	await page.route('**/api/library', (route) => route.fulfill({ status: 503 }), { times: 1 });
	await page.reload();
	await expect(page.getByRole('alert')).toContainText('Library is not reachable');
	await expect(queueRows(page)).toHaveCount(0);
	expect(await storedQueue(page)).toHaveLength(1);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(queueRows(page)).toHaveCount(3);
});

test('Library discards saved IDs on an account replacement with overlapping tracks', async ({ page }) => {
	const { replaceAccount } = await mockLibrary(page);
	await addTracks(page);
	expect(await storedQueue(page)).toHaveLength(1);
	replaceAccount();
	await page.reload();
	await expect(page.getByRole('alert')).toContainText('Your session changed');
	expect(await storedQueue(page)).toEqual([]);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveCount(0);
	await libraryRow(page, 'second').click();
	expect(await storedQueue(page)).toEqual([{ orderIds: ['second'], currentId: 'second' }]);
});

test('logout clears queues in both tabs while a Library reload is pending', async ({ page, context }) => {
	await mockLibrary(page);
	await addTracks(page);
	const other = await context.newPage();
	await mockLibrary(other);
	await addTracks(other);
	expect(await storedQueue(other)).toHaveLength(1);

	const requested = deferred(), release = deferred();
	await page.route('**/api/library', async (route) => {
		requested.resolve();
		await release.promise;
		await route.fulfill({ headers: { 'x-nntune-session-binding': sessionA }, json: catalogue });
	}, { times: 1 });
	await page.reload();
	await requested.promise;
	try {
		await other.route('**/v1/session/logout', (route) => route.fulfill({ json: { ok: true } }));
		await other.getByRole('button', { name: 'Account menu' }).click();
		await other.getByRole('menuitem', { name: 'Sign out', exact: true }).click();
		await expect(other).toHaveURL(/\/login$/);
		await expect(page.getByRole('alert')).toContainText('Your session changed');
		expect(await storedQueue(other)).toEqual([]);
		expect(await storedQueue(page)).toEqual([]);
	} finally {
		release.resolve();
	}
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveCount(0);
	expect(await storedQueue(page)).toEqual([]);
});

test('a cross-tab auth generation change invalidates a saved queue', async ({ page, context }) => {
	await mockLibrary(page);
	await addTracks(page);
	expect(await storedQueue(page)).toHaveLength(1);
	const other = await context.newPage();
	await other.goto(new URL('/login', page.url()).href);
	await other.evaluate(() => localStorage.setItem('nntune.auth.generation.v1', crypto.randomUUID()));
	await expect.poll(() => storedQueue(page)).toEqual([]);
	await page.reload();
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveCount(0);
});

for (const [kind, raw] of [
	['invalid JSON', '['],
	['invalid current ID', '{"orderIds":["first"],"currentId":12}'],
	['duplicate IDs', '{"orderIds":["first","first"],"currentId":"first"}'],
	['stale IDs', '{"orderIds":["first","missing"],"currentId":"first"}'],
	['oversized JSON', `{"orderIds":["first"],"currentId":"first"}${' '.repeat(65_536)}`]
] as const) {
	test(`Library discards ${kind} without filling the queue`, async ({ page }) => {
		await mockLibrary(page);
		await addTracks(page);
		await page.evaluate(({ prefix, raw }) => {
			const key = Object.keys(sessionStorage).find((key) => key.startsWith(prefix))!;
			sessionStorage.setItem(key, raw);
		}, { prefix: queuePrefix, raw });
		await page.reload();
		await expect(listings(page)).toHaveCount(3);
		await expect(queueRows(page)).toHaveCount(0);
		expect(await storedQueue(page)).toEqual([]);
		await libraryRow(page, 'second').click();
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'second');
	});
}

test('a refused storage write keeps the queue usable and removes the older order', async ({ page }) => {
	await mockLibrary(page);
	await addTracks(page);
	expect(await storedQueue(page)).toHaveLength(1);
	await page.evaluate((prefix) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (this === sessionStorage && key.startsWith(prefix)) throw new DOMException('Full', 'QuotaExceededError');
			return setItem.call(this, key, value);
		};
	}, queuePrefix);
	await page.getByRole('button', { name: 'Select Second Song', exact: true }).click();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'second');
	expect(await storedQueue(page)).toEqual([]);
	await page.reload();
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveCount(0);
});
