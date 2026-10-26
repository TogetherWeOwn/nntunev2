import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: favorites tell the truth about unsaved edits and about saved
 * favorites absent from the loaded tracks.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * favorites-truth implementation on top of this branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead of being fixed beside the implementation.
 *
 * Harness note: `/library` is client-fetched and the vitest setup in this
 * package has no DOM component-test environment, so the contract is pinned
 * here with a route-mocked `/api/library`, following the offline
 * `library-favorites-local` and `library-favorites-arrangement` browser
 * specs. Fully offline and deterministic: only HTTP is mocked; the real
 * library page, scoped browser storage and queue are under test. The pure
 * half (`favorites-truth-acceptance.test.ts`) pins the same save-outcome
 * and count-projection contract at the module seam.
 *
 * UI contract the build implements inside `data-testid="favorites-section"`:
 * - `data-testid="favorites-save-notice"`: visit-local feedback shown after
 *   any unsuccessful user edit (row star toggle, queued star toggle, shelf
 *   unstar, move, clear-all) while the scoped write fails or storage or the
 *   auth context is unavailable. Hidden when the last edit persisted and
 *   before any edit. Text matches /not saved|unsaved/i; it never calls an
 *   edit saved, never carries exception text, raw track ids or
 *   auth-context material, and is polite (`aria-live="polite"` or
 *   `role="status"`, never assertive). It carries the explicit retry
 *   control: a button whose accessible name matches
 *   /retry|try again|save again|save now/i. Retry persists the complete
 *   current ordered in-memory set (including ids absent from the loaded
 *   snapshot) through the existing scoped key; a confirmed write clears
 *   the notice, a failed one preserves memory and order. No automatic
 *   retries, background timers, new locks or migrations: each user edit
 *   attempts exactly one write, and refresh never rewrites favorites.
 * - `data-testid="favorites-summary"`: resolved/unresolved counts. Truly
 *   zero favorites keep the existing `favorites-empty` ("No favorites
 *   yet") copy with no summary. Saved favorites absent from the loaded
 *   tracks are counted with /not (in|shown|among)/ phrasing plus the
 *   digits; mixed states show both counts. The summary never labels
 *   absent ids deleted or inaccessible, never shows raw ids, never
 *   fetches existence information, never prunes ids, and never changes
 *   queue-all-resolved semantics (Queue favorites still appends exactly
 *   the resolved rows).
 * - Auth transitions clear private counts, warnings and retry snapshots
 *   before a new context can appear; the next account starts truly empty.
 * - Current player, audio, queue, hidden favorite order and keyboard focus
 *   are preserved across unsaved edits and recovery.
 *
 * Case IDs (stable): FAVTRUTH-E01 failed-edit notice, E02 writers,
 * E03 explicit retry, E04 failed retry plus no auto-retry, E05 refresh
 * and reload authority, E06 ghost-only summary, E07 mixed summary plus
 * queue-all-resolved, E08 auth transition, E09 preservation, E10 320px
 * light/dark, E11 storage-denied usability, E12 healthy controls.
 */

// Fixture titles/artists deliberately avoid the words "star", "favorite",
// "retry", "saved" and "unsaved" so the toggle/notice name filters below
// can only match the controls themselves, never row or notice prose.
const ITEMS = [
	{ id: 'truth-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'truth-alpha', title: 'Alpha Song' },
	{ id: 'truth-mike', title: 'Mike Song', artist: 'Harbor Lights' }
];
const GHOST_A = 'truth-ghost-alpha';
const GHOST_B = 'truth-ghost-beta';
const CONTEXT = 'truth-account-a';
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const KEY_BASE = 'nntune.library.favorites.v1';
const KEY = `${KEY_BASE}.${CONTEXT}`;
const RETRY_NAME = /retry|try again|save again|save now/i;
const UNSAVED_TEXT = /not saved|unsaved/i;
const ABSENT_TEXT = /not (in|shown|among)/i;
const BANNED_ABSENT_WORDS = /delet|inaccessible/i;
const EXCEPTION_TEXT = /securityerror|quotaexceeded|notallowederror|domexception|typeerror|syntaxerror|unexpected token/i;

function section(page: Page): Locator {
	return page.getByTestId('favorites-section');
}
function notice(page: Page): Locator {
	return section(page).getByTestId('favorites-save-notice');
}
function retryButton(page: Page): Locator {
	return notice(page).getByRole('button', { name: RETRY_NAME });
}
function summary(page: Page): Locator {
	return section(page).getByTestId('favorites-summary');
}
function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /^(?:Unstar|Star) / });
}
function queuedStarButton(page: Page, id: string, title: string): Locator {
	return page
		.locator(`[data-testid="queue-item"][data-track-id="${id}"]`)
		.getByRole('button', { name: new RegExp(`^(?:Unstar|Star) ${title}$`) });
}
function moveButton(page: Page, title: string, direction: 'up' | 'down'): Locator {
	return section(page).getByRole('button', {
		name: `Move favorite ${title} ${direction}`,
		exact: true
	});
}
function shelfUnstar(page: Page, title: string): Locator {
	return section(page).getByRole('button', { name: `Unstar ${title}`, exact: true });
}

