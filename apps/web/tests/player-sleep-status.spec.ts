import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Protected test-first human-UX contract; do not edit this or neighboring suites.
 * Existing comfort/transport hooks remain unchanged. ONE additive DOM seam:
 * `sleep-timer-remaining` is visible text showing remaining m:ss / h:mm:ss
 * (ceil fractional seconds, never negative), end-of-track, or N tracks remaining.
 * It has no live ancestor/descendant. Existing sleep-timer-status stays polite
 * and announces meaningful transitions, not elapsed seconds. Input errors are
 * visible, associated by aria-describedby/aria-errormessage and actionable; no
 * exact error wording, new validation helper or refresh implementation is pinned.
 *
 * Only native media I/O is stubbed. Playwright's clock drives real Date/timers;
 * jump-without-ticks + visibility tests background resync, not the old offset
 * hook. No providers, persistence fixture, production, OS IME or real-decoder
 * claims. IDs SLP-Bxx are stable. Test-only timer instrumentation accepts timeout,
 * interval or animation-frame refresh; callbacks can be replayed after cleanup.
 */
type Task = { kind: 'interval' | 'timeout' | 'frame'; fire: () => void; callback: TimerHandler };
type Probe = {
	plays: number;
	pauses: number;
	tasks: Map<number, Task>;
	background: Set<TimerHandler>;
	stale: (() => void)[];
	announcements: string[];
};
type ProbeWindow = Window & { __sleepStatus: Probe };
const epoch = new Date('2026-01-01T00:00:00Z');
const remaining = (page: Page) => page.getByTestId('sleep-timer-remaining');
const announcement = (page: Page) => page.getByTestId('sleep-timer-status');

test.use({ actionTimeout: 5_000, reducedMotion: 'reduce', hasTouch: true });

async function boot(page: Page) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.clock.install({ time: epoch });
	await page.clock.pauseAt(epoch);
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (url.hostname !== 'localhost' && url.hostname !== '127.0.0.1') return route.abort();
		if (url.pathname === '/api/demo/audio') return; // media intentionally pending
		return route.continue();
	});
	await page.addInitScript(() => {
		const probe: Probe = { plays: 0, pauses: 0, tasks: new Map(), background: new Set(), stale: [], announcements: [] };
		Object.assign(window, { __sleepStatus: probe });
		HTMLMediaElement.prototype.play = function () {
			probe.plays += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauses += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		const interval = window.setInterval.bind(window);
		const timeout = window.setTimeout.bind(window);
		const clearInterval = window.clearInterval.bind(window);
		const clearTimeout = window.clearTimeout.bind(window);
		const frame = window.requestAnimationFrame.bind(window);
		const cancelFrame = window.cancelAnimationFrame.bind(window);
		window.setInterval = ((callback: TimerHandler, delay?: number, ...args: unknown[]) => {
			const fire = () => { if (typeof callback === 'function') callback(...args); };
			const id = interval(fire, delay);
			probe.tasks.set(id, { kind: 'interval', fire, callback });
			return id;
		}) as typeof window.setInterval;
		window.setTimeout = ((callback: TimerHandler, delay?: number, ...args: unknown[]) => {
			const fire = () => { if (typeof callback === 'function') callback(...args); };
			const id = timeout(() => { probe.tasks.delete(id); fire(); }, delay);
			probe.tasks.set(id, { kind: 'timeout', fire, callback });
			return id;
		}) as typeof window.setTimeout;
		window.requestAnimationFrame = (callback) => {
			const fire = () => callback(performance.now());
			const id = frame(() => { probe.tasks.delete(id); fire(); });
			probe.tasks.set(id, { kind: 'frame', fire, callback });
			return id;
		};
		window.clearInterval = (id) => { probe.tasks.delete(id!); clearInterval(id); };
		window.clearTimeout = (id) => { probe.tasks.delete(id!); clearTimeout(id); };
		window.cancelAnimationFrame = (id) => { probe.tasks.delete(id); cancelFrame(id); };
	});
	await page.goto('/demo/player?radio-seed=7');
	await page.waitForFunction(() => typeof (
		window as unknown as { __comforts?: { advanceSleep?: unknown } }
	).__comforts?.advanceSleep === 'function');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await page.evaluate(() => {
		const p = (window as unknown as ProbeWindow).__sleepStatus;
		// Existing framework one-shot tasks (e.g. app-version polling) belong
		// to the document, not this feature. Follow their callback identity if
		// rescheduled; never replay them as stale sleep callbacks. Existing
		// intervals include the real sleep authority and remain under test.
		p.background = new Set([...p.tasks.values()].filter((task) => task.kind === 'timeout').map((task) => task.callback));
	});
	return errors;
}

