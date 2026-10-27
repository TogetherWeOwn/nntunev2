import { expect, test, type Page } from '@playwright/test';

/**
 * Protected RED acceptance: offer explicit recovery after prolonged
 * playback loading (browser half). Unit half: LW-U* in
 * src/lib/player/longWait.acceptance.test.ts, which locks the pure
 * policy the page wires to this DOM seam. Do not edit this bank to make
 * an implementation pass; a wrong test returns to the acceptance card.
 *
 * LOCKED DOM SEAM (the one additive surface on the synthetic demo route):
 * - `player-long-wait`: the Still waiting notice container. Present in
 *   the DOM only while shown. Its visible text contains "Still waiting"
 *   and stays neutral — elapsed-wait feedback, never a diagnosis (no
 *   network/provider/offline/timeout/failure wording, no player-error,
 *   status never becomes error from waiting alone).
 * - `player-long-wait-retry`: a real button, accessible name
 *   "Retry playback". Exactly one explicit new attempt on the
 *   still-current demo source through the existing transport.
 * - `player-long-wait-pause`: a real button, accessible name "Pause".
 *   Cancels the pending intent with no new play, load or fetch.
 * Distinct from the error state's `player-retry`: while merely loading
 * (including past 15s) the error retry must not appear.
 *
 * BEHAVIOUR PINNED HERE (timing boundaries themselves are LW-U02..U04):
 * the notice appears after 15 seconds of continuous current-source
 * initial loading or of an actual-playing-then-waiting interruption
 * with active intent; a stalled event alone never starts it. It never
 * auto-retries, auto-advances, reloads or refetches. Playing, pause,
 * end, error, track/source replacement, unload and teardown clear it
 * and its timer; repeated waits need a full new 15 seconds. Late
 * promises/events from an old attempt or source are fenced. The
 * transition is announced once, politely (role=status or
 * aria-live=polite on the container; never alert/assertive), does not
 * steal focus, and the native audio controls stay usable.
 *
 * Only native media I/O is stubbed (the pattern of the sibling buffering
 * bank): real page handlers, queue, status and shortcuts run.
 * Playwright's clock drives the page's real clocks/timers, so a page
 * scheduler callback and a periodic tick are both acceptable internal
 * mechanisms; browser waits below use a 1s slack past 15s and leave the
 * exact boundary to the unit half. Fully offline/synthetic: the media
 * response is held pending, no provider or production traffic.
 *
 * CASE INDEX (stable IDs): LW-B01 initial never-settling play shows
 * the neutral notice past 15s, LW-B02 nothing automatic happens while
 * it shows, LW-B03 playing->waiting interruption with recovery and
 * restart, LW-B04 stalled alone is never enough, LW-B05 explicit Retry
 * only, LW-B06 explicit Pause with late-promise fencing, LW-B07 pause
 * before the deadline cancels, LW-B08 track replacement fences and
 * restarts, LW-B09 error supersedes the notice, LW-B10 natural end
 * restarts only for the next source, LW-B11 clearing the queue
 * (unload) cancels, LW-B12 focus/native-controls/single polite
 * transition, LW-B13 no active intent shows nothing (negative
 * control), LW-B14 a background-delayed clock jump shows once for the
 * still-current source.
 */
type Probe = {
	resolve: () => void;
	counts: { play: number; pause: number; load: number; fetch: number };
	noticeRises: number;
};
type ProbeWindow = Window & { __longWait: Probe };

const epoch = new Date('2026-01-01T00:00:00Z');
const notice = (page: Page) => page.getByTestId('player-long-wait');
const retryButton = (page: Page) => page.getByTestId('player-long-wait-retry');
const pauseButton = (page: Page) => page.getByTestId('player-long-wait-pause');

test.use({ actionTimeout: 5_000, reducedMotion: 'reduce' });

