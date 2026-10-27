import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned supplement to the protected play-from-top acceptance bank
 * (tests/library-play-displayed.spec.ts) and the metadata-only bulk contract
 * pinned by tests/library-bulk-queue.spec.ts. The protected banks stay
 * unchanged; every case here pins a behavior they leave open:
 *
 * - the first appended track (not the first displayed slot) is selected when
 *   the first displayed track is already queued while later ones are missing;
 * - a resolving play() announces the exact Playing outcome, and a rejected
 *   play() names paused/blocked without ever claiming Playing;
 * - a deferred play() outcome is fenced by each direct queue edit and by an
 *   auth transition, while a no-op Next (same queue object) leaves the
 *   pending outcome alone and a real Next still fences it;
 * - the bulk action appends stream-bearing items as metadata only: no audio
 *   element mounts and play() is never attempted.
 *
 * From apps/web: `CI=1 pnpm exec playwright test
 * tests/library-play-displayed-builder.spec.ts --workers=1`. Real /library,
 * real queue and real ordering; only /api/library is mocked and media
 * requests are refused. No backend/provider/production requests.
 */

const LABEL = 'Play displayed from the top';
const BULK_LABEL = 'Add displayed tracks to queue';
const ITEMS = [
	{ id: 'shown-zulu', title: 'Zulu Song', artist: 'Bravo Band' },
	{ id: 'shown-alpha', title: 'Alpha Song', artist: 'Zulu Band' },
	{ id: 'shown-untitled', title: '   ', artist: '   ' },
	{ id: 'shown-middle', title: 'Middle Song', artist: 'Alpha Band' },
	{ id: 'shown-echo', title: 'Echo Song', artist: 'Echo Band' }
];
const STREAM_ITEMS = [
	{ id: 'shown-zulu', title: 'Zulu Song', artist: 'Bravo Band', streamUrl: '/__playall-build__/zulu.mp3' },
	{ id: 'shown-alpha', title: 'Alpha Song', artist: 'Zulu Band', streamUrl: '/__playall-build__/alpha.mp3' },
	{
		id: 'shown-middle',
		title: 'Middle Song',
		artist: 'Alpha Band',
		streamUrl: '/__playall-build__/middle.mp3'
	}
];
const SOLO_ITEM = [
	{ id: 'solo-track', title: 'Solo Song', artist: 'Solo Band', streamUrl: '/__playall-build__/solo.mp3' }
];

function action(page: Page): Locator {
	return page.getByRole('button', { name: LABEL, exact: true });
}

function bulkAction(page: Page): Locator {
	return page.getByRole('button', { name: BULK_LABEL, exact: true });
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function rowAction(page: Page, id: string): Locator {
	return row(page, id).getByRole('button').filter({ hasNotText: /[★☆]/ }).first();
}

function announcement(page: Page): Locator {
	return page.getByTestId('queue-announcement');
}

async function mockLibrary(page: Page, items: unknown = ITEMS) {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(items) })
	);
}

async function openLibrary(page: Page, count: number) {
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(count);
}

async function expectQueue(page: Page, ids: string[], current: string) {
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-track-id'))
	)).toEqual(ids);
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.filter((item) => item.getAttribute('aria-current') === 'true')
			.map((item) => item.getAttribute('data-track-id'))
	)).toEqual([current]);
	await expect(page.getByTestId('queue-position')).toContainText(`Track ${ids.indexOf(current) + 1} of ${ids.length}`);
}

async function stubRejectPlay(page: Page) {
	await page.addInitScript(() => {
		const scope = window as unknown as { __playCalls?: number[] };
		scope.__playCalls = [];
		HTMLMediaElement.prototype.play = function () {
			scope.__playCalls?.push(Date.now());
			return Promise.reject(new DOMException('play() rejected for builder bank', 'NotAllowedError'));
		};
	});
}

async function stubResolvePlay(page: Page) {
	await page.addInitScript(() => {
		const scope = window as unknown as { __playCalls?: number[] };
		scope.__playCalls = [];
		HTMLMediaElement.prototype.play = function () {
			scope.__playCalls?.push(Date.now());
			return Promise.resolve();
		};
	});
}

async function stubDeferredPlay(page: Page) {
	await page.addInitScript(() => {
		const scope = window as unknown as {
			__playCalls?: number[];
			__playResolve?: (() => void) | null;
		};
		scope.__playCalls = [];
		scope.__playResolve = null;
		HTMLMediaElement.prototype.play = function () {
			scope.__playCalls?.push(Date.now());
			return new Promise<void>((resolve) => {
				scope.__playResolve = () => resolve();
			});
		};
	});
}

