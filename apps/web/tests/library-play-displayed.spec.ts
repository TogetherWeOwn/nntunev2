import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * PLAYALL acceptance bank — protected; the paired build owns the combined
 * GREEN ready PR. Do not change this file to accommodate an implementation,
 * or open a standalone ready PR containing the deliberate pre-feature
 * failures.
 *
 * From repo root (existing package scripts and mocked-browser harness):
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test
 *   CI=1 pnpm --dir apps/web exec playwright test tests/library-play-displayed.spec.ts --workers=1
 * Omit an extra `--` with this pnpm version: otherwise the runners ignore the
 * focus/options and run the whole suite. CI=1 prevents reuse of another
 * worktree's preview server on the shared port. Linux browsers may need the
 * host's installed Chromium libraries on LD_LIBRARY_PATH; do not replace the
 * browser. Regressions: same test:e2e script with
 * tests/library-bulk-queue.spec.ts, tests/library-favorites-queue-all.spec.ts
 * and tests/library-subset-append.spec.ts.
 *
 * Real /library, real queue and real ordering; only /api/library is mocked
 * and media requests are refused. No backend/provider/production requests.
 * The one new action keeps the exact accessible name `Play displayed from
 * the top` in every state (the bulk precedent); state travels through
 * disabled plus an accessible description, never a renamed button. Unlike
 * bulk it stays enabled while everything is queued, because selecting the
 * first displayed track's queue position stays useful. The play() rejection
 * case supplies streamUrl-bearing items: the build carries item.streamUrl
 * into the new path (the row-selection precedent), so audio mounts and the
 * gesture attempts play() exactly once. Case IDs form the handoff; every
 * case emits a PLAYALL-RECEIPT JSON line plus an acceptance-case.json
 * attachment with expected/actual/status/error_code/next_action.
 *
 * RED verified <pending hosted dispatch> against <pending SHA>:
 * PLAYALL-00 passes; PLAYALL-01–08 fail on the absent-action assertion
 * after successful browser/app setup. Narrow/keyboard play SUCCESS evidence
 * remains the build's GREEN obligation; these pre-feature failures are not
 * a claim the missing control is usable.
 *
 * Breaks caught: queue built from raw API/favorites instead of displayed
 * order; removed or reordered prefix/current; inaccurate added/skipped
 * counts; all-queued reorder instead of in-place select; invented tracks
 * from an empty filter; unhandled play() rejection or out-of-gesture
 * retry; false now-playing claim while paused; favorites/subset/find/
 * order/view/share mutation; storage writes; network or media effects;
 * auth leakage; inaccessible or overflowing narrow-screen action.
 * Pre-feature expected RED: action/disabled-state assertions, never setup
 * errors.
 */

const LABEL = 'Play displayed from the top';
const BULK_LABEL = 'Add displayed tracks to queue';
const ACTION_MARKER = '[playall-action-missing]';
const RECEIPT_PREFIX = 'PLAYALL-RECEIPT';
const ITEMS = [
	{ id: 'shown-zulu', title: 'Zulu Song', artist: 'Bravo Band' },
	{ id: 'shown-alpha', title: 'Alpha Song', artist: 'Zulu Band' },
	{ id: 'shown-untitled', title: '   ', artist: '   ' },
	{ id: 'shown-middle', title: 'Middle Song', artist: 'Alpha Band' },
	{ id: 'shown-echo', title: 'Echo Song', artist: 'Echo Band' }
];
const ORIGINAL = ['shown-zulu', 'shown-alpha', 'shown-untitled', 'shown-middle', 'shown-echo'];
const TITLE = ['shown-alpha', 'shown-echo', 'shown-middle', 'shown-untitled', 'shown-zulu'];
const STREAM_ITEMS = [
	{ id: 'shown-zulu', title: 'Zulu Song', artist: 'Bravo Band', streamUrl: '/__playall__/zulu.mp3' },
	{ id: 'shown-alpha', title: 'Alpha Song', artist: 'Zulu Band', streamUrl: '/__playall__/alpha.mp3' },
	{
		id: 'shown-middle',
		title: 'Middle Song',
		artist: 'Alpha Band',
		streamUrl: '/__playall__/middle.mp3'
	}
];

