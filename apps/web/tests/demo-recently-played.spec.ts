import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: session-local demo recently-played list (test half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build half continues
 * this branch and must make these tests pass WITHOUT editing this file; a
 * wrong test comes back to the acceptance card instead.
 *
 * Surface: `/demo/player` is the only surface with real media events, so the
 * list is pinned here over the 3-track demo catalogue. Media loading is held
 * pending (route-held, never fulfilled) and `play()` is stubbed to resolve,
 * so flows are driven by synthetic media events with no network beyond the
 * local preview server (the radio-continue-spec pattern).
 *
 * Contract seams the build half implements:
 * - `recently-played`: the section, headed `Recently played`, demo-labeled
 *   (its text matches /demo/i) and never mentioning library tracks.
 * - `recently-played-empty`: honest empty state, visible iff the list is
 *   empty, with non-empty text.
 * - `recent-item[data-track-id]`: one per entry, most-recent-first, showing
 *   the demo track title.
 * - `recent-played-at-{id}`: non-empty relative played-at label per entry.
 * - `recent-replay-{id}`: replay action per entry; activating it selects
 *   the track (now-playing names it) and moves it to the front without
 *   duplicating. Keyboard operable.
 * - `recently-played-clear`: explicit Clear; empties the list, persists `[]`,
 *   and its section copy documents the queue semantics (matches /queue/i):
 *   clearing the queue never wipes history — explicit Clear only.
 * - Storage: sessionStorage under `nntune:demo-recently-played:v1` holds a
 *   JSON array of `{ trackId, playedAt }` (the unit half pins the exact
 *   shape, cap and corrupt fallback in
 *   `src/lib/player/recentlyPlayed.acceptance.test.ts`).
 *
 * What's pinned: completed plays (natural `ended`) record the COMPLETED
 * track; explicit plays (queue select, Next/Previous, play-press, replay)
 * record the newly playing track; list is capped at 10 distinct with
 * replay-to-front dedupe; seeks, retries, pauses and queue edits never
 * append; reload persists within the session; corrupt storage falls back to
 * the empty state with the page usable; entries reference demo tracks only;
 * flows make zero backend calls.
 *
 * Locked decisions (so the builder never guesses):
 * - Auto-advance after a natural end records nothing extra: the completed
 *   track's entry covers the listen, so select+ended yields exactly ONE
 *   entry. The auto-advanced track earns its entry when IT completes or is
 *   explicitly played. The same holds for radio passes.
 * - An explicit pick that fails to play still records (the gesture was an
 *   explicit play); only the RETRY afterwards must not append.
 * - Radio completions record like queue completions (a completed demo play
 *   is a completed demo play); radio badge/behavior is otherwise untouched.
 * - Pause records nothing; play-after-pause dedupes to the single entry.
 *
 * Out of scope (not pinned): library/private history, cross-device sync,
 * play counts, scrobbling, exact relative-label wording (non-empty only),
 * unknown-id rendering (storage assertions carry the cap), reduced-motion
 * internals (no-new-timers is a builder code-review check).
 *
 * CASE INDEX (stable IDs; RP-U-* live in the unit acceptance file):
 * RP-B-01 empty state, RP-B-02 completion appends, RP-B-03 Next appends,
 * RP-B-04 selection appends, RP-B-05 play-press appends, RP-B-06 dedupe,
 * RP-B-07 seeks, RP-B-08 retry, RP-B-09 queue edits, RP-B-10 queue-clear
 * keeps history, RP-B-11 replay, RP-B-12 replay keyboard, RP-B-13 Clear,
 * RP-B-14 corrupt storage, RP-B-15 reload persistence, RP-B-16 cap,
 * RP-B-17 demo-only, RP-B-18 zero backend calls, RP-B-19 pause,
 * RP-B-20 comforts, RP-B-21 radio completion.
 */

const STORAGE_KEY = 'nntune:demo-recently-played:v1';
const DEMO_IDS = ['demo-track', 'second-signal', 'third-light'];
const TITLE_BY_ID: Record<string, string> = {
	'demo-track': 'Demo track',
	'second-signal': 'Second Signal',
	'third-light': 'Third Light'
};

async function openPlayer(page: Page, url = '/demo/player'): Promise<void> {
	// Hold media loading synthetic and pending (the radio-spec pattern):
	// fulfilling garbage bytes would fire a native error event on the real
	// element. Only fixture events settle playback.
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto(url);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

/** Browser end-of-media: both flags set, then pause before ended. */
async function naturalEnd(page: Page): Promise<void> {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}

async function storedEntries(page: Page): Promise<{ trackId: string; playedAt: number }[]> {
	const raw = await page.evaluate((key) => sessionStorage.getItem(key), STORAGE_KEY);
	if (raw === null) return [];
	return JSON.parse(raw) as { trackId: string; playedAt: number }[];
}

async function recentIds(page: Page): Promise<string[]> {
	return page.getByTestId('recent-item').evaluateAll((items) =>
		items.map((item) => item.getAttribute('data-track-id') ?? '')
	);
}

test('RP-B-01 fresh load shows an honest empty state in a demo-labeled section', async ({
	page
}) => {
	await openPlayer(page);

	const section = page.getByTestId('recently-played');
	await expect(section).toBeVisible();
	await expect(section.getByRole('heading')).toContainText('Recently played');
	await expect(section).toContainText(/demo/i);

	const empty = page.getByTestId('recently-played-empty');
	await expect(empty).toBeVisible();
	expect(((await empty.textContent()) ?? '').trim().length).toBeGreaterThan(0);

	await expect(page.getByTestId('recent-item')).toHaveCount(0);
});

test('RP-B-02 a completed play appends the completed track with title and played-at', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await naturalEnd(page);

	await expect(page.getByTestId('recent-item')).toHaveCount(1);
	const item = page.getByTestId('recent-item').first();
	await expect(item).toHaveAttribute('data-track-id', 'demo-track');
	await expect(item).toContainText('Demo track');
	const playedAt = page.getByTestId('recent-played-at-demo-track');
	await expect(playedAt).toBeVisible();
	expect(((await playedAt.textContent()) ?? '').trim().length).toBeGreaterThan(0);
	await expect(page.getByTestId('recently-played-empty')).toBeHidden();
});

test('RP-B-03 explicit Next records the newly selected track', async ({ page }) => {
	await openPlayer(page);

	await page.getByTestId('player-next').click();

	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect(await recentIds(page)).toEqual(['second-signal']);
});

test('RP-B-04 explicit queue selection records the picked track', async ({ page }) => {
	await openPlayer(page);

	await page.getByTestId('queue-select-third-light').click();

	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	expect(await recentIds(page)).toEqual(['third-light']);
});

test('RP-B-05 a play-press records the current track', async ({ page }) => {
	await openPlayer(page);

	await page.keyboard.press('k');

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await recentIds(page)).toEqual(['demo-track']);
});