async function resolveDeferredPlay(page: Page): Promise<void> {
	await page.evaluate(() => {
		const scope = window as unknown as { __playResolve?: (() => void) | null };
		const resolve = scope.__playResolve;
		if (!resolve) throw new Error('no deferred play() is pending');
		scope.__playResolve = null;
		resolve();
	});
}

async function playCalls(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __playCalls?: unknown[] }).__playCalls?.length ?? -1
	);
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const request = route.request();
		if (request.resourceType() === 'media') return route.abort('blockedbyclient');
		const url = new URL(request.url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		return route.continue();
	});
});

test('selects the first appended track when the first displayed track is already queued', async ({ page }) => {
	await mockLibrary(page);
	await stubRejectPlay(page);
	await openLibrary(page, ITEMS.length);
	await rowAction(page, 'shown-zulu').click();
	await expectQueue(page, ['shown-zulu'], 'shown-zulu');
	await action(page).click();
	// Queue order extended in displayed order; current lands on the first
	// appended track (Alpha at the old queue length), not on the already
	// queued first displayed track (Zulu at slot 0).
	await expectQueue(
		page,
		['shown-zulu', 'shown-alpha', 'shown-untitled', 'shown-middle', 'shown-echo'],
		'shown-alpha'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	const region = announcement(page);
	await expect(region).toContainText('Alpha Song');
	await expect(region).toContainText(/\b4\b/);
	await expect(region).toContainText(/1 already queued/);
	expect(await playCalls(page)).toBe(0);
});

test('resolving play() announces the exact Playing outcome with counts', async ({ page }) => {
	await mockLibrary(page, STREAM_ITEMS);
	await stubResolvePlay(page);
	await openLibrary(page, STREAM_ITEMS.length);
	await action(page).click();
	await expectQueue(page, ['shown-zulu', 'shown-alpha', 'shown-middle'], 'shown-zulu');
	await expect.poll(() => playCalls(page)).toBe(1);
	const player = page.locator('audio[data-testid="audio-player"]');
	await expect(player).toHaveCount(1);
	await expect(player).toHaveAttribute('src', /__playall-build__\/zulu\.mp3/);
	const region = announcement(page);
	await expect(region).toContainText('Playing Zulu Song.');
	await expect(region).toContainText(/\b3\b/);
	await page.waitForTimeout(500);
	expect(await playCalls(page), 'no retry outside the gesture').toBe(1);
});

test('rejected play() names paused/blocked and never claims Playing', async ({ page }) => {
	await mockLibrary(page, STREAM_ITEMS);
	await stubRejectPlay(page);
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(String(error)));
	await openLibrary(page, STREAM_ITEMS.length);
	await action(page).click();
	await expectQueue(page, ['shown-zulu', 'shown-alpha', 'shown-middle'], 'shown-zulu');
	await expect.poll(() => playCalls(page)).toBe(1);
	const region = announcement(page);
	await expect(region).toContainText('Zulu Song');
	await expect(region).toContainText('paused: playback was blocked');
	await expect(region).toContainText(/\b3\b/);
	await expect(region, 'truthful feedback never claims playback started').not.toContainText(/Playing /);
	await page.waitForTimeout(500);
	expect(await playCalls(page), 'no retry outside the gesture').toBe(1);
	expect(pageErrors, 'rejected play() stays handled').toEqual([]);
});

type QueueEdit = { name: string; click: (page: Page) => Promise<void>; outcome: RegExp };

const DIRECT_EDITS: QueueEdit[] = [
	{
		name: 'remove',
		click: (page) => page.getByRole('button', { name: 'Remove Middle Song from queue', exact: true }).click(),
		outcome: /Middle Song removed from the queue\./
	},
	{
		name: 'move',
		click: (page) => page.getByRole('button', { name: 'Move Middle Song up', exact: true }).click(),
		outcome: /Middle Song moved to 2 of 3/
	},
	{
		name: 'select',
		click: (page) => page.getByRole('button', { name: 'Select Middle Song', exact: true }).click(),
		outcome: /Selected Middle Song/
	},
	{
		name: 'clear-upcoming',
		click: (page) => page.getByRole('button', { name: 'Clear upcoming', exact: true }).click(),
		outcome: /Removed 2 upcoming tracks\./
	},
	{
		name: 'clear',
		click: (page) => page.getByRole('button', { name: 'Clear queue', exact: true }).click(),
		outcome: /Queue cleared\./
	}
];