type Reply = { items: unknown; status?: number };

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

function queued(page: Page, id: string): Locator {
	return page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
}

function announcement(page: Page): Locator {
	return page.getByTestId('queue-announcement');
}

async function mockLibrary(page: Page, reply: Reply = { items: ITEMS }) {
	let reads = 0;
	const mutations: string[] = [];
	const media: string[] = [];
	page.on('request', (request) => {
		if (!['GET', 'HEAD', 'OPTIONS'].includes(request.method())) mutations.push(`${request.method()} ${new URL(request.url()).pathname}`);
		if (request.resourceType() === 'media') media.push(request.url());
	});
	await page.route('**/api/library', (route) => {
		reads += 1;
		return route.fulfill({
			status: reply.status ?? 200, contentType: 'application/json', body: JSON.stringify(reply.items)
		});
	});
	return { reads: () => reads, mutations, media };
}

async function openLibrary(page: Page, count = ITEMS.length) {
	await page.goto('/library');
	if (count > 0) await expect(page.getByTestId('library-item')).toHaveCount(count);
	else await expect(page.getByRole('status').filter({ hasText: /your library is empty/i })).toBeVisible();
}

async function expectView(page: Page, ids: string[]) {
	await expect.poll(() => page.getByTestId('library-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-id'))
	)).toEqual(ids);
}

async function sort(page: Page, label: 'Title A–Z' | 'Artist A–Z' | 'Original order') {
	await page.getByRole('combobox', { name: /order|sort/i }).selectOption({ label });
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

async function expectOutcome(
	page: Page,
	expected: { title: string; added: number; skipped?: number; verb?: RegExp }
) {
	const region = announcement(page);
	await expect(region).toHaveCount(1);
	await expect(region).toContainText(expected.verb ?? /play/i);
	await expect(region).toContainText(expected.title);
	await expect(region).toContainText(new RegExp(`\\b${expected.added}\\b`));
	if (expected.skipped !== undefined) {
		await expect(region).toContainText(new RegExp(`\\b${expected.skipped}\\b`));
		await expect(region).toContainText(/already queued|skipped/i);
	}
	const text = (await region.innerText()).trim();
	expect(text.length, 'one concise announcement naming outcome and counts').toBeLessThanOrEqual(200);
}

async function stubPlay(page: Page) {
	await page.addInitScript(() => {
		const scope = window as unknown as { __playCalls?: number[] };
		scope.__playCalls = [];
		HTMLMediaElement.prototype.play = function () {
			scope.__playCalls?.push(Date.now());
			return Promise.reject(new DOMException('play() rejected for acceptance', 'NotAllowedError'));
		};
	});
}

async function playCalls(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __playCalls?: unknown[] }).__playCalls?.length ?? -1
	);
}

async function storage(page: Page) {
	return page.evaluate(() => ({
		local: Object.fromEntries(Object.entries(localStorage).sort()),
		session: Object.fromEntries(Object.entries(sessionStorage).sort())
	}));
}