test('RP-B-06 re-plays dedupe to the front; auto-advance appends nothing extra', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	// The completed track's entry covers the listen: the auto-advanced
	// track earns its entry only when IT completes or is explicit.
	await naturalEnd(page);
	expect(await recentIds(page)).toEqual(['demo-track']);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');

	await page.getByTestId('player-next').click();
	expect(await recentIds(page)).toEqual(['third-light', 'demo-track']);

	await page.getByTestId('player-prev').click();
	expect(await recentIds(page)).toEqual(['second-signal', 'third-light', 'demo-track']);

	// Replaying the oldest moves it to the front without duplicating.
	await page.getByTestId('queue-select-demo-track').click();
	expect(await recentIds(page)).toEqual(['demo-track', 'second-signal', 'third-light']);
});

test('RP-B-07 seeks never append', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await page.keyboard.press('l');
	await page.getByTestId('demo-audio').evaluate((el) => {
		el.dispatchEvent(new Event('seeking'));
		el.dispatchEvent(new Event('seeked'));
	});
	await page.keyboard.press('j');

	expect(await recentIds(page)).toEqual(['demo-track']);
	expect(await storedEntries(page)).toHaveLength(1);
});

test('RP-B-08 retries never append', async ({ page }) => {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		const control = { mode: 'reject' };
		(window as unknown as { __playMode: { mode: string } }).__playMode = control;
		HTMLMediaElement.prototype.play = function () {
			if (control.mode === 'reject') return Promise.reject(new Error('fixture boom'));
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	// The explicit pick records even though its playback fails.
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-error')).toBeVisible();
	expect(await recentIds(page)).toEqual(['second-signal']);

	// The retry recovers playback but must not append.
	await page.evaluate(() => {
		(window as unknown as { __playMode: { mode: string } }).__playMode.mode = 'resolve';
	});
	await page.getByTestId('player-retry').click();
	await page.getByTestId('demo-audio').evaluate((el) => {
		el.dispatchEvent(new Event('playing'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await recentIds(page)).toEqual(['second-signal']);
	expect(await storedEntries(page)).toHaveLength(1);
});

test('RP-B-09 queue edits never append', async ({ page }) => {
	await openPlayer(page);

	// Down then up on the same track: both buttons are enabled at click
	// time (moving another track to a list edge would disable its button).
	await page.getByTestId('queue-move-down-demo-track').click();
	await page.getByTestId('queue-move-up-demo-track').click();

	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	expect(await storedEntries(page)).toEqual([]);
});

test('RP-B-10 clearing the queue keeps history and the copy says so', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	expect(await recentIds(page)).toEqual(['demo-track']);

	await page.getByTestId('queue-clear').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	expect(await recentIds(page)).toEqual(['demo-track']);
	// Explicit Clear only: the section documents the queue semantics.
	await expect(page.getByTestId('recently-played')).toContainText(/queue/i);
});

test('RP-B-11 replay selects the track and moves it to the front without duplicating', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	await page.getByTestId('player-next').click();
	expect(await recentIds(page)).toEqual(['third-light', 'demo-track']);

	await page.getByTestId('recent-replay-demo-track').click();

	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	expect(await recentIds(page)).toEqual(['demo-track', 'third-light']);
});

test('RP-B-12 replay is keyboard operable', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	await page.getByTestId('player-next').click();
	expect(await recentIds(page)).toEqual(['third-light', 'demo-track']);

	await page.getByTestId('recent-replay-demo-track').focus();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	expect(await recentIds(page)).toEqual(['demo-track', 'third-light']);
});

test('RP-B-13 Clear empties the list and persists []', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	await expect(page.getByTestId('recent-item')).toHaveCount(1);

	await page.getByTestId('recently-played-clear').click();

	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
	expect(await page.evaluate((key) => sessionStorage.getItem(key), STORAGE_KEY)).toBe('[]');
});

