import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: star or unstar a queued library track without leaving the queue.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * queue star-toggle implementation on top of this branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead of being fixed beside the implementation.
 *
 * Harness note: `/library` is client-fetched (`ssr = false`, component
 * fetches `/api/library` on mount) and the vitest setup in this package is
 * SSR-render/source-pin only — there is no DOM component-test environment.
 * The offline Playwright library specs (`library-favorites-local.spec.ts`,
 * `library-queue-announcement.spec.ts`) are the interactive harness for this
 * page, so the queue-star contract is pinned here with a route-mocked
 * `/api/library`. Fully offline and deterministic: no backend, no provider.
 *
 * Locked contract under test (so the builder never guesses):
 * - Every library queue row (`queue-item`) carries exactly one star toggle:
 *   a `button` whose accessible name matches `/star|favorite/i`, with a
 *   per-track `aria-label` containing the track's display title and an
 *   `aria-pressed` reflecting state — `Star {title}` / pressed false when
 *   unstarred, `Unstar {title}` / pressed true when starred, mirroring the
 *   shelf's `starLabel`. With an empty queue there is no toggle anywhere.
 * - Toggling reuses the single favorites source (`favorites.ts` load/save
 *   with the same auth-context scoping): starring from the queue adds the
 *   shelf entry, flips the shelf star, and persists the id in the one
 *   scoped storage key; unstarring removes the shelf entry per the shelf's
 *   empty-state semantics. No second store: shelf and queue toggles agree
 *   in both directions and no new `favorites` storage key appears.
 * - Toggles announce through the existing `queue-announcement` live region:
 *   `starred {title}` / `unstarred {title}`. One region, text replaced per
 *   toggle, never appended.
 * - Toggles never reorder, reselect, transport, play, or fetch: queue ids,
 *   now-playing, aria-current, and the queue-position summary are identical
 *   before and after every toggle; no non-GET request; no audio element.
 * - Toggles are keyboard operable (focusable, Enter/Space) and
 *   narrow-screen touch safe (visible and tappable at 360px).
 * - Demo inline queue rows are out of scope: demo catalogue ids do not
 *   resolve in library favorites, so starring them would create
 *   unresolvable entries. The demo control below pins their absence.
 *
 * Every RED test below asserts its toggle's visibility with `expect` before
 * touching it, so RED on unimplemented branches is always a failing
 * assertion — never a click timeout, compile failure, or missing import.
 * Setup assertions that already pass pre-RED (empty-queue negatives,
 * shelf-star steadiness while queueing) are not feature evidence; the RED
 * gate in each test is the first toggle-visibility assertion.
 *
 * There is no acceptance-owned unit half: the slice reuses the existing
 * `favorites.ts` load/save whose storage keys, auth scoping, and corrupt
 * fallback are already pinned (`favorites-local-acceptance.test.ts`,
 * FAV-05/06). A unit file could only import a helper that does not exist
 * yet (an import failure, not RED) or prescribe the builder's internal prop
 * names. The build's own units cover whatever toggle wiring it extracts;
 * storage reuse is pinned here at the browser contract as QSTAR-04/05/06.
 *
 * Case IDs (stable): QSTAR-01 presence + per-track labels + pressed,
 * QSTAR-02 star from queue, QSTAR-03 unstar from queue, QSTAR-04
 * bidirectional single source + reload, QSTAR-05 auth-transition purge,
 * QSTAR-06 corrupt-storage fallback, QSTAR-07 keyboard, QSTAR-08
 * narrow-screen touch, QSTAR-09 announcement replace + zero side effects,
 * QSTAR-10 demo-exclusion control (passes pre- and post-slice).
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   pnpm build && pnpm preview --port <free> --strictPort  # never reuse another worktree's preview
 *   pnpm exec playwright test --config <tmp config with that baseURL> tests/library-queue-star.spec.ts
 * Expected pre-slice RED: QSTAR-01..09 fail at the toggle-visibility
 * assertion (no star control on queue rows); QSTAR-10 passes as the demo
 * negative control. The Builder consumes these tests in one combined ready
 * PR; never merge the RED test bank by itself.
 */

