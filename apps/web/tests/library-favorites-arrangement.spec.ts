import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: the builder must not edit this file or sibling favorites tests.
 * Uses the real library page, scoped browser storage and queue; only HTTP is mocked.
 * No proposed helper imports: RED must be a browser assertion, not a missing API.
 * FAVORDER-01..09 are stable case IDs. Pure movement has no exported baseline seam;
 * these exercise the same ordered-ID contract through its existing UI/storage seams.
 */
const ITEMS = [
	{ id: 'arr-z', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'arr-a', title: 'Alpha Song', artist: 'Cedar' },
	{ id: 'arr-m', title: 'Mike Song', artist: 'Harbor Lights' }
];
const CONTEXT = 'arrangement-account';
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const KEY_BASE = 'nntune.library.favorites.v1';
const KEY = `${KEY_BASE}.${CONTEXT}`;
const ALL = ['arr-z', 'arr-a', 'arr-m'];

function section(page: Page): Locator {
	return page.getByTestId('favorites-section');
}
function move(page: Page, title: string, direction: 'up' | 'down'): Locator {
	return section(page).getByRole('button', { name: `Move favorite ${title} ${direction}`, exact: true });
}
function star(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`)
		.getByRole('button', { name: /^(?:Unstar|Star) / });
}
async function expectIds(page: Page, ids: string[]): Promise<void> {
	await expect.poll(() => page.getByTestId('favorite-item').evaluateAll((nodes) =>
		nodes.map((node) => node.getAttribute('data-id')))).toEqual(ids);
}
async function stored(page: Page): Promise<unknown> {
	return page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? 'null'), KEY);
}
async function open(page: Page, ids: string[], items = ITEMS): Promise<void> {
	await page.addInitScript(({ contextKey, context, key, ids }) => {
		// Seed only the first document, never overwrite the order on reload.
		if (sessionStorage.getItem('test.arrangement.seeded')) return;
		sessionStorage.setItem('test.arrangement.seeded', '1');
		sessionStorage.setItem(contextKey, context);
		localStorage.setItem(key, JSON.stringify(ids));
	}, { contextKey: CONTEXT_KEY, context: CONTEXT, key: KEY, ids });
	await page.route('**/api/library', (route) => route.fulfill({
		status: 200, contentType: 'application/json', body: JSON.stringify(items)
	}));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	await expect(section(page)).toBeVisible();
	await expectIds(page, ids.filter((id) => items.some((item) => item.id === id)));
}
async function activate(page: Page, title: string, direction: 'up' | 'down'): Promise<void> {
	const button = move(page, title, direction);
	// Deliberate assertion-RED on baseline, before an action can time out.
	await expect(button, 'labeled favorite movement control must be available').toBeVisible();
	await expect(button).toBeEnabled();
	await button.click();
}
async function frame(page: Page): Promise<void> {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
}
async function faultStorage(page: Page, kind: 'getter' | 'write'): Promise<void> {
	await page.evaluate(({ kind, base }) => {
		const descriptor = Object.getOwnPropertyDescriptor(window, 'localStorage')!;
		const setItem = Storage.prototype.setItem;
		if (kind === 'getter') {
			Object.defineProperty(window, 'localStorage', { configurable: true, get() {
				throw new DOMException('Storage unavailable', 'SecurityError');
			} });
		} else {
			const storage = window.localStorage;
			Storage.prototype.setItem = function (key, value) {
				if (this === storage && key.startsWith(`${base}.`)) {
					throw new DOMException('Favorites quota', 'QuotaExceededError');
				}
				return setItem.call(this, key, value);
			};
		}
		Reflect.set(window, '__restoreArrangementStorage', () => {
			Object.defineProperty(window, 'localStorage', descriptor);
			Storage.prototype.setItem = setItem;
		});
	}, { kind, base: KEY_BASE });
}

const audits = new WeakMap<Page, { errors: string[]; mutations: string[] }>();

test.beforeEach(async ({ page }) => {
	const audit = { errors: [] as string[], mutations: [] as string[] };
	audits.set(page, audit);
	page.on('pageerror', (error) => audit.errors.push(error.message));
	page.on('request', (request) => {
		if (request.method() !== 'GET') audit.mutations.push(`${request.method()} ${new URL(request.url()).pathname}`);
	});
	// No live backend, including unexpected mutations or external providers.
	await page.context().route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (/^\/(?:api|v1)(?:\/|$)/.test(url.pathname)) return route.abort('blockedbyclient');
		return route.continue();
	});
});

test.afterEach(async ({ page }) => {
	const audit = audits.get(page)!;
	expect.soft(audit.errors, 'arrangement/storage must not cause an uncaught browser error').toEqual([]);
	expect.soft(audit.mutations, 'favorites arrangement must remain device-local').toEqual([]);
});

for (const theme of ['light', 'dark'] as const) {
	test(`[FAVORDER-01:${theme}] empty and singleton have no actionable move or announcement`, async ({ page }) => {
		await page.setViewportSize({ width: 360, height: 780 });
		await open(page, []);
		await page.getByRole('combobox', { name: 'Appearance', exact: true }).selectOption(theme);
		await expect(page.getByTestId('favorites-empty')).toBeVisible();
		await expect(section(page).getByRole('button', { name: /^Move favorite / })).toHaveCount(0);
		await star(page, 'arr-m').click();
		await expectIds(page, ['arr-m']);
		for (const direction of ['up', 'down'] as const) {
			await expect(move(page, 'Mike Song', direction)).toBeVisible();
			await expect(move(page, 'Mike Song', direction)).toBeDisabled();
			await move(page, 'Mike Song', direction).evaluate((node: HTMLButtonElement) => node.click());
		}
		await expectIds(page, ['arr-m']);
		expect(await stored(page)).toEqual(['arr-m']);
		expect(await section(page).locator('[aria-live], [role="status"]').allTextContents()).not.toEqual(
			expect.arrayContaining([expect.stringMatching(/moved|position|1 of 1/i)])
		);
	});

	test(`[FAVORDER-02:${theme}] narrow-screen keyboard moves preserve track focus and announce visible position only on moves`, async ({ page }) => {
		await page.setViewportSize({ width: 360, height: 780 });
		await open(page, ALL);
		await page.getByRole('combobox', { name: 'Appearance', exact: true }).selectOption(theme);
		await expect(move(page, 'Zulu Song', 'up')).toBeDisabled();
		await expect(move(page, 'Mike Song', 'down')).toBeDisabled();
		const button = move(page, 'Mike Song', 'up');
		await expect(button).toBeVisible();
		// Reach via actual Tab navigation rather than a programmatic focus shortcut.
		await page.getByRole('combobox', { name: 'Appearance', exact: true }).focus();
		for (let i = 0; i < 45 && !(await button.evaluate((node) => node === document.activeElement)); i++) {
			await page.keyboard.press('Tab');
		}
		await expect(button).toBeFocused();
		await page.keyboard.press('Enter');
		await expectIds(page, ['arr-z', 'arr-m', 'arr-a']);
		await expect(page.locator('[data-testid="favorite-item"][data-id="arr-m"] :focus')).toHaveCount(1);
		const announcement = section(page).locator('[aria-live="polite"], [role="status"]').filter({ hasText: 'Mike Song' });
		await expect(announcement).toHaveCount(1);
		await expect(announcement).not.toHaveAttribute('aria-live', 'assertive');
		await expect(announcement).toContainText(/2\s*(?:of|\/)\s*3/);
		await button.focus();
		await page.keyboard.press('Space');
		await expectIds(page, ['arr-m', 'arr-z', 'arr-a']);
		await expect(page.locator('[data-testid="favorite-item"][data-id="arr-m"] :focus')).toHaveCount(1);
		await expect(announcement).toContainText(/1\s*(?:of|\/)\s*3/);
		await expect(button).toBeDisabled();
		await announcement.evaluate((node) => {
			Reflect.set(window, '__arrangementAnnouncementMutations', 0);
			new MutationObserver((records) => Reflect.set(window, '__arrangementAnnouncementMutations',
				Reflect.get(window, '__arrangementAnnouncementMutations') + records.length))
				.observe(node, { childList: true, subtree: true, characterData: true });
		});
		for (let i = 0; i < 3; i++) await button.evaluate((node: HTMLButtonElement) => node.click());
		await frame(page);
		expect(await page.evaluate(() => Reflect.get(window, '__arrangementAnnouncementMutations'))).toBe(0);
		await expectIds(page, ['arr-m', 'arr-z', 'arr-a']);
		const geometry = await section(page).getByRole('button', { name: /^Move favorite / }).evaluateAll((nodes) =>
			nodes.map((node) => {
				const r = node.getBoundingClientRect();
				const style = getComputedStyle(node);
				return { left: r.left, right: r.right, width: r.width, height: r.height, visibility: style.visibility };
			}));
		for (const box of geometry) {
			expect(box.left).toBeGreaterThanOrEqual(0);
			expect(box.right).toBeLessThanOrEqual(360);
			expect(box.width).toBeGreaterThan(0);
			expect(box.height).toBeGreaterThan(0);
			expect(box.visibility).toBe('visible');
		}
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
	});
}

test('[FAVORDER-03] swaps visible stable IDs in complete-array slots without displacing hidden IDs', async ({ page }) => {
	await open(page, ['gone-first', 'arr-z', 'gone-between', 'arr-a', 'arr-m', 'gone-last']);
	await activate(page, 'Alpha Song', 'up');
	await expectIds(page, ['arr-a', 'arr-z', 'arr-m']);
	expect(await stored(page)).toEqual(['gone-first', 'arr-a', 'gone-between', 'arr-z', 'arr-m', 'gone-last']);
	await activate(page, 'Alpha Song', 'down');
	await expectIds(page, ALL);
	expect(await stored(page)).toEqual(['gone-first', 'arr-z', 'gone-between', 'arr-a', 'arr-m', 'gone-last']);
	await activate(page, 'Mike Song', 'up');
	await expectIds(page, ['arr-z', 'arr-m', 'arr-a']);
	expect(await stored(page)).toEqual(['gone-first', 'arr-z', 'gone-between', 'arr-m', 'arr-a', 'gone-last']);
	// Previously absent IDs become visible in their original complete-array slots.
	const expanded = [...ITEMS, ...['gone-first', 'gone-between', 'gone-last'].map((id) => ({ id, title: id, artist: '' }))];
	await page.route('**/api/library', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(expanded) }));
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(6);
	await expectIds(page, ['gone-first', 'arr-z', 'gone-between', 'arr-m', 'arr-a', 'gone-last']);
});

test('[FAVORDER-04] reload restores arrangement only after authenticated library success with the existing scoped key', async ({ page }) => {
	await open(page, ALL);
	await activate(page, 'Mike Song', 'up');
	await expectIds(page, ['arr-z', 'arr-m', 'arr-a']);
	expect(await stored(page)).toEqual(['arr-z', 'arr-m', 'arr-a']);
	let release!: () => void;
	const pending = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/api/library', async (route) => {
		await pending;
		await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) });
	});
	await page.reload({ waitUntil: 'domcontentloaded' });
	try {
		await expect(page.getByText('Loading your library…', { exact: true })).toBeVisible();
		await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	} finally { release(); }
	await expectIds(page, ['arr-z', 'arr-m', 'arr-a']);
	expect(await page.evaluate((key) => sessionStorage.getItem(key), CONTEXT_KEY)).toBe(CONTEXT);
	expect(await page.evaluate(() => Object.keys(localStorage).filter((key) => key.startsWith('nntune.library.favorites.')))).toEqual([KEY]);
});

for (const kind of ['getter', 'write'] as const) {
	for (const recovery of ['move', 'toggle'] as const) {
		test(`[FAVORDER-05:${kind}:${recovery}] storage failure keeps arrangement authoritative through refresh and later full-list save`, async ({ page }) => {
			await open(page, ['arr-z', 'hidden', 'arr-a', 'arr-m']);
			await faultStorage(page, kind);
			await activate(page, 'Mike Song', 'up');
			await expectIds(page, ['arr-z', 'arr-m', 'arr-a']);
			await activate(page, 'Zulu Song', 'down');
			await expectIds(page, ['arr-m', 'arr-z', 'arr-a']);
			await page.route('**/api/library', (route) => route.fulfill({
				status: 200, contentType: 'application/json',
				body: JSON.stringify(ITEMS.map((item) => item.id === 'arr-a' ? { ...item, artist: 'Refreshed artist' } : item))
			}));
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
			await expect(page.locator('[data-testid="library-item"][data-id="arr-a"]')).toContainText('Refreshed artist');
			await expectIds(page, ['arr-m', 'arr-z', 'arr-a']);
			await page.evaluate(() => Reflect.get(window, '__restoreArrangementStorage')());
			if (recovery === 'move') {
				await activate(page, 'Alpha Song', 'up');
				await expectIds(page, ['arr-m', 'arr-a', 'arr-z']);
				expect(await stored(page)).toEqual(['arr-m', 'hidden', 'arr-a', 'arr-z']);
			} else {
				await star(page, 'arr-z').click();
				await expectIds(page, ['arr-m', 'arr-a']);
				expect(await stored(page)).toEqual(['arr-m', 'hidden', 'arr-a']);
			}
			await page.reload();
			await expectIds(page, recovery === 'move' ? ['arr-m', 'arr-a', 'arr-z'] : ['arr-m', 'arr-a']);
		});
	}
}

test('[FAVORDER-06] remove and re-star use the latest arranged membership without duplication', async ({ page }) => {
	await open(page, ALL);
	await activate(page, 'Mike Song', 'up');
	await expectIds(page, ['arr-z', 'arr-m', 'arr-a']);
	await star(page, 'arr-z').click();
	await expectIds(page, ['arr-m', 'arr-a']);
	await expect(move(page, 'Mike Song', 'up')).toBeDisabled();
	await star(page, 'arr-z').click();
	await expectIds(page, ['arr-m', 'arr-a', 'arr-z']);
	await activate(page, 'Zulu Song', 'up');
	await expectIds(page, ['arr-m', 'arr-z', 'arr-a']);
	expect(await stored(page)).toEqual(['arr-m', 'arr-z', 'arr-a']);
});

test('[FAVORDER-07] an auth transition purges a pending arrangement and never restores another account order', async ({ page }) => {
	await open(page, ALL);
	await faultStorage(page, 'write');
	await activate(page, 'Mike Song', 'up');
	await expectIds(page, ['arr-z', 'arr-m', 'arr-a']);
	const other = await page.context().newPage();
	await other.goto('/login');
	await other.evaluate((key) => {
		localStorage.setItem(key, 'arrangement-auth-transition');
		localStorage.removeItem(key);
	}, TRANSITION_KEY);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await other.close();
	await page.evaluate(() => Reflect.get(window, '__restoreArrangementStorage')());
	expect(await stored(page)).toBeNull();
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	const nextContext = await page.evaluate((key) => sessionStorage.getItem(key), CONTEXT_KEY);
	expect(nextContext).not.toBeNull();
	expect(nextContext).not.toBe(CONTEXT);
	// Even a surviving old-account record must not bootstrap into the new scope.
	await page.evaluate((key) => localStorage.setItem(key, JSON.stringify(['arr-a', 'arr-m'])), KEY);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expectIds(page, []);
	await star(page, 'arr-z').click();
	await expectIds(page, ['arr-z']);
	expect(await page.evaluate(({ base, context }) => JSON.parse(localStorage.getItem(`${base}.${context}`) ?? 'null'),
		{ base: KEY_BASE, context: nextContext })).toEqual(['arr-z']);
});

test('[FAVORDER-08] movement leaves main ordering, stars, selected queue and request counts unchanged', async ({ page }) => {
	const requests: string[] = [];
	const errors: string[] = [];
	page.on('request', (request) => requests.push(`${request.method()} ${new URL(request.url()).pathname}`));
	page.on('pageerror', (error) => errors.push(error.message));
	await open(page, ALL);
	await page.getByRole('combobox', { name: 'Order tracks', exact: true }).selectOption('title');
	await page.locator('[data-testid="library-item"][data-id="arr-a"]')
		.getByRole('button', { name: /^Alpha Song/ }).click();
	await page.locator('[data-testid="library-item"][data-id="arr-z"]')
		.getByRole('button', { name: /^Zulu Song/ }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	const queueBefore = await page.getByTestId('queue-item').allTextContents();
	const nowBefore = await page.getByTestId('now-playing').innerText();
	await frame(page);
	const apiBefore = requests.filter((request) => / \/(?:api|v1)(?:\/|$)/.test(request));
	await activate(page, 'Mike Song', 'up');
	await activate(page, 'Zulu Song', 'down');
	await expectIds(page, ['arr-m', 'arr-z', 'arr-a']);
	await frame(page);
	expect(await page.getByTestId('library-item').evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id')))).toEqual(['arr-a', 'arr-m', 'arr-z']);
	await expect(page.getByRole('combobox', { name: 'Order tracks', exact: true })).toHaveValue('title');
	for (const id of ALL) await expect(star(page, id)).toHaveAttribute('aria-pressed', 'true');
	expect(await page.getByTestId('queue-item').allTextContents()).toEqual(queueBefore);
	await expect(page.getByTestId('now-playing')).toHaveText(nowBefore);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	expect(requests.filter((request) => / \/(?:api|v1)(?:\/|$)/.test(request))).toEqual(apiBefore);
	expect(requests.filter((request) => !request.startsWith('GET '))).toEqual([]);
	expect(errors).toEqual([]);
});

test('[FAVORDER-09] a refreshed-away favorite has no remaining move action and surviving neighbors use visible boundaries', async ({ page }) => {
	await open(page, ['arr-z', 'hidden', 'arr-a', 'arr-m']);
	await page.route('**/api/library', (route) => route.fulfill({
		status: 200, contentType: 'application/json', body: JSON.stringify([ITEMS[0], ITEMS[2]])
	}));
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expectIds(page, ['arr-z', 'arr-m']);
	await expect(move(page, 'Alpha Song', 'up')).toHaveCount(0);
	await expect(move(page, 'Zulu Song', 'up')).toBeDisabled();
	await expect(move(page, 'Mike Song', 'down')).toBeDisabled();
	await activate(page, 'Mike Song', 'up');
	await expectIds(page, ['arr-m', 'arr-z']);
	expect(await stored(page)).toEqual(['arr-m', 'hidden', 'arr-a', 'arr-z']);
});