test.beforeEach(async ({ context }) => {
	// Per-page library mocks take precedence. Everything else stays local,
	// media is refused, and any unmocked API call is refused rather than
	// touching a backend.
	await context.route('**/*', (route) => {
		const request = route.request();
		if (request.resourceType() === 'media') return route.abort('blockedbyclient');
		const url = new URL(request.url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		return route.continue();
	});
});

test.afterEach(async ({}, testInfo) => {
	try {
		const passed = testInfo.status === 'passed';
		const skipped = testInfo.status === 'skipped';
		const actionMissing =
			!passed &&
			!skipped &&
			testInfo.errors.some((error) => (error.message ?? '').includes(ACTION_MARKER));
		const receipt = {
			case_id: testInfo.title.split(' ')[0],
			expected: passed
				? 'all behavioral assertions hold'
				: actionMissing
					? `${LABEL} visible and enabled`
					: 'reach the behavioral assertion',
			actual: passed
				? 'pass'
				: testInfo.errors.map((error) => (error.message ?? '').split('\n')[0].slice(0, 280)),
			status: passed ? 'GREEN' : skipped ? 'SKIPPED' : actionMissing ? 'RED' : 'ERROR',
			error_code:
				passed || skipped ? null : actionMissing ? 'PLAYALL_ACTION_MISSING' : 'HARNESS_OR_ACTION_FAILURE',
			next_action: passed
				? 'preserve acceptance'
				: actionMissing
					? 'implement the play-from-top action; do not edit protected tests'
					: 'repair harness or action; not verified feature RED'
		};
		const body = JSON.stringify(receipt);
		console.log(`${RECEIPT_PREFIX} ${body}`);
		await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
	} catch {
		// Receipts never break cases.
	}
});

test('PLAYALL-00 control: bulk appends in displayed order without reselecting', async ({ page }) => {
	const api = await mockLibrary(page);
	await stubPlay(page);
	await openLibrary(page);
	await expectView(page, ORIGINAL);
	await rowAction(page, 'shown-zulu').click();
	await rowAction(page, 'shown-alpha').click();
	await expectQueue(page, ['shown-zulu', 'shown-alpha'], 'shown-zulu');
	await sort(page, 'Title A–Z');
	await expectView(page, TITLE);
	await expect(bulkAction(page)).toBeEnabled();
	await bulkAction(page).click();
	// Append-only premise: missing IDs join in displayed order; current stays.
	await expectQueue(
		page,
		['shown-zulu', 'shown-alpha', 'shown-echo', 'shown-middle', 'shown-untitled'],
		'shown-zulu'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await playCalls(page)).toBe(0);
	expect(api.reads()).toBe(1);
});

test('PLAYALL-01 empty queue loads displayed order, selects first and names outcome with counts', async ({ page }) => {
	const api = await mockLibrary(page);
	await stubPlay(page);
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(String(error)));
	await openLibrary(page);
	await sort(page, 'Title A–Z');
	await expectView(page, TITLE);
	const before = await storage(page);
	const control = action(page);
	await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
	await expect(control).toBeEnabled();
	const buttonBox = await control.boundingBox();
	const listBox = await page.getByTestId('library-list').boundingBox();
	expect(buttonBox!.y + buttonBox!.height).toBeLessThanOrEqual(listBox!.y);
	await control.click();
	await expectQueue(page, TITLE, 'shown-alpha');
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expectOutcome(page, { title: 'Alpha Song', added: 5 });
	await expect(queued(page, 'shown-untitled')).toContainText('shown-untitled');
	await expect(queued(page, 'shown-untitled')).not.toContainText(/undefined|null/i);
	await expect(page.getByRole('combobox', { name: /order|sort/i })).toHaveValue('title');
	await expect(page.getByLabel('Find in loaded tracks')).toHaveValue('');
	await expect(page.getByTestId('library-share-status')).toBeEmpty();
	expect(new URL(page.url()).search).toBe('');
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await playCalls(page)).toBe(0);
	expect(pageErrors).toEqual([]);
	expect(await storage(page), 'play-from-top is in-memory only').toEqual(before);
	expect(api.mutations).toEqual([]);
	expect(api.media).toEqual([]);
	expect(api.reads()).toBe(1);
});

