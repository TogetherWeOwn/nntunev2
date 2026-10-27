import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * BULK acceptance bank — protected; the paired build owns the combined GREEN
 * ready PR. Do not change this file to accommodate an implementation, or open
 * a standalone ready PR containing the deliberate pre-feature failures.
 *
 * From repo root (existing package scripts and mocked-browser harness):
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test src/routes/library/bulk-queue.acceptance.test.ts
 *   pnpm --dir apps/web test:e2e tests/library-bulk-queue.spec.ts --workers=1
 * Omit an extra `--` with this pnpm version: otherwise the runners ignore the
 * focus/options and run the whole suite. Linux browsers may need the host's
 * installed Chromium libraries on LD_LIBRARY_PATH; do not replace the browser.
 * Regressions: same test:e2e script with tests/library-ordering.spec.ts,
 * tests/library-selectable.spec.ts and tests/library-selection-isolation.spec.ts.
 *
 * Real /library, real queue and real ordering; only /api/library is mocked.
 * No backend/provider/production requests. Existing hook IDs are shared with
 * the neighboring library specs; the new action uses its accessible name.
 * U01–07 and browser case IDs form the handoff; no new production export needed.
 * RED verified 2026-10-24 against adaeac2ad6d2d1967679521618a00e3db60cf62b:
 * BULK-00 passes; BULK-01–08 and BULK-09-light/dark fail on absent action
 * assertions after successful browser/app setup. The 17 ordering/selectable/
 * auth-isolation cases and 28 favorites/row-state regressions pass unchanged.
 * Narrow/keyboard bulk SUCCESS evidence remains the build's GREEN obligation;
 * these pre-feature failures are not a claim the missing control is usable.
 *
 * Breaks caught: queue built from raw API/favorites instead of displayed order;
 * replaced prefix/current; inaccurate added count; duplicate activation; stale
 * enabled state; network/storage/favorite/media side effects; auth leakage;
 * refresh auto-queueing; inaccessible or overflowing narrow-screen action.
 * Pre-feature expected RED: action/disabled-state assertions, never setup errors.
 */

const LABEL = 'Add displayed tracks to queue';
const ITEMS = [
	{ id: 'bulk-z', title: 'Zulu Song', artist: 'Bravo Band' },
	{ id: 'bulk-a', title: 'Alpha Song', artist: 'Zulu Band' },
	{ id: 'bulk-untitled', title: '   ', artist: '   ' },
	{ id: 'bulk-m', title: 'Middle Song', artist: 'Alpha Band' }
];
const ORIGINAL = ['bulk-z', 'bulk-a', 'bulk-untitled', 'bulk-m'];
const TITLE = ['bulk-a', 'bulk-untitled', 'bulk-m', 'bulk-z'];
const ARTIST = ['bulk-m', 'bulk-z', 'bulk-a', 'bulk-untitled'];

type Reply = { items: unknown; status?: number };

function action(page: Page): Locator {
	return page.getByRole('button', { name: LABEL, exact: true });
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

async function expectAdded(page: Page, count: number) {
	const confirmation = page.getByRole('status').filter({
		hasText: new RegExp(`\\badded\\s+${count}\\s+tracks?\\b|\\b${count}\\s+tracks?\\s+added\\b`, 'i')
	});
	await expect(confirmation, `announce the actual ${count} missing IDs, not the view's total`).toBeVisible();
	await expect(confirmation).not.toContainText(/play all|streaming|now playing/i);
}

async function expectUnavailable(page: Page) {
	// Loading/auth/error may hide the action. If present, it cannot be actionable.
	await expect.poll(async () => !(await action(page).count()) || !(await action(page).isEnabled())).toBe(true);
}

async function storage(page: Page) {
	return page.evaluate(() => ({
		local: Object.fromEntries(Object.entries(localStorage).sort()),
		session: Object.fromEntries(Object.entries(sessionStorage).sort())
	}));
}

test.beforeEach(async ({ context }) => {
	// Per-page library mocks take precedence. Everything else stays local and
	// any unmocked API call is refused rather than touching a backend.
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		return route.continue();
	});
});

test('BULK-00 control: existing library order, single add and current selection work', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	await expectView(page, ORIGINAL);
	await rowAction(page, 'bulk-z').click();
	await rowAction(page, 'bulk-a').click();
	await page.getByTestId('player-next').click();
	await expectQueue(page, ['bulk-z', 'bulk-a'], 'bulk-a');
	await expect(page.locator('audio')).toHaveCount(0);
});

