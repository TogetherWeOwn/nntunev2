import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: suggest a displayed track without changing playback or the queue.
 *
 * OWNERSHIP: this file is acceptance-owned and protected. The paired build
 * lands the Suggest action on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the acceptance
 * lane instead. No other protected bank is modified by this file.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click/keyboard contract is pinned
 * here in the offline Playwright suite with a route-mocked `/api/library`.
 * Every test guards its first Suggest interaction with an explicit
 * button-count assertion, so a run against a build without the action fails
 * as an assertion (missing control), never as a click timeout. Fully
 * offline and deterministic: no backend, no provider, no spend. External
 * hosts are blocked and every non-library `/api/*` route 404s; only the
 * mocked `/api/library` boundary is allowed to succeed.
 *
 * Locked surface (the only new elements are the action and its result):
 * - One explicit action on the existing library browse surface, a `button`
 *   whose accessible name matches `/suggest a displayed track/i` (a count
 *   suffix is allowed). It sits beside the existing find/order/view
 *   controls, never inside a row, and never replaces the existing Add
 *   controls (those remain the only queue actions).
 * - One concise result region with `data-testid="library-suggestion"`,
 *   `aria-live="polite"`, and a `data-id` attribute carrying the suggested
 *   stable ID. Its text shows the suggested safe title (blank titles fall
 *   back to the stable ID, exactly like the rows) and the artist when the
 *   row shows one. No history list, no assertive region, no alert.
 * - The sampler chooses only a stable ID from the current visible rows
 *   (the rendered `[data-testid="library-item"]` set) after the current
 *   ordering, find query/scope and view projection. Deterministic
 *   randomness is injected transiently via `Math.random` override (never
 *   persisted): with `Math.random = () => 0` a working sampler over N
 *   visible rows must pick index 0 (first visible); `0.999...` must pick
 *   index N-1 (last visible); `0.5` must pick `floor(N/2)`. Non-finite or
 *   out-of-range draws (`NaN`, `Infinity`, `<0`, `>=1`) must fall back
 *   safely to a displayed track, never blank/undefined/crash.
 * - Immediate-repeat avoidance: two consecutive activations with the same
 *   forced draw must yield different stable IDs when at least two
 *   distinct visible IDs exist. No full no-repeat cycle or personalized
 *   recommendation is claimed; the copy never says recommended-for-you,
 *   never-repeat, full cycle, or history-based.
 * - Single visible row: the action always suggests that row (truthful even
 *   though avoidance cannot apply). Zero visible rows: the action is
 *   disabled and the result region explains (matching
 *   `/no displayed|no matches|empty/i`) instead of presenting a track.
 * - After each activation the suggested stable ID is re-resolved to its
 *   current row and focus lands on an actionable control inside that row
 *   (the existing row action), revealed into the viewport. Focusing never
 *   activates the row: no select/append, no current-track change, no
 *   filter change, no audio.
 * - Stale suggestions clear or invalidate: a successful snapshot that
 *   removes the suggested ID, an effective-filter change (find/scope/
 *   order/view) that hides it, and an auth transition all leave the result
 *   hidden/empty or marked (`/no longer|stale|cleared|changed/i`), never
 *   presenting the missing row as available. A same-account transient
 *   failure preserves the still-valid suggestion.
 * - Presentation-only: no checkbox/favorites/queue/current change, no
 *   storage write (no history key), no network fetch/mutation, no stream
 *   URL resolution (no `<audio>` appears from fixtures without URLs), no
 *   media `play`/`pause` calls. Repeated activations yield one concise
 *   result each (single region, polite only, text under 200 chars).
 *
 * Case IDs (stable): LS-00 control, LS-01 action presence and concise
 * result, LS-02 first-visible draw, LS-03 last-visible draw and safe blank
 * title, LS-04 middle draw, LS-05 non-finite/out-of-range safety, LS-06
 * immediate-repeat avoidance, LS-07 single-row truthful, LS-08 zero-rows
 * disabled/explains, LS-09 filters/Favorites/order composition, LS-10
 * duplicate-ID distinct handling, LS-11 stale invalidation and transient
 * preservation, LS-12 auth teardown, LS-13 presentation-only side-effect
 * audit, LS-14 focus without append and keyboard, LS-15 narrow light/dark
 * and reduced-motion, LS-16 single-result announcement discipline.
 *
 * Out of scope (not pinned): radio/autoplay, shuffle or queue arrangement,
 * personalized recommendations, new discovery page, sonic analysis,
 * listening history, persistence, provider/search calls. Queue/media
 * evidence is state invariance in the mocked page, not audible playback.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-track-suggestion.spec.ts --workers=1 --retries=0
 */

