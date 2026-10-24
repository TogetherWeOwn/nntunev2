import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: the demo player tab shows the now-playing track (browser half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build half continues
 * this branch and must make these tests pass WITHOUT editing this file; a
 * wrong test comes back to the acceptance card instead.
 *
 * Surface: `/demo/player` is the only surface with real media events, so the
 * tab title is pinned here over the 3-track demo catalogue. Media loading is
 * held pending (route-held, never fulfilled) and `play()` is stubbed to
 * resolve, so flows are driven by synthetic media events with no network
 * beyond the local preview server (the radio-continue-spec pattern).
 *
 * Contract seams the build half implements (behavioral — the title is the
 * only surface; there are no new testids, no new DOM, no new storage keys):
 * - While a demo track is selected, `document.title` is
 *   `{title} — {artist}` (EM DASH U+2014; the unit half in
 *   `src/lib/player/tabTitle.acceptance.test.ts` pins fallbacks: blank
 *   artist omitted, blank title falls back to the id, unicode verbatim).
 * - ` (paused)` is suffixed whenever playback is not progressing
 *   (selected-before-play, loading, paused, error, ended-at-queue-end);
 *   cleared while playing.
 * - The exact static head title (`NNTune — player resilience demo`) returns
 *   when the selection clears / the queue empties; unmount teardown restores
 *   exactly once (pinned at unit level — a real unmount is not observable
 *   from a second document, so the browser pins clear/empty restore here).
 * - Updates fire on selection change, play, pause, ended and error; a source
 *   swap never leaves the previous track's title behind.
 * - Browser-effect only: pure `document.title` writes — no storage keys, no
 *   fetches, no transport calls, no layout or focus changes.
 *
 * RED vs GREEN controls: TT-B-01..TT-B-08, TT-B-10 and TT-B-13 are assertion
 * RED on current code (nothing writes `document.title`, so every "title
 * shows the track" expectation fails against the static head title).
 * TT-B-09 (restore half), TT-B-11, TT-B-12 and TT-B-14 are GREEN controls:
 * they pass today and guard the builder against regressing the static
 * title, focus/DOM stability, the zero-footprint rule and pageerror hygiene.
 * The re-add half of TT-B-09 is RED.
 *
 * Locked decisions (so the builder never guesses):
 * - A fresh load already has a selection (queue restores to track 1), so it
 *   shows the track title WITH the paused suffix — "while selected" plus
 *   "not progressing", from the first paint. The static title only returns
 *   on clear/empty/unmount.
 * - Error and ended-at-queue-end keep the current track's title with the
 *   paused suffix (the selection survives; only progress stopped). Queue
 *   CLEAR restores the static title (the selection is gone).
 * - Radio plays demo tracks through the same selection, so a radio track
 *   shows its formatted title like any explicit play (TT-B-13 derives WHICH
 *   track from now-playing but pins the title FORMAT independently).
 * - Retry-after-error clears the suffix once playback progresses; a failed
 *   retry keeps the suffixed title (covered via the error-half pattern).
 * - Swap races are pinned by final state only: transitions are synchronous
 *   reactive writes, so sampling mid-transition would be timing-flaky.
 *
 * Out of scope (not pinned): Media Session changes, library tab titles,
 * favicons/badges, notifications, new shortcuts, buffering-transient
 * wording (too timing-sensitive), unmount-via-navigation (unobservable
 * cross-document; unit-pinned), backend, deployment.
 *
 * CASE INDEX (stable IDs; TT-U-* live in the unit acceptance file):
 * TT-B-01 fresh load paused title, TT-B-02 select-while-playing bare title,
 * TT-B-03 pause suffix, TT-B-04 resume clears, TT-B-05 Next/Previous swap,
 * TT-B-06 error sane + retry clears, TT-B-07 natural end advances,
 * TT-B-08 Next past end keeps last paused, TT-B-09 clear restores static +
 * re-add, TT-B-10 rapid swaps end latest, TT-B-11 no focus/DOM churn
 * (GREEN), TT-B-12 zero network + zero new storage keys (GREEN), TT-B-13
 * radio track title, TT-B-14 no uncaught page errors (GREEN).
 */

const STATIC_TITLE = 'NNTune — player resilience demo';
const DEMO_TITLE = 'Demo track — Fixture Band';
const SECOND_TITLE = 'Second Signal — Fixture Band';
const THIRD_TITLE = 'Third Light — Session Players';

const KNOWN_LOCAL_KEYS = [
	'nntune:sound-preference:v1',
	'nntune:playback-speed:v1',
	'nntune:resume:v1',
	'nntune:radio-continue:v1',
	'nntune.leveling.enabled'
];
const KNOWN_SESSION_KEYS = ['nntune:demo-queue:v1', 'nntune:demo-recently-played:v1'];

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

/** Native-control resume: unpause flags, then play before playing. */
async function nativeResume(page: Page): Promise<void> {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: false, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
}

async function focusAndDom(page: Page): Promise<{ active: string | null; nodes: number }> {
	return page.evaluate(() => ({
		active: (document.activeElement as HTMLElement | null)?.getAttribute('data-testid'),
		nodes: document.querySelectorAll('*').length
	}));
}

test('TT-B-01 fresh load shows the selected track with the paused suffix', async ({
	page
}) => {
	await openPlayer(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');

	await expect(page).toHaveTitle(`${DEMO_TITLE} (paused)`);
});

test('TT-B-02 explicit select while playing shows the bare track title', async ({
	page
}) => {
	await openPlayer(page);

	await page.getByTestId('queue-select-second-signal').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page).toHaveTitle(SECOND_TITLE);
});

test('TT-B-03 pausing appends the paused suffix', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page).toHaveTitle(SECOND_TITLE);

	await page.keyboard.press('k');

	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page).toHaveTitle(`${SECOND_TITLE} (paused)`);
});