async function start(page: Page, id = 'demo-track') {
	await page.getByTestId(`queue-select-${id}`).click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}
async function duration(page: Page, minutes: string) {
	await page.getByTestId('sleep-timer-minutes').fill(minutes);
	await page.getByTestId('sleep-timer-set-duration').click();
	await expect(page.getByTestId('sleep-timer-cancel')).toBeEnabled();
}
async function end(page: Page) {
	await page.getByTestId('demo-audio').evaluate((audio) => {
		Object.defineProperty(audio, 'paused', { value: true, configurable: true });
		Object.defineProperty(audio, 'ended', { value: true, configurable: true });
		audio.dispatchEvent(new Event('pause'));
		audio.dispatchEvent(new Event('ended'));
	});
}
async function probe(page: Page) {
	return page.evaluate(() => {
		const { plays, pauses, tasks, background } = (window as unknown as ProbeWindow).__sleepStatus;
		return { plays, pauses, tasks: [...tasks].filter(([, task]) => !background.has(task.callback)).map(([id]) => id) };
	});
}
async function order(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}
async function visibleRemaining(page: Page, pattern: RegExp) {
	// Missing new presentation is an explicit assertion failure, not a missing
	// method/selector action that prevents testing existing pause/queue controls.
	await expect.soft.poll(async () => (await remaining(page).allTextContents()).join(' '), {
		message: 'Show truthful remaining sleep state, separately from announcements', timeout: 700
	}).toMatch(pattern);
	if (await remaining(page).count()) await expect.soft(remaining(page)).toBeVisible();
}
async function noLiveCountdown(page: Page) {
	const live = await remaining(page).evaluateAll((nodes) => nodes.some((node) => {
		const selector = '[aria-live]:not([aria-live="off"]), [role="status"], [role="alert"], [role="log"]';
		return Boolean(node.closest(selector) || node.querySelector(selector));
	}));
	expect.soft(live, 'Per-second text must not be inside or contain a live region').toBe(false);
}
async function feedback(input: Locator, kind: 'minutes' | 'tracks') {
	await expect.soft(input, 'Rejected values need input-associated invalid state').toHaveAttribute('aria-invalid', 'true', { timeout: 500 });
	const descriptions = await input.evaluate((el) => {
		const ids = `${el.getAttribute('aria-describedby') ?? ''} ${el.getAttribute('aria-errormessage') ?? ''}`.trim().split(/\s+/);
		return ids.flatMap((id) => {
			const node = document.getElementById(id);
			return node && node.getClientRects().length ? [node.textContent ?? ''] : [];
		}).join(' ');
	});
	expect.soft(descriptions, 'A visible inline error must say what to enter next').toMatch(/enter|choose|use|provide|try|type|select/i);
	expect.soft(descriptions).toMatch(kind === 'minutes' ? /minute|duration/i : /track|count/i);
	expect.soft(descriptions).toMatch(kind === 'minutes' ? /positive|greater|finite|number|valid|large/i : /whole|integer|positive|greater|valid/i);
	return descriptions;
}
async function noFeedback(input: Locator, staleDescription = '') {
	expect(await input.getAttribute('aria-invalid')).not.toBe('true');
	// Guidance may remain, but the old associated error may not remain visible.
	const messages = await input.evaluate((el) => {
		const ids = `${el.getAttribute('aria-describedby') ?? ''} ${el.getAttribute('aria-errormessage') ?? ''}`.trim().split(/\s+/);
		return ids.flatMap((id) => {
			const node = document.getElementById(id);
			return node && node.getClientRects().length ? [node.textContent ?? ''] : [];
		}).join(' ');
	});
	if (staleDescription) expect(messages, 'Successful set/cancel clears stale inline feedback').not.toBe(staleDescription);
	const errorIds = await input.getAttribute('aria-errormessage');
	if (errorIds) {
		const visibleError = await input.evaluate((el) => (el.getAttribute('aria-errormessage') ?? '').split(/\s+/).some((id) => {
			const node = document.getElementById(id);
			return Boolean(node?.getClientRects().length && node.textContent?.trim());
		}));
		expect(visibleError).toBe(false);
	}
}
async function rememberCallbacks(page: Page) {
	await page.evaluate(() => {
		const p = (window as unknown as ProbeWindow).__sleepStatus;
		p.stale.push(...[...p.tasks.values()].filter((task) => !p.background.has(task.callback)).map((task) => task.fire));
	});
}
async function replayCallbacks(page: Page) {
	await page.evaluate(() => {
		const p = (window as unknown as ProbeWindow).__sleepStatus;
		for (const callback of [...p.stale]) callback();
	});
}
async function visibility(page: Page, state: 'hidden' | 'visible') {
	await page.evaluate((value) => {
		Object.defineProperty(document, 'visibilityState', { configurable: true, value });
		Object.defineProperty(document, 'hidden', { configurable: true, value: value === 'hidden' });
		document.dispatchEvent(new Event('visibilitychange'));
	}, state);
}