async function openLibrary(
	page: Page,
	options: { items?: typeof ITEMS; context?: string; stored?: string[] } = {}
): Promise<void> {
	const items = options.items ?? ITEMS;
	await page.addInitScript(
		({ contextKey, context, key, stored }) => {
			// Seed only the first document; reloads must observe whatever the
			// page under test persisted, never a re-seeded fixture.
			if (window.sessionStorage.getItem('test.truth.seeded')) return;
			window.sessionStorage.setItem('test.truth.seeded', '1');
			if (context) window.sessionStorage.setItem(contextKey, context);
			if (stored) window.localStorage.setItem(key, JSON.stringify(stored));
		},
		{ contextKey: CONTEXT_KEY, context: options.context ?? null, key: KEY, stored: options.stored ?? null }
	);
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(items) })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	await expect(section(page)).toBeVisible();
}

/** Refuse scoped favorites writes while counting every attempt; other keys pass through. */
async function failFavoritesWrites(page: Page): Promise<void> {
	await page.evaluate((base) => {
		const setItem = Storage.prototype.setItem;
		Reflect.set(window, '__truthWriteCalls', [] as string[]);
		Storage.prototype.setItem = function (key, value) {
			if (this === window.localStorage && key.startsWith(`${base}.`)) {
				(Reflect.get(window, '__truthWriteCalls') as string[]).push(key);
				throw new DOMException('Favorites writes unavailable', 'QuotaExceededError');
			}
			return setItem.call(this, key, value);
		};
		Reflect.set(window, '__restoreTruthStorage', () => {
			Storage.prototype.setItem = setItem;
		});
	}, KEY_BASE);
}

async function favoritesWriteAttempts(page: Page): Promise<number> {
	return page.evaluate(() => (Reflect.get(window, '__truthWriteCalls') as string[]).length);
}

async function restoreTruthStorage(page: Page): Promise<void> {
	await page.evaluate(() => (Reflect.get(window, '__restoreTruthStorage') as () => void)());
}

/** Deny the whole localStorage area, as in blocked-cookies contexts. Installed after load. */
async function faultLocalStorageGetter(page: Page): Promise<void> {
	await page.evaluate(() => {
		const descriptor = Object.getOwnPropertyDescriptor(window, 'localStorage')!;
		Object.defineProperty(window, 'localStorage', {
			configurable: true,
			get() {
				throw new DOMException('Storage unavailable', 'SecurityError');
			}
		});
		Reflect.set(window, '__restoreTruthGetter', () => {
			Object.defineProperty(window, 'localStorage', descriptor);
		});
	});
}

async function restoreTruthGetter(page: Page): Promise<void> {
	await page.evaluate(() => (Reflect.get(window, '__restoreTruthGetter') as () => void)());
}

async function orderedFavoriteIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id')));
}

async function readAuthContext(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.sessionStorage.getItem(key), CONTEXT_KEY);
}

async function readFavoritesRaw(page: Page, context: string | null): Promise<string | null> {
	return page.evaluate(
		({ base, ctx }) => (ctx ? window.localStorage.getItem(`${base}.${ctx}`) : null),
		{ base: KEY_BASE, ctx: context }
	);
}

function storedIds(raw: string | null): string[] {
	if (!raw) return [];
	return (JSON.parse(raw) as unknown[]).filter(
		(value): value is string => typeof value === 'string' && value.length > 0
	);
}

async function queueIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