async function boot(page: Page) {
	const errors: string[] = [];
	const audioRequests: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => {
		if (new URL(request.url()).pathname === '/api/demo/audio') audioRequests.push(request.url());
	});
	await page.clock.install({ time: epoch });
	await page.clock.pauseAt(epoch);
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (url.pathname === '/api/demo/audio') return; // synthetic media held pending
		if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		sessionStorage.clear();
		localStorage.clear();
		const probe: Probe = {
			resolve: () => {},
			counts: { play: 0, pause: 0, load: 0, fetch: 0 },
			noticeRises: 0
		};
		Object.assign(window, { __longWait: probe });
		HTMLMediaElement.prototype.play = function () {
			probe.counts.play++;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return new Promise<void>((resolve) => {
				probe.resolve = resolve;
			});
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.counts.pause++;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		HTMLMediaElement.prototype.load = function () {
			probe.counts.load++;
		};
		const realFetch = window.fetch.bind(window);
		window.fetch = (...args) => {
			probe.counts.fetch++;
			return realFetch(...args);
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	// One announced transition == one rising edge of the notice in the DOM.
	await page.evaluate(() => {
		const probe = (window as unknown as ProbeWindow).__longWait;
		let present = false;
		new MutationObserver(() => {
			const now = document.querySelector('[data-testid="player-long-wait"]') !== null;
			if (now && !present) probe.noticeRises++;
			present = now;
		}).observe(document.body, { subtree: true, childList: true });
	});
	return { errors, audioRequests };
}

async function advance(page: Page, ms: number) {
	await page.clock.runFor(ms);
}

async function event(page: Page, name: string) {
	await page.getByTestId('demo-audio').evaluate((el, type) => {
		if (type === 'play' || type === 'pause') {
			Object.defineProperty(el, 'paused', { value: type === 'pause', configurable: true });
		}
		el.dispatchEvent(new Event(type));
	}, name);
}

async function settlePlay(page: Page) {
	await page.evaluate(() => (window as unknown as ProbeWindow).__longWait.resolve());
}

async function status(page: Page, state: string) {
	await expect(page.getByTestId('player-status')).toHaveText(new RegExp(`^Status: ${state}$`, 'i'));
}

async function startLoading(page: Page, id = 'demo-track') {
	await page.getByTestId(`queue-select-${id}`).click();
	await status(page, 'loading');
}

async function startPlaying(page: Page, id = 'demo-track') {
	await startLoading(page, id);
	await settlePlay(page);
	await event(page, 'playing');
	await status(page, 'playing');
}

async function counts(page: Page) {
	return page.evaluate(() => ({ ...(window as unknown as ProbeWindow).__longWait.counts }));
}

async function noticeRises(page: Page) {
	return page.evaluate(() => (window as unknown as ProbeWindow).__longWait.noticeRises);
}

async function source(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el) => el.getAttribute('src'));
}

async function queueIds(page: Page) {
	return page
		.getByTestId('queue-item')
		.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}

async function naturalEnd(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}

test('LW-B01 a never-settling initial play shows the neutral Still waiting notice past 15s', async ({ page }) => {
	const { errors } = await boot(page);
	await startLoading(page);
	await advance(page, 14_000);
	await expect(notice(page), 'LW-B01: nothing before 15s of continuous loading').toHaveCount(0);
	await advance(page, 2_000);
	await expect(notice(page)).toBeVisible();
	await expect(notice(page)).toContainText(/still waiting/i);
	await expect(
		notice(page),
		'LW-B01: elapsed-wait feedback, never a diagnosis'
	).not.toContainText(/offline|network|provider|timed out|timeout|failed|error/i);
	await status(page, 'loading');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await expect(page.getByTestId('player-retry'), 'LW-B01: the error retry stays error-only').toHaveCount(0);
	await expect(retryButton(page)).toHaveAccessibleName(/retry playback/i);
	await expect(pauseButton(page)).toHaveAccessibleName(/^pause$/i);
	const polite = await notice(page).evaluate(
		(el) => el.getAttribute('role') === 'status' || el.getAttribute('aria-live') === 'polite'
	);
	expect(polite, 'LW-B01: the transition is announced politely').toBe(true);
	await expect(notice(page)).not.toHaveAttribute('role', 'alert');
	await expect(notice(page)).not.toHaveAttribute('aria-live', 'assertive');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	expect((await counts(page)).play).toBe(1);
	expect(await noticeRises(page)).toBe(1);
	expect(errors).toEqual([]);
});