test('PLAYALL-02 non-empty queue appends missing through the append core and selects first appended', async ({ page }) => {
	const api = await mockLibrary(page);
	await stubPlay(page);
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(String(error)));
	await openLibrary(page);
	await row(page, 'shown-zulu').getByRole('button', { name: 'Star Zulu Song', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await row(page, 'shown-echo').getByRole('checkbox').check();
	await expect(page.getByTestId('selection-count')).toContainText('1 selected');
	await sort(page, 'Title A–Z');
	await expectView(page, TITLE);
	await rowAction(page, 'shown-middle').click();
	await rowAction(page, 'shown-echo').click();
	await expectQueue(page, ['shown-middle', 'shown-echo'], 'shown-middle');
	const before = await storage(page);
	const control = action(page);
	await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
	await expect(control).toBeEnabled();
	await control.click();
	await expectQueue(
		page,
		['shown-middle', 'shown-echo', 'shown-alpha', 'shown-untitled', 'shown-zulu'],
		'shown-alpha'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expectOutcome(page, { title: 'Alpha Song', added: 3, skipped: 2 });
	// No removal or reorder of existing rows; no adjacent-state mutation.
	await expect(
		row(page, 'shown-zulu').getByRole('button', { name: 'Unstar Zulu Song', exact: true })
	).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(row(page, 'shown-echo').getByRole('checkbox')).toBeChecked();
	await expect(page.getByTestId('selection-count')).toContainText('1 selected');
	await expect(page.getByTestId('append-result')).toHaveCount(0);
	await expect(page.getByRole('combobox', { name: /order|sort/i })).toHaveValue('title');
	await expect(page.getByLabel('Find in loaded tracks')).toHaveValue('');
	await expect(page.getByLabel('Show')).toHaveValue('all');
	await expect(page.getByTestId('library-share-status')).toBeEmpty();
	expect(new URL(page.url()).search).toBe('');
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await playCalls(page)).toBe(0);
	expect(pageErrors).toEqual([]);
	expect(await storage(page)).toEqual(before);
	expect(api.mutations).toEqual([]);
	expect(api.media).toEqual([]);
	expect(api.reads()).toBe(1);
});

test('PLAYALL-03 all-already-queued selects the first displayed track in place without reorder', async ({ page }) => {
	await mockLibrary(page);
	await stubPlay(page);
	await openLibrary(page);
	await expect(bulkAction(page)).toBeEnabled();
	await bulkAction(page).click();
	await expectQueue(page, ORIGINAL, 'shown-zulu');
	await sort(page, 'Title A–Z');
	await expectView(page, TITLE);
	const control = action(page);
	await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
	await expect(control).toBeEnabled();
	await control.click();
	// Queue order untouched; current jumps to the first displayed track's slot.
	await expectQueue(page, ORIGINAL, 'shown-alpha');
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expectOutcome(page, { title: 'Alpha Song', added: 5, verb: /already queued/i });
	await expect(announcement(page)).not.toContainText(/added\s+[1-9]\d*/i);
	// Selecting stays useful, so the control stays enabled (unlike bulk).
	await expect(control).toBeEnabled();
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await playCalls(page)).toBe(0);
});

test('PLAYALL-04 empty displayed set disables honestly and never invents tracks', async ({ page }) => {
	const api = await mockLibrary(page);
	await stubPlay(page);
	await openLibrary(page);
	await page.getByLabel('Find in loaded tracks').fill('zzz-no-such-track');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByText('No matches in your loaded tracks.')).toBeVisible();
	const control = action(page);
	await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
	await expect(control).toBeDisabled();
	await expect(control).toHaveAccessibleDescription(/track|play|display|empty|match|find|nothing|no /i);
	await control.evaluate((button: HTMLButtonElement) => button.click());
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(announcement(page)).toBeEmpty();
	await expect(page.getByTestId('library-share-status')).toBeEmpty();
	expect(api.reads()).toBe(1);
	expect(api.mutations).toEqual([]);
	// The filter text survives; clearing restores every row.
	await expect(page.getByLabel('Find in loaded tracks')).toHaveValue('zzz-no-such-track');
	await page.getByRole('button', { name: 'Clear search', exact: true }).first().click();
	await expectView(page, ORIGINAL);
});

test('PLAYALL-05 play() rejection leaves the first track selected-but-paused with truthful feedback', async ({ page }) => {
	const api = await mockLibrary(page, { items: STREAM_ITEMS });
	await stubPlay(page);
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(String(error)));
	await openLibrary(page, STREAM_ITEMS.length);
	const control = action(page);
	await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
	await expect(control).toBeEnabled();
	await control.click();
	await expectQueue(page, ['shown-zulu', 'shown-alpha', 'shown-middle'], 'shown-zulu');
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	// Exactly one gesture-driven play() attempt; no out-of-gesture retry.
	await expect.poll(() => playCalls(page)).toBe(1);
	const player = page.locator('audio[data-testid="audio-player"]');
	await expect(player).toHaveCount(1);
	await expect(player).toHaveAttribute('src', /__playall__\/zulu\.mp3/);
	expect(await player.evaluate((audio: HTMLAudioElement) => audio.paused), 'rejection leaves audio paused').toBe(true);
	await page.waitForTimeout(500);
	expect(await playCalls(page), 'no retry outside the gesture').toBe(1);
	const region = announcement(page);
	await expect(region).toContainText('Zulu Song');
	await expect(region).toContainText(/\b3\b/);
	await expect(region, 'truthful feedback never claims playback started').not.toContainText(/now playing/i);
	const text = (await region.innerText()).trim();
	expect(text.length).toBeLessThanOrEqual(200);
	expect(pageErrors, 'rejected play() stays handled').toEqual([]);
	expect(api.mutations).toEqual([]);
	expect(api.reads()).toBe(1);
});