function escapeRegExp(text: string): string {
	return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

const audits = new WeakMap<Page, { errors: string[]; mutations: string[] }>();

test.beforeEach(async ({ page }) => {
	const audit = { errors: [] as string[], mutations: [] as string[] };
	audits.set(page, audit);
	page.on('pageerror', (error) => audit.errors.push(error.message));
	page.on('request', (request) => {
		if (request.method() !== 'GET') audit.mutations.push(`${request.method()} ${request.url()}`);
	});
	// Unexpected API requests cannot reach a backend. Seed routes register
	// after this catch-all (Playwright is LIFO), so the mocked library
	// response still lands. Page-scoped so a second tab can still drive
	// the real auth-transition broadcast in E08.
	await page.route('**/api/**', (route) => route.abort('blockedbyclient'));
});

test.afterEach(async ({ page }) => {
	const audit = audits.get(page)!;
	expect.soft(audit.errors, 'favorites truth must not cause an uncaught browser error').toEqual([]);
	expect.soft(audit.mutations, 'favorites truth must never fetch or mutate over the network').toEqual([]);
});

test('[FAVTRUTH-E01] a failed star shows an unsaved notice while memory stays starred and storage stays stale', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT });
	await starButton(page, 'truth-zulu').click();
	await expect(starButton(page, 'truth-zulu')).toHaveAttribute('aria-pressed', 'true');
	await expect(notice(page), 'no notice while writes succeed').toHaveCount(0);
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		'truth-zulu'
	]);

	await failFavoritesWrites(page);
	await starButton(page, 'truth-mike').click();

	await expect(
		notice(page),
		'a failed edit must surface visit-local unsaved feedback'
	).toBeVisible();
	await expect(notice(page)).toContainText(UNSAVED_TEXT);
	expect(await favoritesWriteAttempts(page)).toBe(1);
	// Memory is authoritative this visit: both tracks stay starred in order.
	await expect(starButton(page, 'truth-mike')).toHaveAttribute('aria-pressed', 'true');
	await expect(starButton(page, 'truth-zulu')).toHaveAttribute('aria-pressed', 'true');
	expect(await orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-mike']);
	// Stale persisted membership is untouched.
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		'truth-zulu'
	]);

	// Polite, and free of exception text, raw ids and auth-context material.
	const live = await notice(page).getAttribute('aria-live');
	const role = await notice(page).getAttribute('role');
	expect(live === 'polite' || role === 'status', 'save notice must be polite').toBe(true);
	expect(live, 'save notice must never be assertive').not.toBe('assertive');
	const text = (await notice(page).textContent()) ?? '';
	expect(text).toMatch(UNSAVED_TEXT);
	expect(text).not.toMatch(EXCEPTION_TEXT);
	expect(text).not.toMatch(/truth-zulu|truth-mike/);
	expect(text).not.toMatch(new RegExp(escapeRegExp((await readAuthContext(page)) ?? 'no-context')));
	// Unrelated storage still works: only the favorites write is refused.
	expect(
		await page.evaluate(() => {
			window.localStorage.setItem('truth.probe', 'available');
			return window.localStorage.getItem('truth.probe');
		})
	).toBe('available');
});

test('[FAVTRUTH-E02] move, shelf unstar and clear-all surface the same notice while storage fails', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, 'truth-zulu', GHOST_B, 'truth-alpha'] });
	await expect
		.poll(() => orderedFavoriteIds(page))
		.toEqual(['truth-zulu', 'truth-alpha']);
	await expect(notice(page), 'no notice while writes succeed').toHaveCount(0);

	await failFavoritesWrites(page);
	const attemptsBefore = await favoritesWriteAttempts(page);

	// Move persists the full set through the truthful path: hidden ids ride along.
	await moveButton(page, 'Alpha Song', 'up').click();
	await expect(notice(page), 'a failed move must surface unsaved feedback').toBeVisible();
	expect(await orderedFavoriteIds(page)).toEqual(['truth-alpha', 'truth-zulu']);
	expect(await favoritesWriteAttempts(page)).toBe(attemptsBefore + 1);

	// Shelf unstar removes one id from the full visit state.
	await shelfUnstar(page, 'Zulu Song').click();
	await expect(notice(page), 'a failed shelf unstar must surface unsaved feedback').toBeVisible();
	expect(await orderedFavoriteIds(page)).toEqual(['truth-alpha']);

	// Clear-all empties memory; the empty write itself can fail too.
	await section(page).getByRole('button', { name: 'Clear local favorites', exact: true }).click();
	await section(page).getByRole('button', { name: 'Yes, clear local favorites', exact: true }).click();
	await expect(notice(page), 'a failed clear must surface unsaved feedback').toBeVisible();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);

	// Every failed writer left the last saved set untouched.
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		GHOST_A,
		'truth-zulu',
		GHOST_B,
		'truth-alpha'
	]);
});

