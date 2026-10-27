import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: library recently-played list from actual playback (browser half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * library recents wiring on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance card instead.
 *
 * Surface: `/library` with a route-mocked `/api/library` whose items carry
 * synthetic `streamUrl` paths, so the shipped row→queue→audio wiring
 * renders the native player exactly as the streaming slice will. Media
 * loading is held pending (route-held, never fulfilled) and `play()` is
 * stubbed to resolve, so flows are driven by synthetic media events with
 * no network beyond the local preview server. Only the native transport
 * boundary is mocked; queue/library/storage/session code stays real.
 *
 * Contract seams the build implements:
 * - `library-recently-played`: the section, always rendered inside the
 *   loaded library view after `library-list`, headed `Recently played`,
 *   never mentioning demo tracks. Its copy documents the queue semantics
 *   (matches /queue/i): clearing the queue never wipes history.
 * - `library-recently-played-empty`: honest empty state, visible iff the
 *   list is empty, with the exact text `Nothing played yet this visit`.
 * - `library-recent-item[data-track-id]`: one per entry,
 *   most-recent-first, showing the library track title.
 * - `library-recent-played-at-{id}`: non-empty relative played-at label.
 * - `library-recent-replay-{id}`: replay action per entry (accessible name
 *   matches /replay/i); activating it makes the track the queue current
 *   (now-playing names it, appending first when absent, never duplicating
 *   the queue) and moves the entry to the front with a refreshed
 *   timestamp. Selection-only: no play/pause/seek/load calls.
 * - `library-recent-forget-{id}`: forget action per entry (accessible name
 *   matches /forget/i); removes exactly that id, preserves the remaining
 *   order and timestamps, persists, and keeps focus on a surviving
 *   enabled control inside the section — never the body.
 * - `library-recently-played-clear`: explicit Clear (accessible name
 *   matches /clear/i); empties the list, persists `[]`, keeps the queue
 *   and transport untouched, and keeps focus inside the section.
 * - Storage: sessionStorage under `nntune:library-recently-played:v1`
 *   holds a JSON array of `{ trackId, playedAt }` (the unit half pins the
 *   exact shape, cap, bounds and corrupt fallback in
 *   `src/lib/library/libraryRecentlyPlayed.acceptance.test.ts`).
 *   localStorage never holds the key; the demo key is never touched.
 * - Announcements: exactly one non-empty polite live message in the
 *   section per record/replay/forget/clear; zero assertive regions.
 *   Selections, queue edits, pause and progress never chatter.
 *
 * What's pinned: record ONLY on actual `playing` for the current source
 * (proof of listening); move-to-front on replay and on re-play; cap
 * eviction at the 11th distinct track; non-recording for row selection,
 * queue adds, Next/direct-select without `playing`, `play()` that never
 * fires `playing`, pause/end/error, stale-source and duplicate rapid
 * `playing`; forget-per-row exact-match; clear-all; truthful empty state;
 * session-only namespaced key; bounded/oversized/denied/corrupt handling;
 * auth-transition/account-switch/sign-out/expired-session purge; keyboard
 * focus recovery; narrow light/dark layout; zero history-driven backend
 * calls.
 *
 * Locked decisions (so the builder never guesses):
 * - Selection is never proof of listening: queueing, starring, finding,
 *   Next and direct-select record nothing without a `playing` event.
 * - A `playing` event whose element source no longer matches the current
 *   track's stream URL records nothing (stale guard).
 * - A repeat `playing` for the already-front track never duplicates; with
 *   the clock fixed the entries stay deep-equal.
 * - Pause/end/error record nothing and never refresh timestamps.
 * - Unknown history ids are explained as unavailable, never substituted
 *   with another track, and never block the known rows.
 * - Purge clears stored bytes at every auth boundary (cross-tab
 *   transition, account switch, sign-out, expired session); the list
 *   never leaks into the next account.
 *
 * Out of scope (not pinned): cross-visit persistence, play counts,
 * most-played/stats, recently-added, selection history, queue restore,
 * backend sync, telemetry, exact relative-label wording (non-empty only),
 * demo player changes, real streams or providers.
 *
 * CASE INDEX (stable IDs; LR-U-* live in the unit acceptance file):
 * LR-B-01 empty state, LR-B-02 record on playing, LR-B-03 re-play
 * move-to-front, LR-B-04 cap eviction, LR-B-05 selection-only,
 * LR-B-06 queue edits, LR-B-07 play-without-playing, LR-B-08
 * pause/end/error, LR-B-09 stale source, LR-B-10 duplicate playing,
 * LR-B-11 replay, LR-B-12 replay keyboard, LR-B-13 replay re-queues,
 * LR-B-14 forget row, LR-B-15 forget last, LR-B-16 Clear, LR-B-17 unknown
 * id, LR-B-18 session-only, LR-B-19 oversized, LR-B-20 corrupt, LR-B-21
 * denied storage, LR-B-22 auth-transition purge, LR-B-23 account switch,
 * LR-B-24 sign-out purge, LR-B-25 expired-session purge, LR-B-26
 * announcements, LR-B-27 narrow light/dark, LR-B-28 zero backend calls.
 */

const LIBRARY_KEY = 'nntune:library-recently-played:v1';
const DEMO_KEY = 'nntune:demo-recently-played:v1';
const TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const AUDIO_PREFIX = '/__library_audio__';
const EMPTY_TEXT = 'Nothing played yet this visit';
const T0 = new Date('2026-01-01T12:00:00Z');
const T1 = new Date('2026-01-01T12:01:00Z');

type Entry = { trackId: string; playedAt: number };
type LibraryItem = { id: string; title: string; artist?: string; streamUrl: string };

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS: LibraryItem[] = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist', streamUrl: `${AUDIO_PREFIX}/row-zulu.wav` },
	{ id: 'row-alpha', title: 'Alpha Song', artist: 'Alpha Band', streamUrl: `${AUDIO_PREFIX}/row-alpha.wav` },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Mike Band', streamUrl: `${AUDIO_PREFIX}/row-mike.wav` }
];

