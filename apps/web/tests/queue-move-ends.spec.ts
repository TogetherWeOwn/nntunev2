import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * QTOP acceptance-owned contract; Builder must not edit this file.
 * Test bank ref: test/queue-move-ends-acceptance.
 *
 * Companion: src/lib/player/queue.moveEnds.acceptance.test.ts (QTOP-U-*)
 * pins the pure helpers; src/lib/player/playerQueue.moveEnds.acceptance.test.ts
 * (QTOP-P-*) pins the PlayerQueue markup. QTOP-B-* cases below drive the REAL
 * /library route (PlayerQueue) and the REAL /demo/player route (inline queue)
 * with local fixtures only.
 *
 * Locked contract (so the builder never guesses):
 * - Row buttons are named `Move {title} to top` / `Move {title} to bottom`
 *   (exact accessible names), on library rows and demo inline rows alike.
 * - Moves reuse the shared queue.ts helpers: the current *track* stays
 *   current (verified via now-playing text, queue-position and aria-current).
 * - Moves announce `{title} moved to {position} of {count}` through the
 *   `queue-announcement` live region; the demo inline queue gains the same
 *   hook. No-op end buttons never announce.
 * - Unreachable-end buttons (top on the first row, bottom on the last) are
 *   disabled or hidden; forcing a click on a present one changes nothing.
 *
 * CASE INDEX (stable IDs): QTOP-B-01 library last-to-first, QTOP-B-02
 * library first(current)-to-last, QTOP-B-03 library middle move preserves
 * another current, QTOP-B-04 library end buttons inert and silent,
 * QTOP-B-05 demo last-to-first, QTOP-B-06 demo first(current)-to-last,
 * QTOP-B-07 demo end buttons inert and silent, QTOP-B-08 narrow library.
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   CI=1 pnpm test:e2e tests/queue-move-ends.spec.ts --workers=1
 * Ensure preview port 4173 is free; never reuse another worktree's preview.
 * Expected pre-feature RED: assertion failures for the missing top/bottom
 * buttons and demo announcement hook, not harness/import errors. Passing
 * disabled-or-absent end checks are negative controls, not feature evidence.
 * The Builder consumes these tests in one combined ready PR; never merge
 * the RED test bank by itself.
 */

const LIBRARY = [
	{ id: 'qtop-lib-a', title: 'Alpha Song', artist: 'Move Ends Band' },
	{ id: 'qtop-lib-b', title: 'Bravo Song', artist: 'Move Ends Band' },
	{ id: 'qtop-lib-c', title: 'Charlie Song', artist: 'Move Ends Band' },
	{ id: 'qtop-lib-d', title: 'Delta Song', artist: 'Move Ends Band' }
];

const DEMO_TITLE: Record<string, string> = {
	'demo-track': 'Demo track',
	'second-signal': 'Second Signal',
	'third-light': 'Third Light'
};

interface Traffic {
	apiCalls: string[];
	foreign: string[];
}

/** Same-origin only; the library API is fulfilled locally, demo media held pending. */
async function localOnly(page: Page, origin: string, library: unknown, traffic: Traffic) {
	const url = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(url.hostname);
	await page.route('**/*', async (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== url.origin) {
			traffic.foreign.push(request.url());
			return route.abort();
		}
		if (target.pathname.startsWith('/api/')) {
			traffic.apiCalls.push(`${request.method()} ${target.pathname}`);
			if (target.pathname === '/api/library' && request.method() === 'GET') {
				return route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify(library)
				});
			}
			// Hold demo media pending (the radio-spec pattern): fulfilling
			// garbage bytes would fire a native error event on the real element.
			if (target.pathname.startsWith('/api/demo/audio')) return;
			return route.continue();
		}
		return route.continue();
	});
}

function queueIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}

function currentRow(page: Page): Locator {
	return page.locator('[data-testid="queue-item"][aria-current="true"]');
}

async function announcementText(page: Page): Promise<string> {
	return ((await page.getByTestId('queue-announcement').textContent()) ?? '').trim();
}

function topButton(page: Page, title: string): Locator {
	return page.getByRole('button', { name: `Move ${title} to top`, exact: true });
}

function bottomButton(page: Page, title: string): Locator {
	return page.getByRole('button', { name: `Move ${title} to bottom`, exact: true });
}