test('SLP-B01 wall-clock display changes without per-second announcements or transport', async ({ page }) => {
	const errors = await boot(page);
	await start(page);
	await duration(page, '1');
	await visibleRemaining(page, /\b1:00\b/);
	await noLiveCountdown(page);
	await expect(announcement(page)).toHaveAttribute('aria-live', 'polite');
	const said = await announcement(page).innerText();
	await announcement(page).evaluate((node) => {
		new MutationObserver(() => {
			(window as unknown as ProbeWindow).__sleepStatus.announcements.push(node.textContent ?? '');
		}).observe(node, { subtree: true, childList: true, characterData: true });
	});
	const before = await probe(page);
	const queue = await order(page);
	await page.getByTestId('sleep-timer-minutes').focus();
	for (const display of [/\b0:59\b/, /\b0:58\b/, /\b0:57\b/]) {
		await page.clock.runFor(1_000);
		await visibleRemaining(page, display);
		await expect(announcement(page)).toHaveText(said);
		await expect(page.getByTestId('sleep-timer-minutes')).toBeFocused();
	}
	expect((await probe(page)).plays).toBe(before.plays);
	expect((await probe(page)).pauses).toBe(before.pauses);
	expect(await page.evaluate(() => (window as unknown as ProbeWindow).__sleepStatus.announcements), 'No per-tick live-region mutation, including transient rewrites').toEqual([]);
	expect(await order(page)).toEqual(queue);
	await expect(page.getByTestId('comforts-panel')).toContainText(/wall.clock|continues? (while|when) paused|even (while|when) paused/i);
	expect(errors).toEqual([]);
});

