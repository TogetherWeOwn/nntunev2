import { expect, test, type Page } from '@playwright/test';

/**
 * Protected, offline acceptance: native media boundary is mocked, real page
 * handlers/rendering/queue/shortcuts are not. No decoder or live stall needed.
 * Existing player-status is the concise, visible polite live/status region.
 * Only text semantics are pinned; no spinner, animation, or exact recovery copy.
 */
type Control = {
	resolve: () => void;
	counts: { play: number; pause: number; load: number; seek: number; timers: number; fetch: number; xhr: number };
	recording: boolean;
	calibrating: boolean;
	eventTimerSite: string | null;
	announcements: string[];
};

async function openPlayer(page: Page) {
	const requests: string[] = [];
	page.on('request', (request) => requests.push(request.url()));
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (url.pathname === '/api/demo/audio') return; // Synthetic pending media.
		if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		sessionStorage.clear();
		localStorage.clear();
		const control: Control = {
			resolve: () => {}, recording: false, calibrating: false, eventTimerSite: null, announcements: [],
			counts: { play: 0, pause: 0, load: 0, seek: 0, timers: 0, fetch: 0, xhr: 0 }
		};
		Object.assign(window, { __buffering: control });
		const positions = new WeakMap<HTMLMediaElement, number>();
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get() { return positions.get(this) ?? 0; },
			set(value: number) { control.counts.seek++; positions.set(this, value); }
		});
		HTMLMediaElement.prototype.play = function () {
			control.counts.play++;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return new Promise<void>((resolve) => { control.resolve = resolve; });
		};
		HTMLMediaElement.prototype.pause = function () {
			control.counts.pause++;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		HTMLMediaElement.prototype.load = function () { control.counts.load++; };
		const timeout = window.setTimeout.bind(window);
		const interval = window.setInterval.bind(window);
		window.setTimeout = ((handler: TimerHandler, ms?: number, ...args: unknown[]) => {
			if (control.recording) {
				// Svelte already schedules event-dispatch cleanup on playing.
				// Learn that one callsite at initial start, not private source text.
				// Other timeout sites and every interval remain forbidden.
				const site = new Error().stack?.split('\n')[2] ?? null;
				if (control.calibrating && control.eventTimerSite === null) control.eventTimerSite = site;
				if (site === null || site !== control.eventTimerSite) control.counts.timers++;
			}
			return timeout(handler, ms, ...args);
		}) as typeof window.setTimeout;
		window.setInterval = ((handler: TimerHandler, ms?: number, ...args: unknown[]) => {
			if (control.recording) control.counts.timers++;
			return interval(handler, ms, ...args);
		}) as typeof window.setInterval;
		const fetch = window.fetch.bind(window);
		window.fetch = (...args) => {
			if (control.recording) control.counts.fetch++;
			return fetch(...args);
		};
		const send = XMLHttpRequest.prototype.send;
		XMLHttpRequest.prototype.send = function (body) {
			if (control.recording) control.counts.xhr++;
			return send.call(this, body);
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	return requests;
}

async function event(page: Page, name: string, calibrating = false) {
	await page.getByTestId('demo-audio').evaluate(async (el, { type, calibrating }) => {
		const control = (window as unknown as { __buffering: Control }).__buffering;
		control.calibrating = calibrating;
		if (type === 'play' || type === 'pause') {
			Object.defineProperty(el, 'paused', { value: type === 'pause', configurable: true });
		}
		// Attribute scheduled work to this event/flush, not Playwright's own
		// locator polling or unrelated already-running page maintenance.
		control.recording = true;
		el.dispatchEvent(new Event(type));
		await Promise.resolve();
		await Promise.resolve();
		control.recording = false;
		control.calibrating = false;
	}, { type: name, calibrating });
}

async function settle(page: Page) {
	await page.evaluate(() => (window as unknown as { __buffering: Control }).__buffering.resolve());
}

async function start(page: Page, id = 'demo-track') {
	await page.getByTestId(`queue-select-${id}`).click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await settle(page);
	await event(page, 'playing', true);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}

async function status(page: Page, state: string) {
	await expect(page.getByTestId('player-status')).toHaveText(new RegExp(`^Status: ${state}$`, 'i'));
}

async function snapshot(page: Page) {
	return page.evaluate(() => {
		const control = (window as unknown as { __buffering: Control }).__buffering;
		const audio = document.querySelector<HTMLAudioElement>('[data-testid="demo-audio"]');
		return {
			counts: { ...control.counts }, time: audio?.currentTime,
			source: audio?.getAttribute('src'),
			queue: Array.from(document.querySelectorAll('[data-testid="queue-item"]'), (el) => el.getAttribute('data-track-id'))
		};
	});
}

async function observeAnnouncements(page: Page) {
	await page.getByTestId('player-status').evaluate((el) => {
		const control = (window as unknown as { __buffering: Control }).__buffering;
		control.announcements = [];
		new MutationObserver(() => control.announcements.push(el.textContent?.trim() ?? ''))
			.observe(el, { subtree: true, childList: true, characterData: true });
	});
}

async function announcements(page: Page) {
	return page.evaluate(() => [...(window as unknown as { __buffering: Control }).__buffering.announcements]);
}

async function naturalEnd(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}

test('BUF-B01 current waiting shows visible accessible buffering, then playing recovers', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	await event(page, 'waiting');
	await status(page, 'buffering');
	const region = page.getByTestId('player-status');
	await expect(region).toBeVisible();
	expect(await region.evaluate((el) => el.getAttribute('role') === 'status' || el.getAttribute('aria-live') === 'polite')).toBe(true);
	await expect(region).not.toHaveAttribute('aria-live', /^(assertive|off)$/);
	await expect(region).not.toHaveAttribute('aria-hidden', 'true');
	await expect(page.locator('[data-testid="queue-item"][data-track-id="demo-track"]')).not.toContainText('(playing)');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await event(page, 'playing');
	await status(page, 'playing');
	await expect(page.locator('[data-testid="queue-item"][data-track-id="demo-track"]')).toContainText('(playing)');
});