/**
 * Disabled-or-hidden end button: hidden passes immediately, a present one
 * must be disabled and a forced click must change nothing and stay silent.
 */
async function expectEndInert(page: Page, button: Locator): Promise<void> {
	if ((await button.count()) === 0) return;
	await expect(button).toBeDisabled();
	const before = await queueIds(page);
	await button.click({ force: true });
	expect(await queueIds(page)).toEqual(before);
	expect(await announcementText(page)).toBe('');
}

async function openLibrary(page: Page, baseURL: string, traffic: Traffic): Promise<void> {
	await page.addInitScript(() => {
		sessionStorage.clear();
		localStorage.clear();
	});
	await localOnly(page, baseURL, LIBRARY, traffic);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(LIBRARY.length);
}

async function queueLibraryTracks(page: Page, ids: string[]): Promise<void> {
	for (const id of ids) {
		await page.locator(`[data-testid="library-item"][data-id="${id}"]`).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(ids.length);
}

async function openDemo(page: Page, baseURL: string, traffic: Traffic): Promise<void> {
	await page.addInitScript(() => {
		sessionStorage.clear();
		localStorage.clear();
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await localOnly(page, baseURL, [], traffic);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// The demo inline queue gains the shared announcement hook (attached,
	// not necessarily visible while empty).
	await expect(page.getByTestId('queue-announcement')).toBeAttached();
}

function expectLibraryTraffic(traffic: Traffic): void {
	expect(traffic.foreign).toEqual([]);
	expect(traffic.apiCalls.length).toBeGreaterThan(0);
	expect(traffic.apiCalls.every((call) => call === 'GET /api/library')).toBe(true);
}

function expectDemoTraffic(traffic: Traffic): void {
	expect(traffic.foreign).toEqual([]);
	expect(
		traffic.apiCalls.every(
			(call) => call.startsWith('GET /api/demo/audio') || call.startsWith('GET /api/demo/')
		)
	).toBe(true);
}

test.describe('mocked library queue', () => {
	test('QTOP-B-01 moves the last track to the top; current stays current and the move announces', async ({
		page,
		baseURL
	}) => {
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openLibrary(page, baseURL!, traffic);
		await queueLibraryTracks(page, ['qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c', 'qtop-lib-d']);
		expect(await announcementText(page)).toBe('');

		const move = topButton(page, 'Delta Song');
		await expect(move).toBeVisible();
		await expect(move).toBeEnabled();
		await move.click();

		expect(await queueIds(page)).toEqual(['qtop-lib-d', 'qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c']);
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'qtop-lib-a');
		await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
		await expect(page.getByTestId('queue-position')).toContainText('Track 2 of 4');
		await expect(page.getByTestId('queue-announcement')).toHaveText('Delta Song moved to 1 of 4');
		expectLibraryTraffic(traffic);
	});

	test('QTOP-B-02 moves the first (current) track to the bottom; identity follows and the move announces', async ({
		page,
		baseURL
	}) => {
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openLibrary(page, baseURL!, traffic);
		await queueLibraryTracks(page, ['qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c', 'qtop-lib-d']);

		const move = bottomButton(page, 'Alpha Song');
		await expect(move).toBeVisible();
		await expect(move).toBeEnabled();
		await move.click();

		expect(await queueIds(page)).toEqual(['qtop-lib-b', 'qtop-lib-c', 'qtop-lib-d', 'qtop-lib-a']);
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'qtop-lib-a');
		await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
		await expect(page.getByTestId('queue-position')).toContainText('Track 4 of 4');
		await expect(page.getByTestId('queue-announcement')).toHaveText('Alpha Song moved to 4 of 4');
		expectLibraryTraffic(traffic);
	});

	test('QTOP-B-03 moves a middle track to the top while another track stays current', async ({
		page,
		baseURL
	}) => {
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openLibrary(page, baseURL!, traffic);
		await queueLibraryTracks(page, ['qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c', 'qtop-lib-d']);
		await page.getByTestId('player-next').click();
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'qtop-lib-b');

		const move = topButton(page, 'Delta Song');
		await expect(move).toBeVisible();
		await move.click();

		expect(await queueIds(page)).toEqual(['qtop-lib-d', 'qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c']);
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'qtop-lib-b');
		await expect(page.getByTestId('now-playing')).toContainText('Bravo Song');
		await expect(page.getByTestId('queue-position')).toContainText('Track 3 of 4');
		await expect(page.getByTestId('queue-announcement')).toHaveText('Delta Song moved to 1 of 4');
		expectLibraryTraffic(traffic);
	});

	test('QTOP-B-04 unreachable-end buttons are inert and silent; middle rows offer both actions', async ({
		page,
		baseURL
	}) => {
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openLibrary(page, baseURL!, traffic);
		await queueLibraryTracks(page, ['qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c']);
		expect(await announcementText(page)).toBe('');

		await expectEndInert(page, topButton(page, 'Alpha Song'));
		await expectEndInert(page, bottomButton(page, 'Charlie Song'));
		expect(await queueIds(page)).toEqual(['qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c']);

		const middleTop = topButton(page, 'Bravo Song');
		const middleBottom = bottomButton(page, 'Bravo Song');
		await expect(middleTop).toBeVisible();
		await expect(middleTop).toBeEnabled();
		await expect(middleBottom).toBeVisible();
		await expect(middleBottom).toBeEnabled();
		expectLibraryTraffic(traffic);
	});

	test('QTOP-B-08 the end actions work at a narrow 360px viewport', async ({
		page,
		baseURL
	}) => {
		await page.setViewportSize({ width: 360, height: 800 });
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openLibrary(page, baseURL!, traffic);
		await queueLibraryTracks(page, ['qtop-lib-a', 'qtop-lib-b', 'qtop-lib-c']);

		const move = topButton(page, 'Charlie Song');
		await expect(move).toBeVisible();
		await expect(move).toBeEnabled();
		await move.click();

		expect(await queueIds(page)).toEqual(['qtop-lib-c', 'qtop-lib-a', 'qtop-lib-b']);
		await expect(page.getByTestId('queue-announcement')).toHaveText('Charlie Song moved to 1 of 3');
		expectLibraryTraffic(traffic);
	});
});