test('RP-B-14 corrupt storage falls back to the empty state with the page usable', async ({
	page
}) => {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript((key) => {
		sessionStorage.setItem(key, '{oops');
	}, STORAGE_KEY);
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');

	// The page renders and plays despite the corrupt list store.
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
	await expect(page.getByTestId('recent-item')).toHaveCount(0);

	// And the store recovers on the next play.
	await page.getByTestId('queue-select-demo-track').click();
	expect(await recentIds(page)).toEqual(['demo-track']);
});

test('RP-B-15 reload persists entries within the session', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	await page.getByTestId('player-next').click();
	expect(await recentIds(page)).toEqual(['third-light', 'demo-track']);

	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	expect(await recentIds(page)).toEqual(['third-light', 'demo-track']);
});

test('RP-B-16 the list stays capped at 10 distinct with the oldest evicted', async ({ page }) => {
	// Ten entries WITHOUT demo-track, so the first completed demo-track
	// play is an 11th distinct track and must evict the oldest (seed-11).
	// Seeding demo-track instead would only dedupe and never pin eviction.
	const seeded = [
		{ trackId: 'second-signal', playedAt: 1000 },
		{ trackId: 'third-light', playedAt: 900 },
		...Array.from({ length: 8 }, (_, i) => ({
			trackId: `seed-0${i + 4}`,
			playedAt: 800 - i
		}))
	];
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(
		({ key, entries }) => {
			sessionStorage.setItem(key, JSON.stringify(entries));
		},
		{ key: STORAGE_KEY, entries: seeded }
	);
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	// Completing a play records demo-track at the front and evicts seed-11.
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);

	const stored = await storedEntries(page);
	expect(stored).toHaveLength(10);
	expect(stored[0]?.trackId).toBe('demo-track');
	expect(stored.some((entry) => entry.trackId === 'seed-11')).toBe(false);

	// Rendering may filter unknown ids; whatever renders stays capped with
	// the just-played track first and no duplicates.
	const ids = await recentIds(page);
	expect(ids.length).toBeLessThanOrEqual(10);
	expect(ids[0]).toBe('demo-track');
	expect(new Set(ids).size).toBe(ids.length);
});