test('BULK-01 title-sorted view appends in displayed order, announces four, and disables repeat', async ({ page }) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	await sort(page, 'Title A–Z');
	await expectView(page, TITLE);
	await expect(action(page), 'the one-action bulk queue control is missing').toBeVisible();
	await expect(action(page)).toBeEnabled();
	const buttonBox = await action(page).boundingBox();
	const listBox = await page.getByTestId('library-list').boundingBox();
	expect(buttonBox!.y + buttonBox!.height).toBeLessThanOrEqual(listBox!.y);
	await action(page).click();
	await expectQueue(page, TITLE, 'bulk-a');
	await expectAdded(page, 4);
	await expect(action(page)).toBeDisabled();
	await expect(page.getByText(/all.*(?:queued|queue)|already.*(?:queued|queue)|no.*tracks.*add/i).first()).toBeVisible();
	await action(page).evaluate((button: HTMLButtonElement) => { button.click(); button.click(); });
	await expectQueue(page, TITLE, 'bulk-a');
	expect(api.reads()).toBe(1);
});

test('BULK-02 mixed queued IDs preserve arranged prefix/current index, count only missing, then re-enable', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	await rowAction(page, 'bulk-z').click();
	await rowAction(page, 'bulk-a').click();
	await queued(page, 'bulk-z').getByRole('button', { name: 'Move Zulu Song down', exact: true }).click();
	await expectQueue(page, ['bulk-a', 'bulk-z'], 'bulk-z');
	await sort(page, 'Title A–Z');
	await expectView(page, TITLE);
	await expect(action(page)).toBeEnabled();
	await action(page).click();
	await expectQueue(page, ['bulk-a', 'bulk-z', 'bulk-untitled', 'bulk-m'], 'bulk-z');
	await expectAdded(page, 2);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(action(page)).toBeDisabled();
	await queued(page, 'bulk-m').getByRole('button', { name: 'Remove Middle Song from queue', exact: true }).click();
	await expect(action(page)).toBeEnabled();
	await action(page).focus();
	await page.keyboard.press('Space');
	await expectQueue(page, ['bulk-a', 'bulk-z', 'bulk-untitled', 'bulk-m'], 'bulk-z');
	await expectAdded(page, 1);
	await expect(action(page)).toBeDisabled();
});

test('BULK-03 artist order uses title/artist fallbacks without storage, favorites, network or media effects', async ({ page }) => {
	let playCalls = 0;
	await page.exposeFunction('bulkUnexpectedPlay', () => { playCalls += 1; });
	await page.addInitScript(() => {
		const original = HTMLMediaElement.prototype.play;
		HTMLMediaElement.prototype.play = function () {
			void (window as unknown as { bulkUnexpectedPlay: () => Promise<void> }).bulkUnexpectedPlay();
			return original.call(this);
		};
	});
	const api = await mockLibrary(page);
	await openLibrary(page);
	await row(page, 'bulk-z').getByRole('button', { name: 'Star Zulu Song', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await sort(page, 'Artist A–Z');
	await expectView(page, ARTIST);
	const before = await storage(page);
	await expect(action(page)).toBeEnabled();
	await action(page).click();
	await expectQueue(page, ARTIST, 'bulk-m');
	await expectAdded(page, 4);
	await expect(queued(page, 'bulk-untitled')).toContainText('bulk-untitled');
	await expect(queued(page, 'bulk-untitled')).not.toContainText(/undefined|null|unknown artist/i);
	await expect(page.getByTestId('now-playing')).toContainText('Middle Song — Alpha Band');
	await expect(row(page, 'bulk-z').getByRole('button', { name: 'Unstar Zulu Song', exact: true })).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	expect(await storage(page), 'bulk is in-memory only; even existing snapshots/favorites are untouched').toEqual(before);
	expect(api.mutations).toEqual([]);
	expect(api.media).toEqual([]);
	expect(api.reads()).toBe(1);
	expect(playCalls).toBe(0);
	await expect(page.locator('audio')).toHaveCount(0);
	await expect(page.getByRole('button', { name: /play all/i })).toHaveCount(0);
});

test('BULK-04 empty library has a disabled labeled action, no queue and no false added confirmation', async ({ page }) => {
	const api = await mockLibrary(page, { items: [] });
	await openLibrary(page, 0);
	await expect(action(page)).toBeVisible();
	await expect(action(page)).toBeDisabled();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByRole('status').filter({ hasText: /added\s+[1-9]|[1-9]\s+tracks?\s+added/i })).toHaveCount(0);
	await action(page).evaluate((button: HTMLButtonElement) => button.click());
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(api.reads()).toBe(1);
	expect(api.mutations).toEqual([]);
});

test('BULK-05 singleton reports one; clear re-enables; already-queued starts disabled', async ({ page }) => {
	await mockLibrary(page, { items: [ITEMS[0]] });
	await openLibrary(page, 1);
	await rowAction(page, 'bulk-z').click();
	await expect(action(page)).toBeDisabled();
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
	await expect(action(page)).toBeEnabled();
	await action(page).click();
	await expectQueue(page, ['bulk-z'], 'bulk-z');
	await expectAdded(page, 1);
	await expect(action(page)).toBeDisabled();
});

test('BULK-06 initial pending/refused response never enables bulk; Retry only loads, never queues', async ({ page }) => {
	let release!: () => void;
	const pending = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/api/library', async (route) => {
		await pending;
		await route.fulfill({ status: 401, contentType: 'application/json', body: '{}' });
	});
	await page.goto('/library');
	await expect(page.getByRole('status').filter({ hasText: /loading/i })).toBeVisible();
	await expectUnavailable(page);
	release();
	await expect(page.getByRole('alert')).toBeVisible();
	await expectUnavailable(page);
	await mockLibrary(page);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ORIGINAL);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(action(page)).toBeEnabled();
	await action(page).click();
	await expectQueue(page, ORIGINAL, 'bulk-z');
	await expectAdded(page, 4);
});