test.describe('demo inline queue', () => {
	test('QTOP-B-05 moves the last demo track to the top; current stays current and the move announces', async ({
		page,
		baseURL
	}) => {
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openDemo(page, baseURL!, traffic);
		expect(await announcementText(page)).toBe('');

		const move = topButton(page, DEMO_TITLE['third-light']!);
		await expect(move).toBeVisible();
		await expect(move).toBeEnabled();
		await move.click();

		expect(await queueIds(page)).toEqual(['third-light', 'demo-track', 'second-signal']);
		await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute(
			'aria-current',
			'true'
		);
		await expect(page.getByTestId('queue-announcement')).toHaveText('Third Light moved to 1 of 3');
		expectDemoTraffic(traffic);
	});

	test('QTOP-B-06 moves the first (current) demo track to the bottom; identity follows and the move announces', async ({
		page,
		baseURL
	}) => {
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openDemo(page, baseURL!, traffic);

		const move = bottomButton(page, DEMO_TITLE['demo-track']!);
		await expect(move).toBeVisible();
		await expect(move).toBeEnabled();
		await move.click();

		expect(await queueIds(page)).toEqual(['second-signal', 'third-light', 'demo-track']);
		await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute(
			'aria-current',
			'true'
		);
		await expect(page.getByTestId('queue-announcement')).toHaveText('Demo track moved to 3 of 3');
		expectDemoTraffic(traffic);
	});

	test('QTOP-B-07 demo unreachable-end buttons are inert and silent; the middle row offers both actions', async ({
		page,
		baseURL
	}) => {
		const traffic: Traffic = { apiCalls: [], foreign: [] };
		await openDemo(page, baseURL!, traffic);
		expect(await announcementText(page)).toBe('');

		await expectEndInert(page, topButton(page, DEMO_TITLE['demo-track']!));
		await expectEndInert(page, bottomButton(page, DEMO_TITLE['third-light']!));
		expect(await queueIds(page)).toEqual(['demo-track', 'second-signal', 'third-light']);

		const middleTop = topButton(page, DEMO_TITLE['second-signal']!);
		const middleBottom = bottomButton(page, DEMO_TITLE['second-signal']!);
		await expect(middleTop).toBeVisible();
		await expect(middleTop).toBeEnabled();
		await expect(middleBottom).toBeVisible();
		await expect(middleBottom).toBeEnabled();
		expectDemoTraffic(traffic);
	});
});