test('LW-B02 while the notice shows, nothing retries, reloads, refetches or advances on its own', async ({ page }) => {
	const { audioRequests } = await boot(page);
	await startLoading(page);
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	const before = await counts(page);
	const src = await source(page);
	const queue = await queueIds(page);
	const media = audioRequests.length;
	for (const type of ['waiting', 'waiting', 'stalled']) await event(page, type);
	await advance(page, 60_000);
	await expect(notice(page)).toBeVisible();
	await expect(notice(page)).toHaveCount(1);
	const after = await counts(page);
	expect(after.play, 'LW-B02: never auto-retry').toBe(before.play);
	expect(after.load, 'LW-B02: never auto-load').toBe(before.load);
	expect(audioRequests, 'LW-B02: no extra automatic media requests').toHaveLength(media);
	expect(await source(page)).toBe(src);
	expect(await queueIds(page)).toEqual(queue);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await status(page, 'loading');
	expect(await noticeRises(page), 'LW-B02: one transition only, no duplicates').toBe(1);
});

test('LW-B03 a playing->waiting interruption waits a fresh 15s and recovery cancels it', async ({ page }) => {
	await boot(page);
	await startPlaying(page);
	await event(page, 'waiting');
	await status(page, 'buffering');
	await advance(page, 10_000);
	await expect(notice(page)).toHaveCount(0);
	await event(page, 'playing');
	await status(page, 'playing');
	await advance(page, 10_000); // past the first wait's would-be deadline
	await expect(notice(page), 'LW-B03: recovery cancelled the timer').toHaveCount(0);
	await event(page, 'waiting');
	await status(page, 'buffering');
	await advance(page, 14_000);
	await expect(notice(page), 'LW-B03: the repeated wait restarts in full').toHaveCount(0);
	await advance(page, 2_000);
	await expect(notice(page)).toBeVisible();
	await status(page, 'buffering');
	expect((await counts(page)).play).toBe(1);
});

test('LW-B04 stalled alone with playable audio never starts the wait', async ({ page }) => {
	await boot(page);
	await startPlaying(page);
	await event(page, 'stalled');
	await event(page, 'stalled');
	await advance(page, 30_000);
	await expect(notice(page), 'LW-B04: stalled alone is not enough').toHaveCount(0);
	await status(page, 'playing');
	await event(page, 'waiting');
	await event(page, 'stalled'); // neither cancels nor restarts a real wait
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	await status(page, 'buffering');
});

test('LW-B05 Retry playback makes exactly one explicit attempt on the still-current source', async ({ page }) => {
	await boot(page);
	await startLoading(page);
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	const src = await source(page);
	const queue = await queueIds(page);
	await retryButton(page).click();
	await expect(notice(page), 'LW-B05: retry clears the notice at once').toHaveCount(0);
	await status(page, 'loading');
	expect((await counts(page)).play, 'LW-B05: one explicit attempt, no more').toBe(2);
	expect(await source(page), 'LW-B05: still the current demo source').toBe(src);
	expect(await queueIds(page)).toEqual(queue);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await settlePlay(page); // a fulfilled play promise is not playing
	await status(page, 'loading');
	await advance(page, 16_000);
	await expect(notice(page), 'LW-B05: the retried attempt gets its own full wait').toBeVisible();
	expect((await counts(page)).play).toBe(2);
	expect(await noticeRises(page)).toBe(2);
});

test('LW-B06 Pause on the notice cancels intent with no new play/load/fetch and fences late settlement', async ({ page }) => {
	const { audioRequests } = await boot(page);
	await startLoading(page);
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	const before = await counts(page);
	const media = audioRequests.length;
	await pauseButton(page).click();
	await status(page, 'paused');
	await expect(notice(page)).toHaveCount(0);
	const after = await counts(page);
	expect(after.play, 'LW-B06: pause starts no new play').toBe(before.play);
	expect(after.load, 'LW-B06: pause starts no new load').toBe(before.load);
	expect(after.fetch, 'LW-B06: pause starts no new fetch').toBe(before.fetch);
	expect(audioRequests).toHaveLength(media);
	await settlePlay(page); // the abandoned attempt settles late
	for (const type of ['playing', 'waiting', 'stalled']) await event(page, type);
	await status(page, 'paused');
	await advance(page, 30_000);
	await expect(notice(page), 'LW-B06: no resurrection after pause').toHaveCount(0);
	expect((await counts(page)).play).toBe(before.play);
});

test('LW-B07 pausing before the deadline cancels the pending notice', async ({ page }) => {
	await boot(page);
	await startPlaying(page);
	await event(page, 'waiting');
	await status(page, 'buffering');
	await advance(page, 10_000);
	await page.getByTestId('player-next').focus();
	await page.keyboard.press('k');
	await status(page, 'paused');
	await advance(page, 30_000);
	await expect(notice(page), 'LW-B07: no active intent, no notice').toHaveCount(0);
	expect((await counts(page)).play).toBe(1);
});