// Loaded (API) order is neither title order nor artist order. Titles and
// artists avoid control vocabulary so name locators stay exact. The blank
// title row pins safe display: rows fall back to the stable ID.
const ITEMS = [
	{ id: 'ls-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'ls-alpha', title: 'Alpha Song', artist: 'Solo Act' },
	{ id: 'ls-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'ls-bravo', title: 'Bravo Song', artist: 'Solo Act' },
	{ id: 'ls-blank', title: '   ' }
];
const ALL_IDS = ITEMS.map((item) => item.id);

const DUPLICATE_ROWS = [
	{ id: 'ls-dup-1', title: 'First Copy Song', artist: 'Dup Artist' },
	{ id: 'ls-dup-2', title: 'Second Song' },
	{ id: 'ls-dup-1', title: 'Conflicting Duplicate Title', artist: 'Other Artist' },
	{ id: 'ls-dup-2', title: 'Second Song Again' }
];

const NEXT_ACCOUNT = [{ id: 'ls-kilo', title: 'Kilo Song', artist: 'Next Account' }];

type Harness = {
	requests: string[];
	writes: string[];
	state: { abort: boolean };
};

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function suggestButton(page: Page): Locator {
	return page.getByRole('button', { name: /suggest a displayed track/i });
}

function suggestion(page: Page): Locator {
	return page.getByTestId('library-suggestion');
}

function showControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Show', exact: true });
}

function orderControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Order tracks', exact: true });
}

function findField(page: Page): Locator {
	return page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
}

function findScope(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Search in', exact: true });
}

async function openLibrary(
	page: Page,
	options: { payload?: { current: unknown }; randomSource?: string; expectedCount?: number } = {}
): Promise<Harness> {
	const payload = options.payload ?? { current: ITEMS };
	const expectedCount =
		options.expectedCount ?? (payload.current as unknown[]).length;
	const requests: string[] = [];
	const writes: string[] = [];
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (url.pathname === '/api/library') requests.push(`${request.method()} ${url.pathname}`);
		if (request.method() !== 'GET' && request.method() !== 'HEAD') {
			writes.push(`${request.method()} ${url.pathname}`);
		}
	});
	await page.addInitScript(() => {
		const storageWrites: string[] = [];
		const mediaCalls = { play: 0, pause: 0 };
		Object.assign(window, { __lsStorageWrites: storageWrites, __lsMedia: mediaCalls });
		const originalSet = Storage.prototype.setItem;
		Storage.prototype.setItem = function (this: Storage, key: string, value: string) {
			storageWrites.push(key);
			return originalSet.call(this, key, value);
		};
		const originalRemove = Storage.prototype.removeItem;
		Storage.prototype.removeItem = function (this: Storage, key: string) {
			storageWrites.push(key);
			return originalRemove.call(this, key);
		};
		const originalPlay = HTMLMediaElement.prototype.play;
		HTMLMediaElement.prototype.play = function (this: HTMLMediaElement) {
			mediaCalls.play += 1;
			return originalPlay.call(this);
		};
		const originalPause = HTMLMediaElement.prototype.pause;
		HTMLMediaElement.prototype.pause = function (this: HTMLMediaElement) {
			mediaCalls.pause += 1;
			return originalPause.call(this);
		};
	});
	if (options.randomSource !== undefined) {
		await page.addInitScript(`Math.random = () => ${options.randomSource};`);
	}
	const state = { abort: false };
	await page.route('**/api/library', (route) => {
		if (state.abort) return route.abort('failed');
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		});
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(expectedCount);
	return { requests, writes, state };
}