// Fixture titles/artists deliberately avoid the words "star", "favorite",
// and "queue" so the toggle name filter below can only match a star toggle
// itself — never a row, a move control, or a remove control.
const ITEMS = [
	{ id: 'qstar-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'qstar-alpha', title: 'Alpha Song' },
	{ id: 'qstar-mike', title: 'Mike Song', artist: 'Harbor Lights' }
];

const TITLE_OF: Record<string, string> = Object.fromEntries(
	ITEMS.map((item) => [item.id, item.title])
);

const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const FAVORITES_KEY_BASE = 'nntune.library.favorites.v1';

async function seedLibrary(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function shelfStar(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

function favoriteItem(page: Page, id: string): Locator {
	return page.locator(`[data-testid="favorite-item"][data-id="${id}"]`);
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

function queueStar(page: Page, id: string): Locator {
	return queueItem(page, id).getByRole('button', { name: /star|favorite/i });
}

function allQueueStars(page: Page): Locator {
	return page.getByTestId('queue-item').getByRole('button', { name: /star|favorite/i });
}

/** Queue mike, zulu, alpha in gesture order; current is mike. */
async function seedQueue(page: Page): Promise<void> {
	await seedLibrary(page);
	await row(page, 'qstar-mike').click();
	await row(page, 'qstar-zulu').click();
	await row(page, 'qstar-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
}

async function queueIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

async function ariaCurrent(locator: Locator): Promise<string | null> {
	return locator.evaluate((element) => element.getAttribute('aria-current'));
}

async function readAuthContext(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.sessionStorage.getItem(key), AUTH_CONTEXT_KEY);
}

async function readFavoritesRaw(page: Page, context: string | null): Promise<string | null> {
	return page.evaluate(
		({ base, ctx }) => (ctx ? window.localStorage.getItem(`${base}.${ctx}`) : null),
		{ base: FAVORITES_KEY_BASE, ctx: context }
	);
}

function storedIds(raw: string | null): string[] {
	if (!raw) return [];
	return (JSON.parse(raw) as unknown[]).filter(
		(value): value is string => typeof value === 'string' && value.length > 0
	);
}

async function favoriteKeys(page: Page): Promise<string[]> {
	return page.evaluate((base) => {
		const keys: string[] = [];
		for (let i = 0; i < window.localStorage.length; i++) {
			const key = window.localStorage.key(i);
			if (key?.startsWith(base)) keys.push(key);
		}
		return keys.sort();
	}, FAVORITES_KEY_BASE);
}

test('[QSTAR-01] every library queue row carries one per-track star toggle', async ({
	page
}) => {
	await seedLibrary(page);

	// Empty queue: no rows, no toggles (negative setup, not feature evidence).
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(allQueueStars(page)).toHaveCount(0);

	await row(page, 'qstar-mike').click();
	await row(page, 'qstar-zulu').click();
	await row(page, 'qstar-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	// Queueing alone stars nothing on the shelf.
	await expect(shelfStar(page, 'qstar-mike')).toHaveAttribute('aria-pressed', 'false');
	await expect(shelfStar(page, 'qstar-zulu')).toHaveAttribute('aria-pressed', 'false');
	await expect(shelfStar(page, 'qstar-alpha')).toHaveAttribute('aria-pressed', 'false');

	for (const id of ['qstar-mike', 'qstar-zulu', 'qstar-alpha']) {
		const toggle = queueStar(page, id);
		await expect(toggle, `queue star toggle missing on ${id}`).toBeVisible();
		await expect(toggle).toHaveCount(1);
		await expect(toggle).toHaveAttribute('aria-label', new RegExp(TITLE_OF[id] ?? id));
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
	}
	await expect(allQueueStars(page)).toHaveCount(3);
});

test('[QSTAR-02] starring from the queue adds the shelf entry without moving the queue', async ({
	page
}) => {
	await seedQueue(page);

	const idsBefore = await queueIds(page);
	const positionBefore = await page.getByTestId('queue-position').textContent();

	const toggle = queueStar(page, 'qstar-zulu');
	await expect(toggle, 'queue star toggle missing').toBeVisible();
	await toggle.click();

	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(favoriteItem(page, 'qstar-zulu')).toContainText('Zulu Song');
	await expect(shelfStar(page, 'qstar-zulu')).toHaveAttribute('aria-pressed', 'true');
	await expect(toggle).toHaveAttribute('aria-pressed', 'true');
	await expect(toggle).toHaveAttribute('aria-label', /unstar/i);
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		'qstar-zulu'
	]);

	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toHaveCount(1);
	await expect(announcement).toContainText(/\bstarred\b/i);
	await expect(announcement).toContainText('Zulu Song');

	// Order and selection hold: still mike/zulu/alpha with mike current.
	expect(await queueIds(page)).toEqual(idsBefore);
	expect(await queueIds(page)).toEqual(['qstar-mike', 'qstar-zulu', 'qstar-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'qstar-mike'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'qstar-zulu'))).toBeNull();
	expect(await ariaCurrent(queueItem(page, 'qstar-alpha'))).toBeNull();
	expect(await page.getByTestId('queue-position').textContent()).toBe(positionBefore);
});

test('[QSTAR-03] unstarring from the queue removes the shelf entry without moving the queue', async ({
	page
}) => {
	await seedQueue(page);

	const zulu = queueStar(page, 'qstar-zulu');
	await expect(zulu, 'queue star toggle missing').toBeVisible();
	await zulu.click();
	const alpha = queueStar(page, 'qstar-alpha');
	await expect(alpha).toBeVisible();
	await alpha.click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);

	await zulu.click();

	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(favoriteItem(page, 'qstar-zulu')).toHaveCount(0);
	await expect(favoriteItem(page, 'qstar-alpha')).toBeVisible();
	await expect(shelfStar(page, 'qstar-zulu')).toHaveAttribute('aria-pressed', 'false');
	await expect(zulu).toHaveAttribute('aria-pressed', 'false');
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		'qstar-alpha'
	]);

	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toHaveCount(1);
	await expect(announcement).toContainText(/\bunstarred\b/i);
	await expect(announcement).toContainText('Zulu Song');

	expect(await queueIds(page)).toEqual(['qstar-mike', 'qstar-zulu', 'qstar-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'qstar-mike'))).toBe('true');

	// The last removal restores the honest shelf empty state.
	await alpha.click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	expect(await queueIds(page)).toEqual(['qstar-mike', 'qstar-zulu', 'qstar-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('[QSTAR-04] queue and shelf toggles share one favorites source across reload', async ({
	page
}) => {
	await seedQueue(page);

	const toggle = queueStar(page, 'qstar-mike');
	await expect(toggle, 'queue star toggle missing').toBeVisible();

	await toggle.click();
	const context = await readAuthContext(page);
	expect(context, 'library auth context must exist after a successful load').not.toBeNull();
	expect(await favoriteKeys(page)).toEqual([`${FAVORITES_KEY_BASE}.${context}`]);
	expect(storedIds(await readFavoritesRaw(page, context))).toEqual(['qstar-mike']);

	// Shelf unstar reaches the queue toggle through the same source.
	await shelfStar(page, 'qstar-mike').click();
	await expect(toggle).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	expect(storedIds(await readFavoritesRaw(page, context))).toEqual([]);
	expect(await favoriteKeys(page)).toEqual([`${FAVORITES_KEY_BASE}.${context}`]);

	// Shelf star reaches the queue toggle too; still one storage key.
	await shelfStar(page, 'qstar-alpha').click();
	await expect(queueStar(page, 'qstar-alpha')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	expect(await favoriteKeys(page)).toEqual([`${FAVORITES_KEY_BASE}.${context}`]);

	// A queue-starred favorite survives reload and still shows on the row.
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await row(page, 'qstar-alpha').click();
	const afterReload = queueStar(page, 'qstar-alpha');
	await expect(afterReload).toBeVisible();
	await expect(afterReload).toHaveAttribute('aria-pressed', 'true');
	await expect(shelfStar(page, 'qstar-alpha')).toHaveAttribute('aria-pressed', 'true');
});

test('[QSTAR-05] an auth transition purges queue-starred favorites with the shelf', async ({
	page
}) => {
	await seedQueue(page);

	const mike = queueStar(page, 'qstar-mike');
	await expect(mike, 'queue star toggle missing').toBeVisible();
	await mike.click();
	await queueStar(page, 'qstar-zulu').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	const contextBefore = await readAuthContext(page);
	expect(contextBefore).not.toBeNull();
	expect(storedIds(await readFavoritesRaw(page, contextBefore))).toEqual(
		expect.arrayContaining(['qstar-mike', 'qstar-zulu'])
	);

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-queuestar-auth-transition');
		window.localStorage.removeItem(key);
	}, AUTH_TRANSITION_KEY);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(
		await readFavoritesRaw(page, contextBefore),
		'previous-account favorites must be cleared at the auth boundary'
	).toBeNull();
	await otherTab.close();

	// The next account refetches and starts with an empty shelf and queue.
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Starring from the queue works in the new account with no carryover.
	await row(page, 'qstar-alpha').click();
	const nextToggle = queueStar(page, 'qstar-alpha');
	await expect(nextToggle).toBeVisible();
	await expect(nextToggle).toHaveAttribute('aria-pressed', 'false');
	await nextToggle.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueIds(page)).toEqual(['qstar-alpha']);
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
});

test('[QSTAR-06] corrupt favorites storage falls back to unstarred without breaking queue rows', async ({
	page
}) => {
	await page.addInitScript(
		({ contextKey, scopedKey }) => {
			window.sessionStorage.setItem(contextKey, 'test-queuestar-corrupt');
			window.localStorage.setItem(scopedKey, '{not-json');
		},
		{ contextKey: AUTH_CONTEXT_KEY, scopedKey: `${FAVORITES_KEY_BASE}.test-queuestar-corrupt` }
	);
	await seedLibrary(page);

	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await row(page, 'qstar-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	const toggle = queueStar(page, 'qstar-mike');
	await expect(toggle, 'queue star toggle missing').toBeVisible();
	await expect(toggle).toHaveAttribute('aria-pressed', 'false');

	// The toggle still works after the corrupt value is ignored.
	await toggle.click();
	await expect(toggle).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		'qstar-mike'
	]);
});

test('[QSTAR-07] Enter and Space toggle a focused queue star button', async ({ page }) => {
	await seedQueue(page);

	const toggle = queueStar(page, 'qstar-alpha');
	await expect(toggle, 'queue star toggle missing').toBeVisible();
	await toggle.focus();
	await expect(toggle, 'queue star toggle must be keyboard-focusable').toBeFocused();

	await page.keyboard.press('Enter');
	await expect(toggle).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(page.getByTestId('queue-announcement')).toContainText(/\bstarred\b/i);

	await page.keyboard.press('Space');
	await expect(toggle).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toContainText(/\bunstarred\b/i);

	expect(await queueIds(page)).toEqual(['qstar-mike', 'qstar-zulu', 'qstar-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test.describe('narrow-screen touch', () => {
	test.use({ viewport: { width: 360, height: 800 }, hasTouch: true });

	test('[QSTAR-08] the queue star toggle is visible and tappable at 360px', async ({
		page
	}) => {
		await seedQueue(page);

		const toggle = queueStar(page, 'qstar-zulu');
		await expect(toggle, 'queue star toggle must stay visible at 360px width').toBeVisible();
		await toggle.tap();

		await expect(toggle).toHaveAttribute('aria-pressed', 'true');
		await expect(page.getByTestId('favorite-item')).toHaveCount(1);
		await expect(page.getByTestId('queue-announcement')).toContainText(/\bstarred\b/i);

		await toggle.tap();
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByTestId('favorite-item')).toHaveCount(0);
		await expect(page.getByTestId('queue-announcement')).toContainText(/\bunstarred\b/i);

		expect(await queueIds(page)).toEqual(['qstar-mike', 'qstar-zulu', 'qstar-alpha']);
		await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	});
});

test('[QSTAR-09] queue toggles replace one announcement and mutate nothing else', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));

	await seedQueue(page);

	const zulu = queueStar(page, 'qstar-zulu');
	await expect(zulu, 'queue star toggle missing').toBeVisible();
	await zulu.click();

	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toHaveCount(1);
	await expect(announcement).toContainText(/\bstarred\b/i);
	await expect(announcement).toContainText('Zulu Song');

	// The next toggle replaces the outcome text; it never appends beside it.
	await queueStar(page, 'qstar-alpha').click();
	await expect(page.getByTestId('queue-announcement')).toHaveCount(1);
	await expect(announcement).toContainText('Alpha Song');
	await expect(announcement).not.toContainText('Zulu Song');

	await zulu.click();
	await expect(page.getByTestId('queue-announcement')).toHaveCount(1);
	await expect(announcement).toContainText(/\bunstarred\b/i);
	await expect(announcement).toContainText('Zulu Song');

	// Non-vacuous gates: both toggles under observation succeeded.
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	expect(await queueIds(page)).toEqual(['qstar-mike', 'qstar-zulu', 'qstar-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	const mutations = requests.filter((request) => request.method !== 'GET');
	expect(mutations, 'queue toggles must not mutate over the network').toEqual([]);
	await expect(
		page.locator('audio[data-testid="audio-player"]'),
		'queue toggles must not start playback'
	).toHaveCount(0);
});

test('[QSTAR-10] demo inline queue rows carry no star toggle', async ({ page }) => {
	// Control, not RED: demo catalogue ids never resolve in library
	// favorites, so the slice must not star there. Passes pre- and post-.
	await page.route('**/api/demo/audio**', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'audio/wav',
			headers: { 'accept-ranges': 'bytes' },
			body: Buffer.alloc(2048, 7)
		})
	);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(
		page.getByTestId('queue-item').getByRole('button', { name: /star|favorite/i })
	).toHaveCount(0);
});