test('[FAVTRUTH-E03] explicit retry persists the complete ordered set including absent ids, then clears', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, 'truth-zulu'] });
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu']);
	await failFavoritesWrites(page);

	await starButton(page, 'truth-alpha').click();
	await expect(notice(page), 'a failed edit must surface unsaved feedback').toBeVisible();
	await moveButton(page, 'Alpha Song', 'up').click();
	expect(await orderedFavoriteIds(page)).toEqual(['truth-alpha', 'truth-zulu']);

	await restoreTruthStorage(page);
	await expect(retryButton(page), 'the notice must offer an explicit retry').toBeVisible();
	await retryButton(page).click();
	await expect(notice(page), 'a confirmed retry clears the notice').toHaveCount(0);
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		GHOST_A,
		'truth-alpha',
		'truth-zulu'
	]);

	// The retried set is what a reload restores, in order.
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-alpha', 'truth-zulu']);
	await expect(notice(page), 'no notice after a clean reload of saved state').toHaveCount(0);
});

test('[FAVTRUTH-E04] a failed retry preserves the notice and memory, with no automatic retries', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, 'truth-zulu'] });
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu']);
	await failFavoritesWrites(page);

	await starButton(page, 'truth-alpha').click();
	await expect(notice(page), 'a failed edit must surface unsaved feedback').toBeVisible();
	expect(await favoritesWriteAttempts(page)).toBe(1);

	// No background timer may rewrite: the attempt count stays flat while waiting.
	await page.waitForTimeout(1000);
	expect(await favoritesWriteAttempts(page)).toBe(1);
	await expect(notice(page), 'the notice must persist without an automatic retry').toBeVisible();

	// An explicit retry while storage still fails preserves everything.
	await retryButton(page).click();
	expect(await favoritesWriteAttempts(page)).toBe(2);
	await expect(notice(page), 'a failed retry must preserve the notice').toBeVisible();
	expect(await orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);
	await expect(starButton(page, 'truth-alpha')).toHaveAttribute('aria-pressed', 'true');
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		GHOST_A,
		'truth-zulu'
	]);

	// Refresh never rewrites favorites and never clears pending memory.
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(
				ITEMS.map((item) =>
					item.id === 'truth-mike' ? { ...item, title: 'Mike Song refreshed' } : item
				)
			)
		})
	);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await expect(row(page, 'truth-mike')).toContainText('Mike Song refreshed');
	expect(await favoritesWriteAttempts(page)).toBe(2);
	expect(await orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);
	await expect(notice(page), 'refresh must preserve a pending unsaved notice').toBeVisible();
});

test('[FAVTRUTH-E05] refresh keeps unsaved memory authoritative; reload falls back to the last saved set', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, 'truth-zulu'] });
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu']);
	await failFavoritesWrites(page);

	await starButton(page, 'truth-alpha').click();
	await expect(notice(page), 'a failed edit must surface unsaved feedback').toBeVisible();

	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(
				ITEMS.map((item) =>
					item.id === 'truth-mike' ? { ...item, title: 'Mike Song refreshed' } : item
				)
			)
		})
	);
	await page.getByRole('button', { name: 'Refresh' }).click();
	// Changed server text proves the response was committed, not merely requested.
	await expect(row(page, 'truth-mike')).toContainText('Mike Song refreshed');
	expect(await orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);
	await expect(notice(page), 'refresh must not reload stale persisted membership').toBeVisible();
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		GHOST_A,
		'truth-zulu'
	]);

	// A reload is a new visit: the last saved set returns, unsaved edits do not.
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu']);
	await expect(starButton(page, 'truth-alpha')).toHaveAttribute('aria-pressed', 'false');
	await expect(notice(page), 'a fresh visit starts with no pending notice').toHaveCount(0);
});