test('RP-B-17 entries reference demo tracks only; the section never mentions the library', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	await page.getByTestId('player-next').click();

	for (const entry of await storedEntries(page)) {
		expect(DEMO_IDS).toContain(entry.trackId);
	}
	const section = page.getByTestId('recently-played');
	await expect(section).toContainText(/demo/i);
	await expect(section).not.toContainText(/library/i);
});

test('RP-B-18 recent-played flows make zero backend calls', async ({ page }) => {
	const calls: { method: string; url: string; resourceType: string }[] = [];
	page.on('request', (request) => {
		calls.push({ method: request.method(), url: request.url(), resourceType: request.resourceType() });
	});
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	await page.getByTestId('player-next').click();

	// Settle: any (buggy) history call fires with the flows above.
	await page.waitForTimeout(250);

	expect(calls.length).toBeGreaterThan(0);
	// Same-origin with the served page (never hardcoded: the suite runs on
	// whatever preview port the config binds).
	const origin = new URL(page.url()).origin;
	for (const call of calls) {
		expect(call.url.startsWith(`${origin}/`)).toBe(true);
		expect(call.method).toBe('GET');
		expect(['xhr', 'fetch']).not.toContain(call.resourceType);
	}
	expect(calls.some((call) => call.resourceType === 'xhr' || call.resourceType === 'fetch')).toBe(
		false
	);
});

test('RP-B-19 pause appends nothing; play-after-pause does not duplicate', async ({ page }) => {
	await openPlayer(page);
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await recentIds(page)).toEqual(['demo-track']);

	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	expect(await recentIds(page)).toEqual(['demo-track']);

	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await recentIds(page)).toEqual(['demo-track']);
	expect(await storedEntries(page)).toHaveLength(1);
});

test('RP-B-20 speed and sleep comforts never append and keep working', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	expect(await recentIds(page)).toEqual(['demo-track']);

	await page.getByTestId('playback-speed').selectOption('1.5');
	await page.getByTestId('sleep-timer-set-end-of-track').click();
	await expect(page.getByTestId('sleep-timer-status')).toHaveText('Sleep at end of track');
	await page.getByTestId('sleep-timer-cancel').click();
	await expect(page.getByTestId('sleep-timer-status')).toHaveText('Sleep timer off');

	expect(await recentIds(page)).toEqual(['demo-track']);
	expect(await storedEntries(page)).toHaveLength(1);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.5');
});

test('RP-B-21 completing a radio track records it; radio behavior is untouched', async ({
	page
}) => {
	await openPlayer(page, '/demo/player?radio-seed=7');
	await page.getByTestId('queue-select-demo-track').click();
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await naturalEnd(page);
	await expect(page.getByTestId('radio-badge')).toContainText('Radio');

	const radioText = (await page.getByTestId('now-playing').textContent()) ?? '';
	const radioId = DEMO_IDS.find((id) => radioText.includes(TITLE_BY_ID[id] as string));
	expect(radioId).toBeDefined();

	await naturalEnd(page);

	await expect(page.getByTestId('radio-badge')).toContainText('Radio');
	expect(await recentIds(page)).toContain(radioId as string);
});