test('BUF-B02 initial waits stay selected/loading and never fabricate provider failures', async ({ page }) => {
	await openPlayer(page);
	await event(page, 'waiting');
	await event(page, 'stalled');
	await status(page, 'selected');
	await page.getByTestId('queue-select-demo-track').click();
	for (const type of ['waiting', 'waiting', 'stalled']) await event(page, type);
	await status(page, 'loading');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await expect(page.getByTestId('player-retry')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).not.toContainText(/provider|network|offline|timeout/i);
	await settle(page);
	await event(page, 'playing');
	await event(page, 'waiting');
	await status(page, 'buffering');
});

test('BUF-B03 duplicate waits and duplicate playing events announce only entry/recovery transitions', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	await observeAnnouncements(page);
	for (const type of ['waiting', 'waiting', 'stalled', 'waiting']) await event(page, type);
	await status(page, 'buffering');
	let values = await announcements(page);
	expect(values).toHaveLength(1);
	expect(values[0]).toMatch(/buffering/i);
	expect(values[0]!.length).toBeLessThanOrEqual(120);
	await event(page, 'playing');
	await event(page, 'playing');
	await status(page, 'playing');
	values = await announcements(page);
	expect(values).toHaveLength(2);
	expect(values[1]).toMatch(/playing|resum/i);
	await event(page, 'waiting');
	await event(page, 'playing');
	expect(await announcements(page)).toHaveLength(4);
});

test('BUF-B04 stalled alone keeps playing and is not a buffering recovery signal', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	await observeAnnouncements(page);
	await event(page, 'stalled');
	await event(page, 'stalled');
	await status(page, 'playing');
	expect(await announcements(page)).toEqual([]);
	await event(page, 'waiting');
	await status(page, 'buffering');
	await event(page, 'stalled');
	await status(page, 'buffering');
});