test('LW-B08 track replacement clears the old notice and times only the new source', async ({ page }) => {
	await boot(page);
	await startLoading(page);
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	const old = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await status(page, 'loading');
	await expect(notice(page), 'LW-B08: replacement clears notice and timer').toHaveCount(0);
	for (const type of ['waiting', 'playing', 'stalled']) {
		await old!.evaluate((el, name) => el.dispatchEvent(new Event(name)), type);
	}
	await advance(page, 14_000);
	await expect(notice(page), 'LW-B08: the old attempt cannot show for the new track').toHaveCount(0);
	await advance(page, 2_000);
	await expect(notice(page)).toBeVisible();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect((await counts(page)).play).toBe(2);
});

test('LW-B09 an error supersedes the notice and waiting alone never re-shows it', async ({ page }) => {
	await boot(page);
	await startPlaying(page);
	await event(page, 'waiting');
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	await event(page, 'error');
	await status(page, 'error');
	await expect(page.getByTestId('player-error')).toHaveRole('alert');
	await expect(notice(page), 'LW-B09: the error path owns recovery now').toHaveCount(0);
	await advance(page, 30_000);
	await expect(notice(page)).toHaveCount(0);
	expect((await counts(page)).play).toBe(1);
});

test('LW-B10 a natural end before the deadline restarts timing for the next source only', async ({ page }) => {
	await boot(page);
	await startPlaying(page);
	await event(page, 'waiting');
	await advance(page, 10_000);
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await status(page, 'loading');
	await advance(page, 6_000); // past the ended track's would-be deadline
	await expect(notice(page), 'LW-B10: the ended source deadline is fenced').toHaveCount(0);
	await advance(page, 10_000); // 16s into the next source's initial wait
	await expect(notice(page)).toBeVisible();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
});

test('LW-B11 clearing the queue (unload) clears the notice and fences the released element', async ({ page }) => {
	await boot(page);
	await startLoading(page);
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	const old = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('queue-clear').click();
	await status(page, 'empty');
	await expect(notice(page)).toHaveCount(0);
	for (const type of ['waiting', 'playing', 'stalled']) {
		await old!.evaluate((el, name) => el.dispatchEvent(new Event(name)), type);
	}
	await advance(page, 30_000);
	await expect(notice(page), 'LW-B11: teardown leaves nothing to resurrect').toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('LW-B12 the notice keeps focus and native controls and is one polite transition', async ({ page }) => {
	await boot(page);
	await startLoading(page);
	const next = page.getByTestId('player-next');
	await next.focus();
	await expect(next).toBeFocused();
	await advance(page, 16_000);
	await expect(notice(page)).toBeVisible();
	await expect(next, 'LW-B12: showing the notice never steals focus').toBeFocused();
	const audio = page.getByTestId('demo-audio');
	await expect(audio).toBeVisible();
	expect(
		await audio.evaluate((el) => el.hasAttribute('controls')),
		'LW-B12: native controls are retained'
	).toBe(true);
	await expect(retryButton(page)).toBeEnabled();
	await expect(pauseButton(page)).toBeEnabled();
	await retryButton(page).focus();
	await expect(retryButton(page), 'LW-B12: actions are keyboard reachable').toBeFocused();
	await event(page, 'waiting');
	await advance(page, 30_000);
	expect(await noticeRises(page), 'LW-B12: still exactly one announced transition').toBe(1);
});

test('LW-B13 negative control: with no active play intent, time alone shows nothing', async ({ page }) => {
	await boot(page);
	await advance(page, 30_000);
	await expect(notice(page)).toHaveCount(0);
	await status(page, 'selected');
	expect((await counts(page)).play).toBe(0);
});

test('LW-B14 a background-delayed clock jump shows the notice once for the still-current source', async ({ page }) => {
	await boot(page);
	await startLoading(page);
	// Background throttling: time passes with no callbacks, then one runs.
	await page.clock.setSystemTime(new Date(epoch.getTime() + 60_000));
	await page.clock.runFor(1_000);
	await expect(notice(page)).toBeVisible();
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await status(page, 'loading');
	expect((await counts(page)).play).toBe(1);
	expect(await noticeRises(page)).toBe(1);
});