test('[FAVTRUTH-E06] ghost-only favorites show an absent-tracks summary instead of the zero state', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, GHOST_B] });
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(section(page).getByRole('button', { name: 'Queue favorites' })).toHaveCount(0);

	await expect(
		summary(page),
		'saved favorites absent from loaded tracks must be summarized'
	).toBeVisible();
	await expect(summary(page)).toContainText(/2/);
	await expect(summary(page)).toContainText(ABSENT_TEXT);
	const empty = page.getByTestId('favorites-empty');
	if ((await empty.count()) > 0) {
		await expect(empty, 'ghost-only must never read as zero favorites').not.toContainText(
			/no favorites yet/i
		);
	}
	const text = (await section(page).textContent()) ?? '';
	expect(text).not.toMatch(BANNED_ABSENT_WORDS);
	expect(text).not.toMatch(new RegExp(escapeRegExp(GHOST_A)));
	expect(text).not.toMatch(new RegExp(escapeRegExp(GHOST_B)));
	expect(text).not.toMatch(EXCEPTION_TEXT);
});

test('[FAVTRUTH-E07] mixed favorites show both counts while Queue favorites still appends resolved rows only', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, 'truth-zulu'] });
	await starButton(page, 'truth-alpha').click();
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);

	await expect(summary(page), 'mixed states must show both counts').toBeVisible();
	await expect(summary(page)).toContainText(ABSENT_TEXT);
	const counts = ((await summary(page).textContent()) ?? '').match(/\d+/g) ?? [];
	expect(counts.length, 'mixed summary must carry two digits').toBeGreaterThanOrEqual(2);

	const action = section(page).getByRole('button', { name: 'Queue favorites', exact: true });
	await action.click();
	await expect(page.getByTestId('queue-announcement')).toContainText(/queued 2.*0 already queued/i);
	expect(await queueIds(page)).toEqual(['truth-zulu', 'truth-alpha']);
	// Queueing neither prunes absent ids nor resolves their existence elsewhere.
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		GHOST_A,
		'truth-zulu',
		'truth-alpha'
	]);
	const text = (await section(page).textContent()) ?? '';
	expect(text).not.toMatch(BANNED_ABSENT_WORDS);
	expect(text).not.toMatch(new RegExp(escapeRegExp(GHOST_A)));
});

test('[FAVTRUTH-E08] an auth transition clears the notice, summary and retry snapshot for the next account', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, 'truth-zulu'] });
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu']);
	await failFavoritesWrites(page);
	await starButton(page, 'truth-alpha').click();
	await expect(notice(page), 'a failed edit must surface unsaved feedback').toBeVisible();
	const oldContext = await readAuthContext(page);

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'truth-auth-transition');
		window.localStorage.removeItem(key);
	}, TRANSITION_KEY);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(notice(page), 'the transition must clear a pending notice').toHaveCount(0);
	await expect(summary(page), 'the transition must clear private counts').toHaveCount(0);
	await otherTab.close();

	await restoreTruthStorage(page);
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	const newContext = await readAuthContext(page);
	expect(newContext).not.toBeNull();
	expect(newContext).not.toBe(oldContext);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(notice(page), 'the next account starts with no notice').toHaveCount(0);
	await expect(summary(page), 'the next account starts with no summary').toHaveCount(0);
	expect(await readFavoritesRaw(page, oldContext)).toBeNull();
	// The new account saves under its own key only.
	await starButton(page, 'truth-mike').click();
	expect(storedIds(await readFavoritesRaw(page, newContext))).toEqual(['truth-mike']);
	expect(await readFavoritesRaw(page, oldContext)).toBeNull();
});

test('[FAVTRUTH-E09] unsaved edits preserve queue, player, audio and keyboard focus', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT });
	await row(page, 'truth-zulu').click();
	await row(page, 'truth-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	const queuedBefore = await queueIds(page);
	const nowPlayingBefore = (await page.getByTestId('now-playing').textContent()) ?? '';
	expect(nowPlayingBefore.length).toBeGreaterThan(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);

	await failFavoritesWrites(page);
	const star = starButton(page, 'truth-mike');
	await star.focus();
	await page.keyboard.press('Enter');
	await expect(star, 'a failed keyboard toggle must keep focus on the toggle').toBeFocused();
	await expect(
		notice(page),
		'a failed edit must surface unsaved feedback'
	).toBeVisible();
	expect(await queueIds(page)).toEqual(queuedBefore);
	expect(await page.getByTestId('now-playing').textContent()).toBe(nowPlayingBefore);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);

	// A queued star toggle is a writer too: queue membership and playback are untouched.
	await queuedStarButton(page, 'truth-alpha', 'Alpha Song').click();
	await expect(notice(page), 'a failed queued toggle must surface unsaved feedback').toBeVisible();
	expect(await queueIds(page)).toEqual(queuedBefore);
	expect(await page.getByTestId('now-playing').textContent()).toBe(nowPlayingBefore);

	// A failed move keeps focus inside the moved track's controls.
	const mover = moveButton(page, 'Mike Song', 'up');
	await mover.focus();
	await page.keyboard.press('Enter');
	expect(await orderedFavoriteIds(page)).toEqual(['truth-mike', 'truth-alpha']);
	await expect(
		page.locator('[data-testid="favorite-item"][data-id="truth-mike"] :focus')
	).toHaveCount(1);
	expect(await queueIds(page)).toEqual(queuedBefore);
});