test('BUF-B05 buffering never retries, loads, seeks, schedules work, or changes queue/source', async ({ page }) => {
	const requests = await openPlayer(page);
	await start(page);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => { el.currentTime = 42; });
	const before = await snapshot(page);
	const requestCount = requests.length;
	for (const type of ['waiting', 'waiting', 'stalled', 'playing', 'waiting']) await event(page, type);
	expect(await snapshot(page)).toEqual(before);
	expect(requests).toHaveLength(requestCount);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('player-retry')).toHaveCount(0);
	await status(page, 'buffering');
});

test('BUF-B06 keyboard pause wins over buffering and queued playing/waiting without moving focus', async ({ page }) => {
	await page.emulateMedia({ reducedMotion: 'reduce' });
	await openPlayer(page);
	await start(page);
	const next = page.getByTestId('player-next');
	const unfocusedBorder = await next.evaluate((el) => getComputedStyle(el).borderColor);
	await next.focus();
	await page.keyboard.press('Tab');
	await page.keyboard.press('Shift+Tab');
	await expect(next).toBeFocused();
	await event(page, 'waiting');
	await status(page, 'buffering');
	await expect(next).toBeFocused();
	await expect(next).toBeEnabled();
	await expect(next).toHaveAccessibleName(/next/i);
	expect(await next.evaluate((el, border) => {
		const css = getComputedStyle(el);
		return el.matches(':focus-visible') && (
			(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) ||
			css.boxShadow !== 'none' || css.borderColor !== border
		);
	}, unfocusedBorder)).toBe(true);
	const calls = (await snapshot(page)).counts.play;
	await page.keyboard.press('k');
	await status(page, 'paused');
	for (const type of ['waiting', 'playing', 'stalled']) await event(page, type);
	await status(page, 'paused');
	await expect(next).toBeFocused();
	expect((await snapshot(page)).counts.play).toBe(calls);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('BUF-B07 native pause settles buffering and native resume permits a fresh buffering transition', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	await event(page, 'waiting');
	await status(page, 'buffering');
	await event(page, 'pause');
	await event(page, 'waiting');
	await event(page, 'playing');
	await status(page, 'paused');
	await event(page, 'play');
	await event(page, 'playing');
	await status(page, 'playing');
	await event(page, 'waiting');
	await status(page, 'buffering');
});

test('BUF-B08 error replaces buffering; only explicit retry replaces the element and resets prior-playing history', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	const old = await page.getByTestId('demo-audio').elementHandle();
	await event(page, 'waiting');
	await status(page, 'buffering');
	await event(page, 'error');
	await status(page, 'error');
	await expect(page.getByTestId('player-error')).toHaveRole('alert');
	const retry = page.getByTestId('player-retry');
	await expect(retry).toHaveAccessibleName(/retry/i);
	const calls = (await snapshot(page)).counts.play;
	for (const type of ['waiting', 'playing', 'stalled']) await event(page, type);
	await status(page, 'error');
	expect((await snapshot(page)).counts.play).toBe(calls);
	await retry.focus();
	await page.keyboard.press('Enter');
	expect(await old!.evaluate((el) => el.isConnected)).toBe(false);
	await status(page, 'loading');
	await event(page, 'waiting');
	await status(page, 'loading');
	await settle(page);
	await event(page, 'playing');
	await event(page, 'waiting');
	for (const type of ['waiting', 'playing', 'stalled']) await old!.evaluate((el, name) => el.dispatchEvent(new Event(name)), type);
	await status(page, 'buffering');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await event(page, 'playing');
	await status(page, 'playing');
});

test('BUF-B09 track replacement fences detached waits and detached recovery events', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	const old = await page.getByTestId('demo-audio').elementHandle();
	await event(page, 'waiting');
	await status(page, 'buffering');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await status(page, 'loading');
	for (const type of ['waiting', 'playing', 'stalled']) await old!.evaluate((el, name) => el.dispatchEvent(new Event(name)), type);
	await event(page, 'waiting');
	await status(page, 'loading');
	await settle(page);
	await event(page, 'playing');
	await old!.evaluate((el) => el.dispatchEvent(new Event('waiting')));
	await status(page, 'playing');
	await event(page, 'waiting');
	for (const type of ['playing', 'stalled', 'waiting']) await old!.evaluate((el, name) => el.dispatchEvent(new Event(name)), type);
	await status(page, 'buffering');
});