test('PLAYALL-06 favorites-only view plays the displayed projection and leaves favorites intact', async ({ page }) => {
	await mockLibrary(page);
	await stubPlay(page);
	await openLibrary(page);
	await row(page, 'shown-middle').getByRole('button', { name: 'Star Middle Song', exact: true }).click();
	await row(page, 'shown-alpha').getByRole('button', { name: 'Star Alpha Song', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await page.getByLabel('Show').selectOption('favorites');
	await expectView(page, ['shown-alpha', 'shown-middle']);
	const control = action(page);
	await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
	await expect(control).toBeEnabled();
	await control.click();
	await expectQueue(page, ['shown-alpha', 'shown-middle'], 'shown-alpha');
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expectOutcome(page, { title: 'Alpha Song', added: 2 });
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect(
		row(page, 'shown-alpha').getByRole('button', { name: 'Unstar Alpha Song', exact: true })
	).toHaveAttribute('aria-pressed', 'true');
	await expect(
		row(page, 'shown-middle').getByRole('button', { name: 'Unstar Middle Song', exact: true })
	).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByLabel('Show')).toHaveValue('favorites');
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await playCalls(page)).toBe(0);
});

test('PLAYALL-07 auth transition fences play state; the next account starts clean', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	await mockLibrary(page, reply);
	await stubPlay(page);
	await openLibrary(page);
	const control = action(page);
	await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
	await expect(control).toBeEnabled();
	await control.click();
	await expectQueue(page, ORIGINAL, 'shown-zulu');
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'play-displayed-test-boundary');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect.poll(async () => (await control.count()) === 0 || !(await control.isEnabled())).toBe(true);
	reply.items = [{ id: 'shown-other', title: 'Other Account' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['shown-other']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(control).toBeEnabled();
	await control.click();
	await expectQueue(page, ['shown-other'], 'shown-other');
	await expectOutcome(page, { title: 'Other Account', added: 1 });
});

for (const scheme of ['light', 'dark'] as const) {
	test(`PLAYALL-08-${scheme} 320px: Tab reaches visible focus; Enter plays without horizontal overflow`, async ({ page }) => {
		await page.setViewportSize({ width: 320, height: 740 });
		await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
		await mockLibrary(page);
		await stubPlay(page);
		await openLibrary(page);
		const control = action(page);
		await expect(control, `${ACTION_MARKER} the one-action play-from-top control is missing`).toBeVisible();
		for (let tabs = 0; tabs < 30 && !(await control.evaluate((element) => element === document.activeElement)); tabs += 1) {
			await page.keyboard.press('Tab');
		}
		await expect(control).toBeFocused();
		const focus = await control.evaluate((element) => {
			const style = getComputedStyle(element);
			return { outline: style.outlineStyle, width: Number.parseFloat(style.outlineWidth), shadow: style.boxShadow };
		});
		expect((focus.outline !== 'none' && focus.width > 0) || focus.shadow !== 'none', 'keyboard focus needs a visible ring').toBe(true);
		const box = await control.boundingBox();
		expect(box!.x).toBeGreaterThanOrEqual(0);
		expect(box!.x + box!.width).toBeLessThanOrEqual(320);
		expect(box!.height, 'touch action needs a usable target').toBeGreaterThanOrEqual(44);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
		await page.keyboard.press('Enter');
		await expectQueue(page, ORIGINAL, 'shown-zulu');
		await expectOutcome(page, { title: 'Zulu Song', added: 5 });
		await expect(control).toBeEnabled();
		await page.keyboard.press('Enter');
		await expectQueue(page, ORIGINAL, 'shown-zulu');
	});
}