test('[FAVTRUTH-E10] 320px light/dark: notice, retry and summary stay visible with no horizontal scroll', async ({
	page
}) => {
	await page.setViewportSize({ width: 320, height: 700 });
	await openLibrary(page, { context: CONTEXT, stored: [GHOST_A, 'truth-zulu'] });
	await failFavoritesWrites(page);
	await starButton(page, 'truth-alpha').click();
	expect(await orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);
	await expect(notice(page), 'a failed edit must surface unsaved feedback').toBeVisible();
	await expect(summary(page), 'mixed states must show both counts').toBeVisible();

	for (const theme of ['light', 'dark'] as const) {
		await page.getByRole('combobox', { name: 'Appearance', exact: true }).selectOption(theme);
		await expect(retryButton(page), `retry must stay visible at 320px in ${theme}`).toBeVisible();
		await expect(summary(page), `summary must stay visible at 320px in ${theme}`).toBeVisible();
		const box = await retryButton(page).boundingBox();
		expect(box, 'retry must have a touch target at 320px').not.toBeNull();
		expect(box!.width).toBeGreaterThanOrEqual(24);
		expect(box!.height).toBeGreaterThanOrEqual(24);
		const overflow = await page.evaluate(
			() => document.documentElement.scrollWidth - document.documentElement.clientWidth
		);
		expect(overflow, `no horizontal scrolling at 320px in ${theme} mode`).toBeLessThanOrEqual(1);
	}
});

test('[FAVTRUTH-E11] denied storage keeps starring usable in memory with recovery through retry', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT });
	await starButton(page, 'truth-zulu').click();
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu']);
	await expect(notice(page), 'no notice while writes succeed').toHaveCount(0);

	await faultLocalStorageGetter(page);
	await starButton(page, 'truth-alpha').click();
	await expect(
		notice(page),
		'an unavailable store must surface unsaved feedback'
	).toBeVisible();
	await expect(notice(page)).toContainText(UNSAVED_TEXT);
	await expect(starButton(page, 'truth-alpha')).toHaveAttribute('aria-pressed', 'true');
	expect(await orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);

	await restoreTruthGetter(page);
	await retryButton(page).click();
	await expect(notice(page), 'a confirmed retry clears the notice').toHaveCount(0);
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual([
		'truth-zulu',
		'truth-alpha'
	]);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);
});

test('[FAVTRUTH-E12] healthy writers never show a notice and truly zero keeps the honest empty state', async ({
	page
}) => {
	await openLibrary(page, { context: CONTEXT });
	await expect(page.getByTestId('favorites-empty')).toContainText(/no favorites yet/i);
	await expect(summary(page), 'truly zero favorites have no summary').toHaveCount(0);
	await expect(notice(page), 'no notice before any edit').toHaveCount(0);

	await starButton(page, 'truth-zulu').click();
	await starButton(page, 'truth-alpha').click();
	await expect.poll(() => orderedFavoriteIds(page)).toEqual(['truth-zulu', 'truth-alpha']);
	await expect(notice(page), 'no notice while writes succeed').toHaveCount(0);

	await moveButton(page, 'Alpha Song', 'up').click();
	expect(await orderedFavoriteIds(page)).toEqual(['truth-alpha', 'truth-zulu']);
	await expect(notice(page), 'no notice while writes succeed').toHaveCount(0);

	await shelfUnstar(page, 'Zulu Song').click();
	expect(await orderedFavoriteIds(page)).toEqual(['truth-alpha']);
	await expect(notice(page), 'no notice while writes succeed').toHaveCount(0);

	await section(page).getByRole('button', { name: 'Clear local favorites', exact: true }).click();
	await section(page).getByRole('button', { name: 'Yes, clear local favorites', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toContainText(/no favorites yet/i);
	await expect(notice(page), 'no notice while writes succeed').toHaveCount(0);
});