test('BUF-B10 clearing buffering leaves empty state immune to detached events', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	const old = await page.getByTestId('demo-audio').elementHandle();
	await event(page, 'waiting');
	await status(page, 'buffering');
	await page.getByTestId('queue-clear').click();
	for (const type of ['waiting', 'playing', 'stalled']) await old!.evaluate((el, name) => el.dispatchEvent(new Event(name)), type);
	await status(page, 'empty');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await page.getByTestId('queue-add-third-light').click();
	// First add to an empty queue auto-starts it (addToQueue calls
	// playCurrent): the repopulated track is loading on the pending
	// mocked play, and a pre-playing wait must not enter buffering.
	await status(page, 'loading');
	await event(page, 'waiting');
	await status(page, 'loading');
});

test('BUF-B11 natural end from buffering advances once, while end-of-track sleep still pauses', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	await event(page, 'waiting');
	await status(page, 'buffering');
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await status(page, 'loading');
	expect((await snapshot(page)).counts.play).toBe(2);
	await settle(page);
	await event(page, 'playing');
	await page.getByTestId('sleep-timer-set-end-of-track').click();
	await event(page, 'waiting');
	await naturalEnd(page);
	await status(page, 'paused');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
	expect((await snapshot(page)).counts.play).toBe(2);
});

test('BUF-B12 duration sleep expires during buffering without resuming or advancing', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	await page.getByTestId('sleep-timer-minutes').fill('1');
	await page.getByTestId('sleep-timer-set-duration').click();
	await event(page, 'waiting');
	await status(page, 'buffering');
	await page.evaluate(() => {
		(window as unknown as { __comforts: { advanceSleep: (ms: number) => void } }).__comforts.advanceSleep(61_000);
	});
	for (const type of ['waiting', 'playing', 'stalled']) await event(page, type);
	await status(page, 'paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect((await snapshot(page)).counts.play).toBe(1);
});

test('BUF-B13 waiting cannot start radio early; a real final end still continues radio', async ({ page }) => {
	await openPlayer(page);
	await start(page, 'third-light');
	await event(page, 'waiting');
	await status(page, 'buffering');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	expect((await snapshot(page)).counts.play).toBe(1);
	await naturalEnd(page);
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	await status(page, 'loading');
	await settle(page);
	await event(page, 'playing');
	await event(page, 'waiting');
	await status(page, 'buffering');
	await expect(page.getByTestId('radio-stop')).toBeEnabled();
	await page.getByTestId('radio-stop').click();
	await status(page, 'ended');
	await event(page, 'waiting');
	await event(page, 'playing');
	await status(page, 'ended');
});

test('BUF-B14 negative control: stalled while audible changes neither status nor transport', async ({ page }) => {
	await openPlayer(page);
	await start(page);
	await observeAnnouncements(page);
	const before = await snapshot(page);
	await event(page, 'stalled');
	await event(page, 'stalled');
	await status(page, 'playing');
	expect(await snapshot(page)).toEqual(before);
	expect(await announcements(page)).toEqual([]);
	await expect(page.getByTestId('player-error')).toHaveCount(0);
});

test('BUF-B15 negative control: a pending initial play remains loading without retry or diagnosis', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await status(page, 'loading');
	await event(page, 'play', true); // Native play is still not a playing event.
	const before = await snapshot(page);
	for (const type of ['waiting', 'waiting', 'stalled']) await event(page, type);
	await status(page, 'loading');
	expect(await snapshot(page)).toEqual(before);
	await expect(page.getByTestId('player-retry')).toHaveCount(0);
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).not.toContainText(/provider|network|offline|timeout/i);
});