test('TT-B-04 resuming clears the paused suffix', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await page.keyboard.press('k');
	await expect(page).toHaveTitle(`${SECOND_TITLE} (paused)`);

	await page.keyboard.press('k');

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page).toHaveTitle(SECOND_TITLE);
});

test('TT-B-05 Next and Previous swap to the new track title, never stale', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page).toHaveTitle(SECOND_TITLE);

	await page.getByTestId('player-next').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect(page).toHaveTitle(THIRD_TITLE);

	await page.getByTestId('player-prev').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page).toHaveTitle(SECOND_TITLE);
});

test('TT-B-06 error keeps the suffixed title; retry clears it on recovery', async ({
	page
}) => {
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

	await page.getByTestId('queue-select-second-signal').click();

	await expect(page.getByTestId('player-error')).toBeVisible();
	await expect(page).toHaveTitle(`${SECOND_TITLE} (paused)`);

	await page.evaluate(() => {
		(window as unknown as { __playMode: { mode: string } }).__playMode.mode = 'resolve';
	});
	await page.getByTestId('player-retry').click();
	await page.getByTestId('demo-audio').evaluate((el) => {
		el.dispatchEvent(new Event('playing'));
	});

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page).toHaveTitle(SECOND_TITLE);
});

test('TT-B-07 natural end advances the title to the next track', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page).toHaveTitle(DEMO_TITLE);

	await naturalEnd(page);

	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page).toHaveTitle(SECOND_TITLE);
});

test('TT-B-08 explicit Next past the queue end keeps the last track paused title', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-third-light').click();
	await expect(page).toHaveTitle(THIRD_TITLE);

	await page.getByTestId('player-next').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
	await expect(page).toHaveTitle(`${THIRD_TITLE} (paused)`);
});

test('TT-B-09 clearing the queue restores the exact static title; re-add brings the track back', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await page.getByTestId('queue-clear').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page).toHaveTitle(STATIC_TITLE);

	await page.getByTestId('queue-add-demo-track').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page).toHaveTitle(DEMO_TITLE);
});

test('TT-B-10 rapid swaps end on the latest track, never a stale title', async ({
	page
}) => {
	await openPlayer(page);

	await page.getByTestId('queue-select-second-signal').click();
	await page.getByTestId('queue-select-third-light').click();
	await page.getByTestId('queue-select-demo-track').click();

	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page).toHaveTitle(DEMO_TITLE);
});

test('TT-B-11 title-only transitions move no focus and change no DOM (GREEN control)', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('player-next').focus();
	const before = await focusAndDom(page);
	expect(before.active).toBe('player-next');

	// Synthetic pause: a title-only transition with no click and no gesture.
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	expect(await focusAndDom(page)).toEqual(before);

	await nativeResume(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await focusAndDom(page)).toEqual(before);
});

test('TT-B-12 title flows make zero network calls and add zero storage keys (GREEN control)', async ({
	page
}) => {
	const calls: { method: string; url: string; resourceType: string }[] = [];
	page.on('request', (request) => {
		calls.push({ method: request.method(), url: request.url(), resourceType: request.resourceType() });
	});
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');

	// Settle: any (buggy) title-triggered call fires with the flows above.
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

	const keys = await page.evaluate(() => ({
		local: Object.keys(localStorage),
		session: Object.keys(sessionStorage)
	}));
	for (const key of keys.local) {
		expect(KNOWN_LOCAL_KEYS, `unexpected localStorage key from title flows: ${key}`).toContain(
			key
		);
	}
	for (const key of keys.session) {
		expect(KNOWN_SESSION_KEYS, `unexpected sessionStorage key from title flows: ${key}`).toContain(
			key
		);
	}
});

test('TT-B-13 a radio track shows its formatted title like any explicit play', async ({
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
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	// WHICH track is seeded (oracle: now-playing); the FORMAT pin is ours.
	const nowPlaying = (await page.getByTestId('now-playing').textContent()) ?? '';
	const match = /^(?:Now playing|Selected): (.+)$/.exec(nowPlaying.trim());
	expect(match, `unparsable now-playing text: ${JSON.stringify(nowPlaying)}`).not.toBeNull();
	await expect(page).toHaveTitle(match?.[1] ?? 'impossible-title-Colorado');
});

test('TT-B-14 title flows throw no uncaught page errors (GREEN control)', async ({
	page
}) => {
	// Pageerrors only: the artwork 404 fixtures intentionally log console
	// resource errors, so console text is not a clean signal. An uncaught
	// exception from title-effect wiring would surface here instead.
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	expect(errors).toEqual([]);
});