async function displayedIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('library-item')
		.evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}

async function expectDisplayed(page: Page, want: string[]) {
	await expect.poll(() => displayedIds(page)).toEqual(want);
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((rows) =>
			rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

async function queueSnapshot(page: Page) {
	return page.evaluate(() => ({
		rows: [...document.querySelectorAll('[data-testid="queue-item"]')].map((node) => ({
			id: node.getAttribute('data-track-id') ?? node.getAttribute('data-id'),
			current: node.getAttribute('aria-current')
		})),
		now: document.querySelector('[data-testid="now-playing"]')?.textContent,
		audio: document.querySelectorAll('audio, video').length,
		calls: Reflect.get(window, '__lsMedia') as { play: number; pause: number }
	}));
}

async function favoriteIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}

async function storageWriteCount(page: Page): Promise<number> {
	return page.evaluate(() => (Reflect.get(window, '__lsStorageWrites') as string[]).length);
}

async function focusedRow(page: Page): Promise<{ rowId: string | null; actionable: boolean }> {
	return page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		const holder = el?.closest('[data-testid="library-item"]');
		const actionable =
			el !== null &&
			(['INPUT', 'BUTTON', 'SELECT', 'A', 'TEXTAREA'].includes(el.tagName) ||
				el.getAttribute('role') === 'button' ||
				el.getAttribute('role') === 'option');
		return { rowId: holder?.getAttribute('data-id') ?? null, actionable };
	});
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-suggest-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

async function expectSuggestReady(page: Page): Promise<Locator> {
	const button = suggestButton(page);
	await expect(button, 'Suggest a displayed track action must exist once').toHaveCount(1);
	await expect(button).toBeVisible();
	return button;
}

async function suggestedId(page: Page): Promise<string | null> {
	const region = suggestion(page);
	await expect(region, 'suggestion result must appear once').toHaveCount(1);
	return region.getAttribute('data-id');
}