type Probe = {
	blocked: 'none' | 'writes' | 'all';
	plays: number;
	pauses: number;
	loads: number;
	seeks: number;
	sourceChanges: number;
	read: (key: string) => string | null;
};

function rowAction(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`);
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
}

function queueSelect(page: Page, id: string): Locator {
	return queueItem(page, id).getByRole('button', { name: /^Select / });
}

function section(page: Page): Locator {
	return page.getByTestId('library-recently-played');
}

function replayAction(page: Page, id: string): Locator {
	return page.getByTestId(`library-recent-replay-${id}`);
}

function forgetAction(page: Page, id: string): Locator {
	return page.getByTestId(`library-recent-forget-${id}`);
}

async function recentIds(page: Page): Promise<string[]> {
	return page.getByTestId('library-recent-item').evaluateAll((items) =>
		items.map((item) => item.getAttribute('data-track-id') ?? '')
	);
}

async function storedEntries(page: Page): Promise<Entry[]> {
	return page.evaluate((key) => {
		const probe = (window as unknown as { __libraryRecent: Probe }).__libraryRecent;
		const raw = probe.read(key);
		return raw === null ? [] : (JSON.parse(raw) as Entry[]);
	}, LIBRARY_KEY);
}

async function storedRaw(page: Page, key: string): Promise<string | null> {
	return page.evaluate(
		({ probeKey }) =>
			(window as unknown as { __libraryRecent: Probe }).__libraryRecent.read(probeKey),
		{ probeKey: key }
	);
}

async function audioSrc(page: Page): Promise<string | null> {
	return page.getByTestId('audio-player').getAttribute('src');
}

async function liveMessages(page: Page): Promise<string[]> {
	return section(page)
		.locator('[role="status"], [aria-live="polite"]')
		.evaluateAll((regions) =>
			regions.map((node) => node.textContent?.trim() ?? '').filter(Boolean)
		);
}

async function transport(page: Page) {
	return page.evaluate(() => {
		const probe = (window as unknown as { __libraryRecent: Probe }).__libraryRecent;
		const audio = document.querySelector<HTMLAudioElement>('[data-testid="audio-player"]');
		return {
			plays: probe.plays,
			pauses: probe.pauses,
			loads: probe.loads,
			seeks: probe.seeks,
			src: audio?.getAttribute('src') ?? null,
			time: audio?.currentTime ?? null
		};
	});
}

/** Synthetic proof of listening on the current native element. */
async function dispatchPlaying(page: Page): Promise<void> {
	await page.getByTestId('audio-player').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: false, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
}

/** Queue a row (first queue also selects); the caller asserts history. */
async function queueRow(page: Page, id: string): Promise<void> {
	await rowAction(page, id).click();
}

async function installProbe(page: Page, history?: Entry[] | string): Promise<void> {
	await page.addInitScript(
		({ key, seed, hasSeed }) => {
			const session = sessionStorage;
			const nativeGet = Storage.prototype.getItem;
			const nativeSet = Storage.prototype.setItem;
			if (hasSeed) {
				nativeSet.call(session, key, typeof seed === 'string' ? seed : JSON.stringify(seed));
			}
			const probe: Probe = {
				blocked: 'none',
				plays: 0,
				pauses: 0,
				loads: 0,
				seeks: 0,
				sourceChanges: 0,
				read: (k) => nativeGet.call(session, k)
			};
			Object.assign(window, { __libraryRecent: probe });
			new MutationObserver((changes) => {
				probe.sourceChanges += changes.filter(
					(change) => change.target instanceof HTMLMediaElement
				).length;
			}).observe(document, { subtree: true, attributes: true, attributeFilter: ['src'] });
			Storage.prototype.getItem = function (k) {
				if (this === session && probe.blocked === 'all') throw new Error('synthetic storage denial');
				return nativeGet.call(this, k);
			};
			Storage.prototype.setItem = function (k, v) {
				if (this === session && probe.blocked !== 'none') throw new Error('synthetic storage denial');
				return nativeSet.call(this, k, v);
			};
			const time = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime')!;
			Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
				...time,
				set(value: number) {
					probe.seeks += 1;
					time.set!.call(this, value);
				}
			});
			HTMLMediaElement.prototype.play = function () {
				probe.plays += 1;
				return Promise.resolve();
			};
			HTMLMediaElement.prototype.pause = function () {
				probe.pauses += 1;
			};
			HTMLMediaElement.prototype.load = function () {
				probe.loads += 1;
			};
		},
		{ key: LIBRARY_KEY, seed: history, hasSeed: history !== undefined }
	);
}

type LibraryReply = { status: number; body: string };

async function routeStrict(page: Page, reply: () => LibraryReply): Promise<void> {
	const calls: string[] = [];
	backendCalls.set(page, calls);
	let localOrigin: string | null = null;
	await page.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		localOrigin ??= url.origin;
		if (url.origin !== localOrigin) {
			calls.push(`external ${request.method()} ${url.pathname}`);
			await route.abort();
		} else if (url.pathname === '/api/library' && request.method() === 'GET') {
			const current = reply();
			await route.fulfill({
				status: current.status,
				contentType: 'application/json',
				body: current.body
			});
		} else if (url.pathname.startsWith(`${AUDIO_PREFIX}/`)) {
			// Pending native media request: no remote provider and no garbage bytes.
		} else if (request.method() !== 'GET' || ['xhr', 'fetch'].includes(request.resourceType())) {
			calls.push(`${request.method()} ${url.pathname}`);
			await route.abort();
		} else {
			await route.fallback();
		}
	});
}

async function open(
	page: Page,
	options: { items?: LibraryItem[]; history?: Entry[] | string; clock?: Date } = {}
): Promise<void> {
	const items = options.items ?? ITEMS;
	await routeStrict(page, () => ({ status: 200, body: JSON.stringify(items) }));
	if (options.clock) await page.clock.setFixedTime(options.clock);
	await installProbe(page, options.history);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
}

const backendCalls = new WeakMap<Page, string[]>();
test.afterEach(async ({ page }) => {
	expect(backendCalls.get(page) ?? [], 'no history-driven backend calls').toEqual([]);
});

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, TRANSITION_KEY);
	await otherTab.close();
}

async function focusInsideSection(page: Page): Promise<boolean> {
	return section(page).evaluate((el) => {
		const focused = document.activeElement;
		return focused instanceof HTMLElement && focused !== document.body && el.contains(focused);
	});
}

test('LR-B-01 fresh load shows the truthful empty state in a section after the browse list', async ({
	page
}) => {
	await open(page);

	const target = section(page);
	await expect(target).toBeVisible();
	await expect(target.getByRole('heading')).toContainText('Recently played');
	await expect(target).not.toContainText(/demo/i);

	const empty = page.getByTestId('library-recently-played-empty');
	await expect(empty).toBeVisible();
	await expect(empty).toHaveText(EMPTY_TEXT);

	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	expect(await storedEntries(page)).toEqual([]);

	const order = await page.evaluate(() => {
		const list = document.querySelector('[data-testid="library-list"]');
		const recents = document.querySelector('[data-testid="library-recently-played"]');
		if (!list || !recents) return 'missing';
		return list.compareDocumentPosition(recents) & Node.DOCUMENT_POSITION_FOLLOWING
			? 'after'
			: 'before';
	});
	expect(order).toBe('after');
});

test('LR-B-02 actual playing records the current track with title and played-at', async ({
	page
}) => {
	await open(page, { clock: T0 });

	await queueRow(page, 'row-alpha');
	await expect(page.getByTestId('audio-player')).toBeVisible();
	expect(await audioSrc(page)).toBe(`${AUDIO_PREFIX}/row-alpha.wav`);

	await dispatchPlaying(page);

	await expect(page.getByTestId('library-recent-item')).toHaveCount(1);
	const item = page.getByTestId('library-recent-item').first();
	await expect(item).toHaveAttribute('data-track-id', 'row-alpha');
	await expect(item).toContainText('Alpha Song');
	const playedAt = page.getByTestId('library-recent-played-at-row-alpha');
	await expect(playedAt).toBeVisible();
	expect(((await playedAt.textContent()) ?? '').trim().length).toBeGreaterThan(0);
	await expect(page.getByTestId('library-recently-played-empty')).toBeHidden();
	expect(await storedEntries(page)).toEqual([
		{ trackId: 'row-alpha', playedAt: T0.getTime() }
	]);
});

test('LR-B-03 re-playing a listed track moves it to the front without duplicating', async ({
	page
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect(page.getByTestId('library-recent-item')).toHaveCount(1);

	await queueRow(page, 'row-mike');
	await page.getByTestId('player-next').click();
	expect(await audioSrc(page)).toBe(`${AUDIO_PREFIX}/row-mike.wav`);
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);

	await queueSelect(page, 'row-alpha').click();
	expect(await audioSrc(page)).toBe(`${AUDIO_PREFIX}/row-alpha.wav`);
	await dispatchPlaying(page);

	await expect.poll(() => recentIds(page)).toEqual(['row-alpha', 'row-mike']);
	expect(await storedEntries(page)).toHaveLength(2);
});

test('LR-B-04 the 11th distinct track evicts the oldest, keeping 10', async ({ page }) => {
	const tracks = Array.from({ length: 12 }, (_, i) => {
		const id = `cap-${String(i + 1).padStart(2, '0')}`;
		return { id, title: `Cap Song ${i + 1}`, artist: 'Cap Band', streamUrl: `${AUDIO_PREFIX}/${id}.wav` };
	});
	// Ten seeded entries WITHOUT cap-11, so the first cap-11 play is an
	// 11th distinct track and must evict the oldest (cap-01).
	const seeded: Entry[] = Array.from({ length: 10 }, (_, i) => ({
		trackId: `cap-${String(10 - i).padStart(2, '0')}`,
		playedAt: 1010 - i
	}));
	await open(page, { items: tracks, history: seeded });
	await expect.poll(() => recentIds(page)).toEqual(seeded.map((entry) => entry.trackId));

	await queueRow(page, 'cap-11');
	await dispatchPlaying(page);

	const stored = await storedEntries(page);
	expect(stored).toHaveLength(10);
	expect(stored[0]?.trackId).toBe('cap-11');
	expect(stored.some((entry) => entry.trackId === 'cap-01')).toBe(false);

	const ids = await recentIds(page);
	expect(ids).toHaveLength(10);
	expect(ids[0]).toBe('cap-11');
	expect(new Set(ids).size).toBe(ids.length);
});

test('LR-B-05 selecting rows without playback records nothing', async ({ page }) => {
	await open(page);

	await queueRow(page, 'row-mike');
	await queueRow(page, 'row-zulu');
	await queueRow(page, 'row-alpha');

	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	await expect(page.getByTestId('library-recently-played-empty')).toBeVisible();
	expect(await storedEntries(page)).toEqual([]);
});

test('LR-B-06 queue edits without playback record nothing', async ({ page }) => {
	await open(page, { clock: T0 });
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);

	await queueRow(page, 'row-mike');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await queueSelect(page, 'row-alpha').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	expect(await recentIds(page)).toEqual(['row-alpha']);
	expect(await storedEntries(page)).toEqual([{ trackId: 'row-alpha', playedAt: T0.getTime() }]);
});

test('LR-B-07 play() that never fires playing records nothing', async ({ page }) => {
	await open(page, { clock: T0 });
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);

	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => void el.play());

	expect(await recentIds(page)).toEqual(['row-alpha']);
	expect(await storedEntries(page)).toEqual([{ trackId: 'row-alpha', playedAt: T0.getTime() }]);
});

test('LR-B-08 pause, end and error record nothing and never refresh timestamps', async ({
	page
}) => {
	await open(page, { clock: T0 });
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	const recorded = [{ trackId: 'row-alpha', playedAt: T0.getTime() }];
	await expect.poll(() => storedEntries(page)).toEqual(recorded);

	await page.getByTestId('audio-player').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
	});
	expect(await storedEntries(page)).toEqual(recorded);

	await page.getByTestId('audio-player').evaluate((el) => {
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('ended'));
	});
	expect(await storedEntries(page)).toEqual(recorded);

	await page.getByTestId('audio-player').evaluate((el) => {
		el.dispatchEvent(new Event('error'));
	});
	expect(await recentIds(page)).toEqual(['row-alpha']);
	expect(await storedEntries(page)).toEqual(recorded);
});

test('LR-B-09 a playing event for a stale source records nothing', async ({ page }) => {
	await open(page, { clock: T0 });
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	const recorded = [{ trackId: 'row-alpha', playedAt: T0.getTime() }];
	await expect.poll(() => storedEntries(page)).toEqual(recorded);

	// The element now points at a source that is not the current track's.
	await page.getByTestId('audio-player').evaluate((el, staleUrl) => {
		el.setAttribute('src', staleUrl);
		Object.defineProperty(el, 'paused', { value: false, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('playing'));
	}, `${AUDIO_PREFIX}/stale.wav`);
	expect(await recentIds(page)).toEqual(['row-alpha']);
	expect(await storedEntries(page)).toEqual(recorded);

	// Recovery: a genuine source change records its own track.
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	expect(await audioSrc(page)).toBe(`${AUDIO_PREFIX}/row-mike.wav`);
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);
});

test('LR-B-10 duplicate rapid playing events never duplicate the entry', async ({ page }) => {
	await open(page, { clock: T0 });
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	const recorded = [{ trackId: 'row-alpha', playedAt: T0.getTime() }];
	await expect.poll(() => storedEntries(page)).toEqual(recorded);

	await dispatchPlaying(page);
	expect(await storedEntries(page)).toEqual(recorded);

	await page.clock.setFixedTime(T1);
	await dispatchPlaying(page);
	expect(await recentIds(page)).toEqual(['row-alpha']);
	expect(await storedEntries(page)).toHaveLength(1);
});

test('LR-B-11 replay selects the track and moves it to the front without duplicating', async ({
	page
}) => {
	await open(page, { clock: T0 });
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);

	await expect(replayAction(page, 'row-alpha')).toHaveAccessibleName(/replay/i);
	const before = await transport(page);
	await page.clock.setFixedTime(T1);

	await replayAction(page, 'row-alpha').click();

	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha', 'row-mike']);
	expect(await storedEntries(page)).toEqual([
		{ trackId: 'row-alpha', playedAt: T1.getTime() },
		{ trackId: 'row-mike', playedAt: T0.getTime() }
	]);
	// Selection-only replay: the queue arrangement and transport hold still.
	const after = await transport(page);
	expect({ ...after, src: null }).toEqual({ ...before, src: null });
	expect(after.src).toBe(`${AUDIO_PREFIX}/row-alpha.wav`);
	expect(await queueItem(page, 'row-alpha').getAttribute('aria-current')).toBe('true');
	expect(await page.getByTestId('queue-item').count()).toBe(2);
});

test('LR-B-12 replay is keyboard operable', async ({ page }) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);

	await replayAction(page, 'row-alpha').focus();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha', 'row-mike']);
});

test('LR-B-13 clearing the queue keeps history; replay re-queues a missing track', async ({
	page
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);

	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);
	// Explicit Clear only: the section documents the queue semantics.
	await expect(section(page)).toContainText(/queue/i);

	await replayAction(page, 'row-alpha').click();

	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha', 'row-mike']);
	expect(await page.getByTestId('queue-item').count()).toBe(1);
	expect(await audioSrc(page)).toBe(`${AUDIO_PREFIX}/row-alpha.wav`);
});

test('LR-B-14 forget removes exactly its row and survives reload', async ({ page }) => {
	await open(page, { clock: T0 });
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await page.clock.setFixedTime(T1);
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);

	await expect(forgetAction(page, 'row-mike')).toHaveAccessibleName(/forget/i);
	const before = await transport(page);

	await forgetAction(page, 'row-mike').click();

	const expected = [{ trackId: 'row-alpha', playedAt: T0.getTime() }];
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);
	expect(await storedEntries(page)).toEqual(expected);
	expect(await transport(page)).toEqual(before);
	expect(await focusInsideSection(page), 'focus stays inside the section').toBe(true);

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);
	expect(await storedEntries(page)).toEqual(expected);
});

test('LR-B-15 forgetting the last row yields the truthful empty state with focus kept', async ({
	page
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);

	await forgetAction(page, 'row-alpha').focus();
	await page.keyboard.press('Space');

	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	await expect(page.getByTestId('library-recently-played-empty')).toHaveText(EMPTY_TEXT);
	expect(await storedEntries(page)).toEqual([]);
	expect(await focusInsideSection(page)).toBe(true);
});

test('LR-B-16 Clear empties the list, persists [] and leaves the queue playing', async ({
	page
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);
	const before = await transport(page);

	await expect(page.getByTestId('library-recently-played-clear')).toHaveAccessibleName(/clear/i);
	await page.getByTestId('library-recently-played-clear').click();

	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	await expect(page.getByTestId('library-recently-played-empty')).toBeVisible();
	expect(await storedRaw(page, LIBRARY_KEY)).toBe('[]');
	expect(await transport(page)).toEqual(before);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await focusInsideSection(page)).toBe(true);
});

test('LR-B-17 an unrecognized id is explained and never resolves to another track', async ({
	page
}) => {
	await open(page, {
		history: [
			{ trackId: 'ghost-track', playedAt: 400 },
			{ trackId: 'row-alpha', playedAt: 100 }
		]
	});
	const before = await transport(page);

	await expect(section(page)).toContainText(/unavailable|unrecognized|unknown|not available/i);
	expect(await storedEntries(page)).toEqual([
		{ trackId: 'ghost-track', playedAt: 400 },
		{ trackId: 'row-alpha', playedAt: 100 }
	]);
	expect(await transport(page)).toEqual(before);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await recentIds(page)).not.toContain('row-mike');
});

test('LR-B-18 history is session-only under the library key and never touches demo state', async ({
	page,
	context
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);

	expect(await storedRaw(page, LIBRARY_KEY)).not.toBeNull();
	expect(await page.evaluate((key) => localStorage.getItem(key), LIBRARY_KEY)).toBeNull();
	expect(await storedRaw(page, DEMO_KEY)).toBeNull();
	expect(await page.evaluate((key) => localStorage.getItem(key), DEMO_KEY)).toBeNull();

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);

	const other = await context.newPage();
	try {
		await routeStrict(other, () => ({ status: 200, body: JSON.stringify(ITEMS) }));
		await installProbe(other);
		await other.goto('/library');
		await expect(other.getByTestId('library-item')).toHaveCount(ITEMS.length);
		await expect(other.getByTestId('library-recently-played-empty')).toBeVisible();
		await expect(other.getByTestId('library-recent-item')).toHaveCount(0);
	} finally {
		await other.close();
	}
});

test('LR-B-19 an oversized payload falls back to empty with the page usable', async ({ page }) => {
	const valid = JSON.stringify([{ trackId: 'row-alpha', playedAt: 100 }]);
	const oversized = valid + ' '.repeat(65_537 - valid.length);
	await open(page, { history: oversized });

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('library-recently-played-empty')).toBeVisible();
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);

	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);
});

test('LR-B-20 corrupt storage falls back to empty with the page usable', async ({ page }) => {
	await open(page, { history: '{oops' });

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('library-recently-played-empty')).toBeVisible();
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);

	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);
});

test('LR-B-21 denied session storage cannot undo the current-visit list', async ({ page }) => {
	const seeded: Entry[] = [
		{ trackId: 'row-mike', playedAt: 200 },
		{ trackId: 'row-alpha', playedAt: 100 }
	];
	await open(page, { history: seeded });
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);
	const before = await transport(page);

	await page.evaluate(() => {
		(window as unknown as { __libraryRecent: Probe }).__libraryRecent.blocked = 'writes';
	});
	await forgetAction(page, 'row-mike').click();
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);
	// Quota denial is real: persisted bytes remain unchanged.
	expect(await storedEntries(page)).toEqual(seeded);

	await page.evaluate(() => {
		(window as unknown as { __libraryRecent: Probe }).__libraryRecent.blocked = 'all';
	});
	await forgetAction(page, 'row-alpha').click();
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	expect(await storedEntries(page)).toEqual(seeded);
	expect({ ...(await transport(page)), src: null }).toEqual({ ...before, src: null });
});

test('LR-B-22 an auth transition purges history with the queue and never leaks it back', async ({
	page
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);

	await fireAuthTransition(page);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	expect(await storedEntries(page)).toEqual([]);

	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('library-recently-played-empty')).toBeVisible();

	await queueRow(page, 'row-zulu');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-zulu']);
});

test('LR-B-23 an account switch purges history and records the new account fresh', async ({
	page
}) => {
	const accountA: LibraryItem[] = [
		{ id: 'a-one', title: 'A One', artist: 'A Band', streamUrl: `${AUDIO_PREFIX}/a-one.wav` },
		{ id: 'a-two', title: 'A Two', artist: 'A Band', streamUrl: `${AUDIO_PREFIX}/a-two.wav` }
	];
	const accountB: LibraryItem[] = [
		{ id: 'b-one', title: 'B One', artist: 'B Band', streamUrl: `${AUDIO_PREFIX}/b-one.wav` }
	];
	const box = { status: 200, body: JSON.stringify(accountA) };
	await routeStrict(page, () => box);
	await installProbe(page);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(accountA.length);

	await queueRow(page, 'a-one');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['a-one']);

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	box.body = JSON.stringify(accountB);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('B One');
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	expect(await storedEntries(page)).toEqual([]);

	await queueRow(page, 'b-one');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['b-one']);
	expect(await storedEntries(page)).toHaveLength(1);
});

test('LR-B-24 a successful sign-out purges the open library tab before redirect', async ({
	page
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);

	const securityTab = await page.context().newPage();
	try {
		await securityTab.route('**/v1/vault', (route) =>
			route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({}) })
		);
		let posts = 0;
		await securityTab.route('**/v1/session/logout', async (route) => {
			posts += 1;
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true })
			});
		});
		await securityTab.goto('/account/security');
		await securityTab.getByRole('button', { name: /^sign out$/i }).click();

		await securityTab.waitForURL('**/login');
		expect(posts).toBe(1);
	} finally {
		await securityTab.close();
	}

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	expect(await storedEntries(page)).toEqual([]);
});

test('LR-B-25 an expired session purges history and stays empty until the next play', async ({
	page
}) => {
	const box = { status: 200, body: JSON.stringify(ITEMS) };
	await routeStrict(page, () => box);
	await installProbe(page);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);

	box.status = 401;
	box.body = JSON.stringify({ error: 'expired' });
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
	await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
	expect(await storedEntries(page)).toEqual([]);

	box.status = 200;
	box.body = JSON.stringify(ITEMS);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('library-recently-played-empty')).toBeVisible();
});

test('LR-B-26 each history action announces once politely; selection and progress stay quiet', async ({
	page
}) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => liveMessages(page)).toHaveLength(1);
	expect((await liveMessages(page))[0]).toMatch(/recent|played|history/i);

	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await expect.poll(() => liveMessages(page)).toHaveLength(1);

	await replayAction(page, 'row-alpha').click();
	await expect.poll(() => liveMessages(page)).toHaveLength(1);
	expect((await liveMessages(page))[0]).toMatch(/replay|play/i);

	await forgetAction(page, 'row-mike').click();
	await expect.poll(() => liveMessages(page)).toHaveLength(1);
	expect((await liveMessages(page))[0]).toMatch(/forgot|removed/i);

	// Selection, pause and progress never chatter the history region.
	const quiet = await liveMessages(page);
	await queueRow(page, 'row-zulu');
	await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('pause'));
		el.currentTime = 33;
		el.dispatchEvent(new Event('timeupdate'));
	});
	expect(await liveMessages(page)).toEqual(quiet);

	await page.getByTestId('library-recently-played-clear').click();
	await expect.poll(() => liveMessages(page)).toHaveLength(1);
	expect((await liveMessages(page))[0]).toMatch(/clear/i);
	await expect(section(page).locator('[role="alert"], [aria-live="assertive"]')).toHaveCount(0);
});

for (const colorScheme of ['light', 'dark'] as const) {
	test(`LR-B-27-${colorScheme} narrow touch actions are distinct, reachable and contained`, async ({
		browser,
		baseURL
	}, testInfo) => {
		const context = await browser.newContext({
			baseURL,
			viewport: { width: 375, height: 812 },
			hasTouch: true,
			isMobile: true,
			colorScheme
		});
		const page = await context.newPage();
		try {
			await open(page);
			await queueRow(page, 'row-alpha');
			await dispatchPlaying(page);
			await queueRow(page, 'row-mike');
			await queueSelect(page, 'row-mike').click();
			await dispatchPlaying(page);
			await expect.poll(() => recentIds(page)).toEqual(['row-mike', 'row-alpha']);

			await expect(replayAction(page, 'row-alpha')).toBeVisible();
			await expect(forgetAction(page, 'row-alpha')).toBeVisible();
			await expect(page.getByTestId('library-recently-played-clear')).toBeVisible();
			for (const control of [
				replayAction(page, 'row-alpha'),
				forgetAction(page, 'row-alpha'),
				page.getByTestId('library-recently-played-clear')
			]) {
				await control.scrollIntoViewIfNeeded();
				const box = await control.boundingBox();
				expect(box).not.toBeNull();
				expect(box!.x).toBeGreaterThanOrEqual(0);
				expect(box!.x + box!.width).toBeLessThanOrEqual(375);
				expect(box!.width).toBeGreaterThanOrEqual(24);
				expect(box!.height).toBeGreaterThanOrEqual(24);
			}
			await replayAction(page, 'row-alpha').tap();
			await expect.poll(() => recentIds(page)).toEqual(['row-alpha', 'row-mike']);
			await forgetAction(page, 'row-alpha').tap();
			await expect.poll(() => recentIds(page)).toEqual(['row-mike']);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
				true
			);
			await testInfo.attach(`library-recent-${colorScheme}`, {
				body: await section(page).screenshot(),
				contentType: 'image/png'
			});
			expect(backendCalls.get(page)).toEqual([]);
		} finally {
			await context.close();
		}
	});
}

test('LR-B-28 history flows make zero history-driven backend calls', async ({ page }) => {
	await open(page);
	await queueRow(page, 'row-alpha');
	await dispatchPlaying(page);
	await expect.poll(() => recentIds(page)).toEqual(['row-alpha']);
	await queueRow(page, 'row-mike');
	await queueSelect(page, 'row-mike').click();
	await dispatchPlaying(page);
	await replayAction(page, 'row-alpha').click();
	await forgetAction(page, 'row-mike').click();
	await page.getByTestId('library-recently-played-clear').click();

	// Settle: any (buggy) history call fires with the flows above.
	await page.waitForTimeout(250);

	expect(backendCalls.get(page)).toEqual([]);
});