for (const edit of DIRECT_EDITS) {
	test(`deferred play() outcome is fenced by queue ${edit.name}`, async ({ page }) => {
		await mockLibrary(page, STREAM_ITEMS);
		await stubDeferredPlay(page);
		await openLibrary(page, STREAM_ITEMS.length);
		await action(page).click();
		await expect.poll(() => playCalls(page)).toBe(1);
		await edit.click(page);
		// The edit's own announcement proves its epoch bump landed before
		// the late play() outcome resolves.
		await expect(announcement(page)).toContainText(edit.outcome);
		await resolveDeferredPlay(page);
		await page.waitForTimeout(400);
		await expect(announcement(page)).toContainText(edit.outcome);
		await expect(announcement(page)).not.toContainText(/Playing /);
		expect(await playCalls(page)).toBe(1);
	});
}

test('deferred play() outcome is fenced by trimming previous tracks', async ({ page }) => {
	await mockLibrary(page, STREAM_ITEMS);
	await stubDeferredPlay(page);
	await openLibrary(page, STREAM_ITEMS.length);
	await rowAction(page, 'shown-zulu').click();
	await expectQueue(page, ['shown-zulu'], 'shown-zulu');
	await action(page).click();
	await expectQueue(page, ['shown-zulu', 'shown-alpha', 'shown-middle'], 'shown-alpha');
	await expect.poll(() => playCalls(page)).toBe(1);
	await page.getByRole('button', { name: 'Keep current and upcoming', exact: true }).click();
	await expect(announcement(page)).toContainText(/Removed 1 track before the current track\./);
	await resolveDeferredPlay(page);
	await page.waitForTimeout(400);
	await expect(announcement(page)).toContainText(/Removed 1 track before the current track\./);
	await expect(announcement(page)).not.toContainText(/Playing /);
	expect(await playCalls(page)).toBe(1);
});

test('deferred play() outcome is fenced by an auth transition', async ({ page }) => {
	const reply: { items: unknown } = { items: STREAM_ITEMS };
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(reply.items) })
	);
	await stubDeferredPlay(page);
	await openLibrary(page, STREAM_ITEMS.length);
	await action(page).click();
	await expect.poll(() => playCalls(page)).toBe(1);
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'play-displayed-builder-boundary');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await resolveDeferredPlay(page);
	await page.waitForTimeout(400);
	expect(await playCalls(page)).toBe(1);
	// The next account remounts the panel with no stale outcome: the Retry
	// refresh never touches the announcement while the queue is empty, so a
	// leaked late resolve would still be visible here.
	reply.items = [{ id: 'shown-other', title: 'Other Account' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(announcement(page)).toHaveText('');
	await action(page).click();
	await expectQueue(page, ['shown-other'], 'shown-other');
	await expect(announcement(page)).toContainText('Other Account');
});

test('no-op Next while play() is pending leaves the outcome alone', async ({ page }) => {
	await mockLibrary(page, SOLO_ITEM);
	await stubDeferredPlay(page);
	await openLibrary(page, SOLO_ITEM.length);
	await action(page).click();
	await expectQueue(page, ['solo-track'], 'solo-track');
	await expect.poll(() => playCalls(page)).toBe(1);
	// The single track is already last, so Next is a no-op replacement:
	// the selection the pending outcome describes is unchanged.
	await page.getByTestId('player-next').click();
	await resolveDeferredPlay(page);
	await expect(announcement(page)).toContainText('Playing Solo Song.');
	await expect(announcement(page)).toContainText(/\b1\b/);
	await page.waitForTimeout(400);
	expect(await playCalls(page)).toBe(1);
});

test('real Next while play() is pending fences the stale outcome', async ({ page }) => {
	await mockLibrary(page, STREAM_ITEMS);
	await stubDeferredPlay(page);
	await openLibrary(page, STREAM_ITEMS.length);
	await action(page).click();
	await expectQueue(page, ['shown-zulu', 'shown-alpha', 'shown-middle'], 'shown-zulu');
	await expect.poll(() => playCalls(page)).toBe(1);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expect(announcement(page)).toHaveText('');
	await resolveDeferredPlay(page);
	await page.waitForTimeout(400);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expect(announcement(page)).toHaveText('');
	expect(await playCalls(page)).toBe(1);
});

test('bulk append of stream-bearing items stays metadata-only without mounting audio', async ({ page }) => {
	await mockLibrary(page, STREAM_ITEMS);
	await stubRejectPlay(page);
	await openLibrary(page, STREAM_ITEMS.length);
	await expect(bulkAction(page)).toBeEnabled();
	await bulkAction(page).click();
	await expectQueue(page, ['shown-zulu', 'shown-alpha', 'shown-middle'], 'shown-zulu');
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(announcement(page)).toHaveText('Added 3 tracks to the queue.');
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await playCalls(page)).toBe(0);
});