async function expectConciseSafeSuggestion(page: Page, visible: string[]) {
	const region = suggestion(page);
	await expect(region).toHaveCount(1);
	const id = await region.getAttribute('data-id');
	expect(id, 'suggestion must carry a displayed stable ID').not.toBeNull();
	expect(visible, 'suggestion must choose only a displayed ID').toContain(id);
	const text = (await region.textContent()) ?? '';
	expect(text.trim().length, 'suggestion must name the track').toBeGreaterThan(0);
	expect(text.length, 'suggestion must stay concise').toBeLessThan(200);
	expect(text, 'suggestion must never leak undefined/null').not.toMatch(/undefined|null|\[object/i);
	expect(
		text,
		'suggestion must not claim personalization or a full cycle'
	).not.toMatch(/for you|because you|never repeat|full cycle|all tracks heard|history-based/i);
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('[LS-00] control: fixtures, existing Add controls and row append already work', async ({
	page
}) => {
	const h = await openLibrary(page);
	expect(await displayedIds(page)).toEqual(ALL_IDS);
	await expect(page.getByTestId('library-find-count')).toContainText('Showing 5 of 5 tracks');

	const addDisplayed = page.getByRole('button', {
		name: 'Add displayed tracks to queue',
		exact: true
	});
	await expect(addDisplayed).toBeVisible();
	await expect(addDisplayed).toBeEnabled();

	// Row activation appends via the existing path (Suggest must never do this).
	await row(page, 'ls-zulu').getByRole('button').first().click();
	await row(page, 'ls-alpha').getByRole('button').first().click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['ls-zulu', 'ls-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	expect(h.requests).toEqual(['GET /api/library']);
	expect(h.writes).toEqual([]);
});

test('[LS-01] Suggest shows one concise displayed suggestion and queues nothing', async ({
	page
}) => {
	const h = await openLibrary(page, { randomSource: '0' });
	const beforeQueue = await queueSnapshot(page);

	const button = await expectSuggestReady(page);
	await expect(button).toBeEnabled();
	await button.click();

	await expectConciseSafeSuggestion(page, ALL_IDS);
	const id = await suggestedId(page);
	expect(id).toBe('ls-zulu');

	// No select/append, no current-track change, no audio, no refetch.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await queueSnapshot(page)).toEqual(beforeQueue);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	expect(h.requests).toEqual(['GET /api/library']);
	expect(h.writes).toEqual([]);
	await expect(findField(page)).toHaveValue('');
	await expect(orderControl(page)).toHaveValue('original');
	await expect(showControl(page).locator('option:checked')).toHaveText('All tracks');
});

test('[LS-02] forced zero draw suggests the first visible row', async ({ page }) => {
	await openLibrary(page, { randomSource: '0' });
	const button = await expectSuggestReady(page);
	await button.click();
	expect(await suggestedId(page)).toBe('ls-zulu');
	await expectConciseSafeSuggestion(page, ALL_IDS);
});

test('[LS-03] forced last draw suggests the last visible row with a safe title', async ({
	page
}) => {
	await openLibrary(page, { randomSource: '0.999999' });
	const button = await expectSuggestReady(page);
	await button.click();
	// Last in loaded order is the blank-title row: safe display falls back to ID.
	expect(await suggestedId(page)).toBe('ls-blank');
	const text = (await suggestion(page).textContent()) ?? '';
	expect(text, 'blank title must fall back to the stable ID').toContain('ls-blank');
	await expectConciseSafeSuggestion(page, ALL_IDS);
});

test('[LS-04] forced middle draw suggests the middle visible row', async ({ page }) => {
	await openLibrary(page, { randomSource: '0.5' });
	const button = await expectSuggestReady(page);
	await button.click();
	// floor(0.5 * 5) = 2 -> third row in loaded order.
	expect(await suggestedId(page)).toBe('ls-mike');
	await expectConciseSafeSuggestion(page, ALL_IDS);
});

test('[LS-05] non-finite and out-of-range draws still suggest a displayed track', async ({
	page,
	context
}) => {
	for (const source of ['NaN', 'Infinity', '-1', '1', '2']) {
		const probe = await context.newPage();
		try {
			await openLibrary(probe, { randomSource: source });
			const button = suggestButton(probe);
			await expect(button, `Suggest missing for draw ${source}`).toHaveCount(1);
			await button.click();
			await expectConciseSafeSuggestion(probe, ALL_IDS);
			const id = await suggestedId(probe);
			expect(ALL_IDS, `unsafe fallback for draw ${source}`).toContain(id);
			await expect(probe.getByTestId('queue-item')).toHaveCount(0);
		} finally {
			await probe.close();
		}
	}
});

test('[LS-06] the same forced draw does not repeat the immediately preceding suggestion', async ({
	page
}) => {
	await openLibrary(page, { randomSource: '0' });
	const button = await expectSuggestReady(page);

	await button.click();
	const first = await suggestedId(page);
	expect(first).toBe('ls-zulu');

	await button.click();
	const second = await suggestedId(page);
	expect(second, 'second activation must avoid the immediate repeat').not.toBe(first);
	expect(ALL_IDS).toContain(second);

	await button.click();
	const third = await suggestedId(page);
	expect(ALL_IDS).toContain(third);
	await expect(suggestion(page)).toHaveCount(1);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-07] a single visible row is always suggested truthfully', async ({ page }) => {
	await openLibrary(page, { randomSource: '0.999999' });
	await findField(page).fill('alpha');
	await expectDisplayed(page, ['ls-alpha']);

	const button = await expectSuggestReady(page);
	await expect(button).toBeEnabled();
	await button.click();
	expect(await suggestedId(page)).toBe('ls-alpha');
	await expect(suggestion(page)).toContainText('Alpha Song');

	// Repeat avoidance cannot apply with one row: the same truthful answer stays.
	await button.click();
	expect(await suggestedId(page)).toBe('ls-alpha');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-08] zero visible rows disables the action and explains instead', async ({
	page
}) => {
	await openLibrary(page);
	await findField(page).fill('zzz-no-such-track');
	await expectDisplayed(page, []);
	await expect(page.getByTestId('library-find-count')).toContainText('Showing 0 of 5 tracks');

	const button = suggestButton(page);
	await expect(button, 'Suggest must exist even with zero rows').toHaveCount(1);
	await expect(button).toBeDisabled();
	const region = suggestion(page);
	await expect(region, 'zero rows must explain the disabled action').toHaveCount(1);
	await expect(region).toContainText(/no displayed|no matches|empty/i);
	expect(await region.getAttribute('data-id')).toBeNull();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-09] Suggest respects ordering, find and Favorites and leaves filters untouched', async ({
	page
}) => {
	await openLibrary(page, { randomSource: '0' });
	// Star two Harbor Lights rows, then narrow to Favorites-only.
	await row(page, 'ls-zulu')
		.getByRole('button', { name: 'Star Zulu Song', exact: true })
		.click();
	await row(page, 'ls-mike').getByRole('button', { name: 'Star Mike Song', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await showControl(page).selectOption({ label: 'Favorites only' });
	await expectDisplayed(page, ['ls-zulu', 'ls-mike']);

	await orderControl(page).selectOption('title');
	await expectDisplayed(page, ['ls-mike', 'ls-zulu']);
	await findField(page).fill('harbor');
	await expectDisplayed(page, ['ls-mike', 'ls-zulu']);

	const button = await expectSuggestReady(page);
	await button.click();
	// First in title order among the filtered favorites.
	expect(await suggestedId(page)).toBe('ls-mike');
	await expectConciseSafeSuggestion(page, ['ls-mike', 'ls-zulu']);

	// Filters are untouched by suggesting.
	await expect(findField(page)).toHaveValue('harbor');
	await expect(findScope(page)).toHaveValue('both');
	await expect(orderControl(page)).toHaveValue('title');
	await expect(showControl(page).locator('option:checked')).toHaveText('Favorites only');

	// Narrowing to one favorite re-resolves to that row only.
	await findField(page).fill('zulu');
	await expectDisplayed(page, ['ls-zulu']);
	await button.click();
	expect(await suggestedId(page)).toBe('ls-zulu');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-10] duplicate stable IDs collapse to distinct suggestions', async ({ page }) => {
	const payload: { current: unknown } = { current: DUPLICATE_ROWS };
	const h = await openLibrary(page, { payload, randomSource: '0', expectedCount: 2 });
	// The shipped dedupe renders the two distinct rows only.
	await expectDisplayed(page, ['ls-dup-1', 'ls-dup-2']);

	const button = await expectSuggestReady(page);
	await button.click();
	expect(await suggestedId(page)).toBe('ls-dup-1');

	await button.click();
	expect(await suggestedId(page), 'two distinct IDs must allow avoidance').toBe('ls-dup-2');

	expect(h.requests).toEqual(['GET /api/library']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-11] stale suggestions invalidate on filter and refresh; transient failures preserve', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	const h = await openLibrary(page, { payload, randomSource: '0' });
	const button = await expectSuggestReady(page);

	await button.click();
	expect(await suggestedId(page)).toBe('ls-zulu');
	const firstText = ((await suggestion(page).textContent()) ?? '').trim();
	expect(firstText).toContain('Zulu Song');

	// Hiding the suggested row invalidates instead of presenting it as available.
	await findField(page).fill('alpha');
	await expectDisplayed(page, ['ls-alpha']);
	const region = suggestion(page);
	await expect
		.poll(async () => {
			if ((await region.count()) === 0) return 'cleared';
			const current = await region.getAttribute('data-id');
			if (current === null || current === 'ls-alpha') return 'cleared-or-current';
			return `stale:${((await region.textContent()) ?? '').trim()}`;
		})
		.not.toMatch(/^stale:/);
	if ((await region.count()) === 1) {
		const text = ((await region.textContent()) ?? '').trim();
		if (text.includes('Zulu Song')) {
			expect(text).toMatch(/no longer|stale|cleared|changed/i);
		}
	}

	// A successful snapshot that removes the suggestion clears it too.
	await findField(page).fill('');
	await expectDisplayed(page, ALL_IDS);
	await button.click();
	const suggested = await suggestedId(page);
	expect(ALL_IDS).toContain(suggested);
	payload.current = ITEMS.filter((item) => item.id !== suggested);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectDisplayed(page, ALL_IDS.filter((id) => id !== suggested));
	await expect
		.poll(async () => {
			if ((await region.count()) === 0) return 'cleared';
			const current = await region.getAttribute('data-id');
			if (current === null || current !== suggested) return 'cleared-or-replaced';
			return `stale:${((await region.textContent()) ?? '').trim()}`;
		})
		.not.toMatch(/^stale:/);

	// A transient failure keeps the still-valid suggestion (no missing row shown).
	payload.current = ITEMS;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectDisplayed(page, ALL_IDS);
	await button.click();
	const preserved = await suggestedId(page);
	h.state.abort = true;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expectDisplayed(page, ALL_IDS);
	expect(await suggestedId(page), 'transient failure must preserve the valid suggestion').toBe(
		preserved
	);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-12] an auth transition clears the suggestion for the next account', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await openLibrary(page, { payload, randomSource: '0' });
	const button = await expectSuggestReady(page);
	await button.click();
	expect(await suggestedId(page)).toBe('ls-zulu');

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	const region = suggestion(page);
	if ((await region.count()) === 1) {
		const text = ((await region.textContent()) ?? '').trim();
		expect(
			text === '' || /no longer|stale|cleared|changed|session/i.test(text),
			'auth boundary must not leave the old suggestion available'
		).toBe(true);
	}

	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry' }).click();
	await expectDisplayed(page, ['ls-kilo']);
	await expect(button).toBeEnabled();
	await button.click();
	expect(await suggestedId(page)).toBe('ls-kilo');
	await expect(suggestion(page)).toContainText('Kilo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-13] Suggest touches no queue, favorites, selection, storage, network or media', async ({
	page
}) => {
	const h = await openLibrary(page, { randomSource: '0' });

	// Establish real state to protect: a two-track queue played forward, one
	// favorite, and a two-row checkbox selection.
	await row(page, 'ls-zulu').getByRole('button').first().click();
	await row(page, 'ls-bravo').getByRole('button').first().click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Bravo Song');
	await row(page, 'ls-mike').getByRole('button', { name: 'Star Mike Song', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await row(page, 'ls-alpha').getByRole('checkbox').check();
	await row(page, 'ls-mike').getByRole('checkbox').check();
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');

	const beforeQueue = await queueSnapshot(page);
	const beforeFavorites = await favoriteIds(page);
	const beforeRequests = [...h.requests];
	const beforeWrites = [...h.writes];
	const beforeStorage = await storageWriteCount(page);

	const button = await expectSuggestReady(page);
	await button.click();
	await expectConciseSafeSuggestion(page, ALL_IDS);
	await button.click();
	await expectConciseSafeSuggestion(page, ALL_IDS);

	expect(await queueSnapshot(page), 'Suggest must not touch queue/current/media').toEqual(
		beforeQueue
	);
	expect(await favoriteIds(page), 'Suggest must not touch favorites').toEqual(beforeFavorites);
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');
	await expect(row(page, 'ls-alpha').getByRole('checkbox')).toBeChecked();
	await expect(row(page, 'ls-mike').getByRole('checkbox')).toBeChecked();
	expect(h.requests, 'Suggest must not refetch the library').toEqual(beforeRequests);
	expect(h.writes, 'Suggest must not mutate over the network').toEqual(beforeWrites);
	expect(await storageWriteCount(page), 'Suggest must not write storage').toBe(beforeStorage);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.locator('main').locator('[aria-live="assertive"]')).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
});

test('[LS-14] Suggest focuses the suggested row action without appending; keyboard works', async ({
	page
}) => {
	await openLibrary(page, { randomSource: '0' });
	const button = await expectSuggestReady(page);

	await button.click();
	const first = await suggestedId(page);
	expect(first).toBe('ls-zulu');
	await expect.poll(() => focusedRow(page)).toEqual({ rowId: 'ls-zulu', actionable: true });
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Reordering re-resolves the stable ID before focusing (no stale index focus).
	// The same forced draw would sample zulu again, so avoidance must move on.
	await orderControl(page).selectOption('title-desc');
	await expectDisplayed(page, ['ls-zulu', 'ls-mike', 'ls-blank', 'ls-bravo', 'ls-alpha']);
	await button.click();
	const second = await suggestedId(page);
	expect(second, 'reordered Suggest must avoid the immediate repeat').not.toBe(first);
	expect(ALL_IDS).toContain(second);
	await expect.poll(() => focusedRow(page)).toEqual({ rowId: second, actionable: true });
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Keyboard activation focuses the suggested row action visibly.
	await button.focus();
	await expect(button).toBeFocused();
	await page.keyboard.press('Enter');
	const third = await suggestedId(page);
	expect(ALL_IDS).toContain(third);
	expect(third, 'keyboard Suggest must avoid the immediate repeat').not.toBe(second);
	await expect.poll(() => focusedRow(page)).toEqual({ rowId: third, actionable: true });
	const visibleFocus = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return el !== null && el.matches(':focus-visible') && el.isConnected;
	});
	expect(visibleFocus, 'keyboard Suggest must leave a visible focus').toBe(true);

	await button.focus();
	await page.keyboard.press('Space');
	const fourth = await suggestedId(page);
	expect(ALL_IDS).toContain(fourth);
	expect(fourth, 'second keyboard Suggest must avoid the immediate repeat').not.toBe(third);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-15] at 320px in light and dark with reduced motion, Suggest stays usable', async ({
	page
}) => {
	await page.setViewportSize({ width: 320, height: 700 });
	await openLibrary(page, { randomSource: '0' });

	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });
		const button = suggestButton(page);
		await expect(button, `Suggest missing at 320px (${scheme})`).toHaveCount(1);
		await expect(button).toBeVisible();
		await expect(button).toBeEnabled();
		await button.click();
		await expectConciseSafeSuggestion(page, ALL_IDS);
		const region = suggestion(page);
		await expect(region).toBeVisible();
		for (const [name, control] of [
			['Suggest', button],
			['suggestion', region],
			['suggested row', row(page, (await suggestedId(page)) ?? 'ls-zulu')]
		] as const) {
			const fits = await control.evaluate((element) => {
				const rect = element.getBoundingClientRect();
				return rect.left >= -1 && rect.right <= window.innerWidth + 1;
			});
			expect(fits, `${name} must fit 320px in ${scheme} mode`).toBe(true);
		}
		await expect.poll(() => focusedRow(page)).toEqual({
			rowId: await suggestedId(page),
			actionable: true
		});
	}

	await page.emulateMedia({ reducedMotion: 'reduce' });
	const button = suggestButton(page);
	await button.click();
	await expectConciseSafeSuggestion(page, ALL_IDS);
	await expect.poll(() => focusedRow(page)).toEqual({
		rowId: await suggestedId(page),
		actionable: true
	});
	await expect(suggestion(page)).toBeVisible();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LS-16] repeated Suggestions keep a single polite concise result', async ({ page }) => {
	await openLibrary(page, { randomSource: '0' });
	const button = await expectSuggestReady(page);

	await button.click();
	await button.click();
	await button.click();

	const region = suggestion(page);
	await expect(region).toHaveCount(1);
	await expect(region).toHaveAttribute('aria-live', 'polite');
	const text = ((await region.textContent()) ?? '').trim();
	expect(text.length, 'repeated Suggestions must stay concise').toBeLessThan(200);
	expect(text).not.toMatch(/undefined|null|\[object/i);
	await expect(page.locator('main').locator('[aria-live="assertive"]')).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});