test('BULK-07 refresh preserves surviving current, never auto-adds and bulk uses the new sorted view', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	const api = await mockLibrary(page, reply);
	await openLibrary(page);
	await rowAction(page, 'bulk-z').click();
	await sort(page, 'Title A–Z');
	reply.status = 503;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expectView(page, TITLE);
	await expectQueue(page, ['bulk-z'], 'bulk-z');
	reply.status = 200;
	reply.items = [ITEMS[0], { id: 'bulk-new', title: 'New Song', artist: 'New Band' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['bulk-new', 'bulk-z']);
	await expectQueue(page, ['bulk-z'], 'bulk-z');
	await expect(action(page)).toBeEnabled();
	await action(page).click();
	await expectQueue(page, ['bulk-z', 'bulk-new'], 'bulk-z');
	await expectAdded(page, 1);
	expect(api.reads()).toBe(3);
	expect(api.mutations).toEqual([]);
});

test('BULK-08 auth transition clears bulk-populated queue; next account adds only its own rows', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	await mockLibrary(page, reply);
	await openLibrary(page);
	await expect(action(page)).toBeEnabled();
	await action(page).click();
	await expectQueue(page, ORIGINAL, 'bulk-z');
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'bulk-test-boundary');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expectUnavailable(page);
	reply.items = [{ id: 'bulk-other', title: 'Other Account' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['bulk-other']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(action(page)).toBeEnabled();
	await action(page).click();
	await expectQueue(page, ['bulk-other'], 'bulk-other');
	await expectAdded(page, 1);
});

for (const scheme of ['light', 'dark'] as const) {
	test(`BULK-09-${scheme} 320px: Tab reaches visible focus above rows; Enter adds without horizontal overflow`, async ({ page }) => {
		await page.setViewportSize({ width: 320, height: 740 });
		await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
		await mockLibrary(page);
		await openLibrary(page);
		await expect(action(page)).toBeVisible();
		for (let tabs = 0; tabs < 30 && !(await action(page).evaluate((element) => element === document.activeElement)); tabs += 1) {
			await page.keyboard.press('Tab');
		}
		await expect(action(page)).toBeFocused();
		const focus = await action(page).evaluate((element) => {
			const style = getComputedStyle(element);
			return { outline: style.outlineStyle, width: Number.parseFloat(style.outlineWidth), shadow: style.boxShadow };
		});
		expect((focus.outline !== 'none' && focus.width > 0) || focus.shadow !== 'none', 'keyboard focus needs a visible ring').toBe(true);
		const box = await action(page).boundingBox();
		expect(box!.x).toBeGreaterThanOrEqual(0);
		expect(box!.x + box!.width).toBeLessThanOrEqual(320);
		expect(box!.height, 'touch action needs a usable target').toBeGreaterThanOrEqual(44);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
		await page.keyboard.press('Enter');
		await expectQueue(page, ORIGINAL, 'bulk-z');
		await expectAdded(page, 4);
		await expect(action(page)).toBeDisabled();
		await page.keyboard.press('Enter');
		await expectQueue(page, ORIGINAL, 'bulk-z');
	});
}