test('SLP-B02 fractional minutes expire once even while playback is paused', async ({ page }) => {
	await boot(page);
	await start(page);
	await duration(page, '0.5');
	await visibleRemaining(page, /\b0:30\b/);
	await page.getByTestId('demo-audio').evaluate((audio) => {
		Object.defineProperty(audio, 'paused', { value: true, configurable: true });
		audio.dispatchEvent(new Event('pause'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await page.clock.runFor(15_000);
	await visibleRemaining(page, /\b0:15\b/);
	await page.clock.runFor(15_000);
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
	await expect(announcement(page)).toContainText(/paused|expir|fired/i);
	const after = await probe(page);
	await page.clock.runFor(120_000);
	expect((await probe(page)).pauses).toBe(after.pauses);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});

test('SLP-B03 h:mm:ss formatting and no arbitrary new valid-minute ceiling', async ({ page }) => {
	const errors = await boot(page);
	await duration(page, '61.5');
	await visibleRemaining(page, /\b1:01:30\b/);
	await page.clock.runFor(29_000);
	await visibleRemaining(page, /\b1:01:01\b/);
	await duration(page, '10000');
	await expect(page.getByTestId('sleep-timer-cancel')).toBeEnabled();
	await noFeedback(page.getByTestId('sleep-timer-minutes'));
	await visibleRemaining(page, /\b166:40:00\b/);
	expect(errors).toEqual([]);
});

test('SLP-B04 visibility resync skips missed ticks, clamps and pauses exactly once', async ({ page }) => {
	await boot(page);
	await start(page);
	await duration(page, '2');
	const before = await probe(page);
	await rememberCallbacks(page);
	await visibility(page, 'hidden');
	await page.clock.setSystemTime(new Date(epoch.getTime() + 75_000));
	await visibility(page, 'visible');
	await page.clock.runFor(1_000); // at most one ordinary refresh, not 76 missed ticks
	await visibleRemaining(page, /\b0:44\b/);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await visibility(page, 'hidden');
	await page.clock.setSystemTime(new Date(epoch.getTime() + 300_000));
	await visibility(page, 'visible');
	await page.clock.runFor(1_000);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
	await replayCallbacks(page);
	await page.clock.runFor(5_000);
	const after = await probe(page);
	expect(after.pauses - before.pauses, 'A delayed refresh cannot become a second expiry authority').toBe(1);
	expect(after.plays).toBe(before.plays);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	const text = (await remaining(page).allTextContents()).join(' ');
	expect(text).not.toMatch(/-\d|NaN|Infinity/);
});

for (const item of [
	{ id: 'SLP-B05a', kind: 'minutes', value: '', reason: 'blank' },
	{ id: 'SLP-B05b', kind: 'minutes', value: '0', reason: 'zero' },
	{ id: 'SLP-B05c', kind: 'minutes', value: '-2', reason: 'negative' },
	{ id: 'SLP-B05d', kind: 'minutes', value: '1e309', reason: 'non-finite numeric entry' },
	{ id: 'SLP-B05e', kind: 'minutes', value: '1e308', reason: 'finite minutes overflow ms' },
	{ id: 'SLP-B06a', kind: 'tracks', value: '', reason: 'blank' },
	{ id: 'SLP-B06b', kind: 'tracks', value: '0', reason: 'zero' },
	{ id: 'SLP-B06c', kind: 'tracks', value: '-1', reason: 'negative' },
	{ id: 'SLP-B06d', kind: 'tracks', value: '1.5', reason: 'noninteger' },
	{ id: 'SLP-B06e', kind: 'tracks', value: '1e309', reason: 'non-finite numeric entry' }
] as const) {
	test(`${item.id} ${item.kind} ${item.reason}: inline next action, original deadline survives`, async ({ page }) => {
		const errors = await boot(page);
		await start(page);
		await duration(page, '1');
		await page.clock.runFor(15_000);
		const input = page.getByTestId(`sleep-timer-${item.kind}`);
		// Real number inputs sanitize out-of-range floating-point entry to blank;
		// type it through Chromium, rather than forge impossible `.value` state.
		await input.fill('');
		if (item.value === '1e309') await input.pressSequentially(item.value);
		else await input.fill(item.value);
		const set = page.getByTestId(item.kind === 'minutes' ? 'sleep-timer-set-duration' : 'sleep-timer-set-stop-after');
		await set.focus();
		await page.keyboard.press('Enter');
		await expect(set).toBeFocused();
		await feedback(input, item.kind);
		await page.clock.runFor(15_000);
		await visibleRemaining(page, /\b0:30\b/);
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await page.clock.runFor(30_000);
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
		expect.soft(errors, 'Validation is feedback, never an uncaught RangeError').toEqual([]);
	});
}

test('SLP-B07 valid set and deliberate cancel clear stale feedback on both inputs', async ({ page }) => {
	await boot(page);
	await duration(page, '1');
	const minutes = page.getByTestId('sleep-timer-minutes');
	const tracks = page.getByTestId('sleep-timer-tracks');
	const stale: string[] = [];
	for (const [input, button, kind] of [
		[minutes, 'sleep-timer-set-duration', 'minutes'],
		[tracks, 'sleep-timer-set-stop-after', 'tracks']
	] as const) {
		await input.fill('0');
		await page.getByTestId(button).click();
		stale.push(await feedback(input, kind));
	}
	await duration(page, '1.25');
	await noFeedback(minutes, stale[0]);
	await noFeedback(tracks, stale[1]);
	await visibleRemaining(page, /\b1:15\b/);
	await minutes.fill('');
	await page.getByTestId('sleep-timer-set-duration').click();
	const cancelledError = await feedback(minutes, 'minutes');
	await page.getByTestId('sleep-timer-cancel').click();
	await noFeedback(minutes, cancelledError);
	await noFeedback(tracks);
	await expect(announcement(page)).toContainText(/off|cancel/i);
});

test('SLP-B08 after-N decrements only for natural ends, politely, through edits/seeks/retry', async ({ page }) => {
	await boot(page);
	await start(page);
	await page.getByTestId('sleep-timer-tracks').fill('2');
	await page.getByTestId('sleep-timer-set-stop-after').click();
	await visibleRemaining(page, /\b2\b.*track/i);
	const said = await announcement(page).innerText();
	await page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => {
		audio.currentTime = 45;
		audio.dispatchEvent(new Event('seeking'));
		audio.dispatchEvent(new Event('seeked'));
		audio.dispatchEvent(new Event('timeupdate'));
	});
	await page.getByTestId('queue-move-up-third-light').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await page.getByTestId('demo-audio').dispatchEvent('error');
	await expect(page.getByTestId('player-status')).toHaveText('Status: error');
	await page.getByTestId('player-retry').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.clock.runFor(60_000);
	await visibleRemaining(page, /\b2\b.*track/i);
	await expect(announcement(page)).toHaveText(said);
	await page.getByTestId('sleep-timer-tracks').focus();
	await end(page);
	await visibleRemaining(page, /\b1\b.*track/i);
	await expect.soft(announcement(page), 'Announce the real remaining-track change').toContainText(/\b1\b.*track/i);
	await expect(page.getByTestId('sleep-timer-tracks')).toBeFocused();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	const queue = await order(page);
	await end(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
	expect(await order(page)).toEqual(queue);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
});

for (const repeat of ['off', 'one', 'queue'] as const) {
	test(`SLP-B09-${repeat} end-of-track sleep wins over repeat/radio without queue changes`, async ({ page }) => {
		await boot(page);
		await page.getByTestId('player-repeat').selectOption(repeat);
		await start(page, 'third-light');
		await page.getByTestId('sleep-timer-set-end-of-track').click();
		await visibleRemaining(page, /end.of.(the.)?track/i);
		const before = await probe(page);
		const queue = await order(page);
		await page.clock.runFor(120_000);
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await end(page);
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		await expect(page.getByTestId('now-playing')).toContainText('Third Light');
		expect((await probe(page)).plays).toBe(before.plays);
		expect((await probe(page)).pauses - before.pauses).toBe(1);
		expect(await order(page)).toEqual(queue);
		await expect(page.getByTestId('radio-badge')).toHaveCount(0);
		await expect(page.getByTestId('player-repeat')).toHaveValue(repeat);
	});
}

test('SLP-B10 one owned refresh, replacement/cancel/unmount cleanups and stale callbacks', async ({ page }) => {
	const errors = await boot(page);
	await page.clock.runFor(100);
	const idle = await probe(page);
	await start(page);
	await duration(page, '1');
	await rememberCallbacks(page);
	for (const value of ['2', '3', '4']) {
		await duration(page, value);
		await page.clock.runFor(100);
		expect((await probe(page)).tasks.length, 'No refresh accumulation across replacement').toBeLessThanOrEqual(idle.tasks.length + 1);
		await rememberCallbacks(page);
	}
	const replaced = await probe(page);
	await page.clock.setSystemTime(new Date(epoch.getTime() + 61_000));
	await replayCallbacks(page); // callbacks captured before replacement
	expect((await probe(page)).pauses).toBe(replaced.pauses);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('sleep-timer-cancel').click();
	await page.clock.runFor(1_100); // drain one-off browser/framework housekeeping
	expect((await probe(page)).tasks.filter((id) => !idle.tasks.includes(id)), 'Cancel releases status refresh resources').toHaveLength(0);
	const cancelled = await probe(page);
	await page.clock.setSystemTime(new Date(epoch.getTime() + 600_000));
	await replayCallbacks(page);
	expect((await probe(page)).pauses).toBe(cancelled.pauses);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await duration(page, '1');
	await rememberCallbacks(page);
	// SPA navigation destroys the actual component but keeps the JS realm so
	// replay can exercise callbacks that were queued just before teardown.
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.href = '/'; link.textContent = 'Leave player fixture';
		link.dataset['testid'] = 'sleep-leave-fixture'; document.body.append(link);
	});
	await page.getByTestId('sleep-leave-fixture').click();
	// SvelteKit's SPA commit may await an animation frame. Advance the paused
	// harness clock while waiting, rather than mistaking frozen navigation for
	// a product teardown failure.
	await expect.poll(async () => {
		await page.clock.runFor(100);
		return page.getByTestId('comforts-panel').count();
	}).toBe(0);
	await page.clock.runFor(100);
	const left = await probe(page);
	expect(left.tasks, 'Page-owned refresh resources are cleaned on unmount').toHaveLength(0);
	await page.clock.setSystemTime(new Date(epoch.getTime() + 900_000));
	await replayCallbacks(page);
	await page.clock.runFor(5_000);
	expect((await probe(page)).pauses).toBe(left.pauses);
	expect((await probe(page)).plays).toBe(left.plays);
	expect((await probe(page)).tasks, 'A stale callback cannot resurrect the refresh loop').toHaveLength(0);
	expect(errors).toEqual([]);
});

test('SLP-B11 sleep changes cause no persistence and reload starts disarmed', async ({ page }) => {
	await boot(page);
	const saved = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }));
	await duration(page, '1');
	await page.clock.runFor(5_000);
	await page.getByTestId('sleep-timer-set-end-of-track').click();
	await page.getByTestId('sleep-timer-tracks').fill('3');
	await page.getByTestId('sleep-timer-set-stop-after').click();
	await page.getByTestId('sleep-timer-cancel').click();
	const after = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }));
	expect(after).toEqual(saved);
	await duration(page, '1');
	await page.reload();
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
	await expect(announcement(page)).toContainText(/off/i);
});

for (const theme of ['light', 'dark'] as const) {
	test(`SLP-B12-${theme} 360px keyboard/focus, associated error and non-overflowing state`, async ({ page }, testInfo) => {
		await page.setViewportSize({ width: 360, height: 800 });
		await page.emulateMedia({ colorScheme: theme });
		await boot(page);
		const input = page.getByTestId('sleep-timer-minutes');
		await input.fill('0');
		await input.focus();
		await page.keyboard.press('Tab');
		const set = page.getByTestId('sleep-timer-set-duration');
		await expect(set).toBeFocused();
		expect(await set.evaluate((el) => el.matches(':focus-visible'))).toBe(true);
		await page.keyboard.press('Enter');
		await expect(set).toBeFocused();
		await feedback(input, 'minutes');
		await testInfo.attach(`sleep-input-${theme}`, { body: await page.screenshot(), contentType: 'image/png' });
		await input.fill('0.5');
		await input.focus();
		await page.keyboard.press('Tab');
		await page.keyboard.press('Enter');
		await visibleRemaining(page, /\b0:30\b/);
		await page.clock.runFor(1_000);
		await expect(set).toBeFocused();
		await expect(announcement(page)).toContainText(/sleep/i);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Narrow screen must not require horizontal scrolling').toBe(true);
		await noLiveCountdown(page);
		await testInfo.attach(`sleep-remaining-${theme}`, { body: await page.screenshot(), contentType: 'image/png' });
		await page.getByTestId('sleep-timer-set-end-of-track').tap();
		await expect(announcement(page)).toContainText(/end of (the )?track/i);
		await page.getByTestId('sleep-timer-cancel').tap();
		await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
	});
}
