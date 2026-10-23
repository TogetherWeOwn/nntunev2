import { expect, test, type ElementHandle, type Page } from '@playwright/test';

/**
 * TRACK-TIME protected, test-first browser contract. No production hook required.
 * - track-time-mode: native select, accessible name "Time display", values
 *   elapsed/remaining with human-readable options; keyboard focus is visible.
 * - track-time-summary: visible application-owned text naming Elapsed/Remaining,
 *   selected coordinate and total duration. Unknown totals say "Duration unavailable";
 *   unknown remaining says "Remaining unavailable". Empty queue: "No track selected".
 * - No live-region ancestor/descendant for the per-second summary.
 * - Choice survives source/retry changes on this page, not teardown/reload. No
 *   storage write or transport side effect is caused by switching the choice.
 *
 * Unit contract and commands: src/lib/track-time.test.ts. From repository root:
 *   CI=1 pnpm --dir apps/web exec playwright test tests/track-time.spec.ts --workers=1
 * Existing regression commands (do not weaken these suites):
 *   pnpm --dir apps/web test
 *   CI=1 pnpm --dir apps/web exec playwright test tests/player-lifecycle.spec.ts tests/player-comforts.spec.ts tests/radio-continue.spec.ts tests/player-queue.spec.ts tests/media-session-artwork-errors.spec.ts --workers=1
 *   CI=1 pnpm --dir apps/web exec playwright test tests/player-shortcuts.spec.ts tests/player-shortcuts-builder.spec.ts --workers=1
 *   pnpm --dir apps/web check
 * CI=1 refuses another checkout's preview on port 4173; use an isolated local
 * port/config if occupied. Never attach to a different worktree's preview.
 * Real-decoder regression (local generated audio, no provider):
 *   CI=1 pnpm --dir apps/web exec playwright test tests/player-browser-contract.spec.ts tests/player-range-seek.spec.ts --workers=1
 *
 * Synthetic media stays pending, following player-lifecycle.spec.ts; only the
 * fixture sets time/duration/paused and emits events. No decoder, wall-clock
 * advance, backend duration, stream provider, production or secret is needed.
 * This catches omitted listeners, stale element reads, non-finite formatting,
 * rate-divided coordinates, transport-on-toggle and per-second announcements.
 */
type Frame = { time: number; duration: number; paused: boolean; currentSrc?: string };
type Harness = {
	frame: (audio: HTMLMediaElement) => Frame;
	playCalls: number;
	pauseCalls: number;
	seekCalls: number;
	resolve: () => void;
};
type HarnessWindow = Window & { __trackTime: Harness };
type Sample = { time: number; duration: number; event?: string; currentSrc?: string };

const summary = (page: Page) => page.getByTestId('track-time-summary');
const mode = (page: Page) => page.getByTestId('track-time-mode');

async function openPlayer(page: Page) {
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (url.hostname !== 'localhost' && url.hostname !== '127.0.0.1') return route.abort();
		if (url.pathname === '/api/demo/audio') return;
		return route.continue();
	});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		localStorage.removeItem('nntune:resume:v1');
		localStorage.removeItem('nntune:playback-speed:v1');
		const frames = new WeakMap<HTMLMediaElement, Frame>();
		const frame = (audio: HTMLMediaElement) => {
			let state = frames.get(audio);
			if (!state) {
				state = { time: 0, duration: NaN, paused: true };
				frames.set(audio, state);
			}
			return state;
		};
		const control: Harness = { frame, playCalls: 0, pauseCalls: 0, seekCalls: 0, resolve: () => {} };
		Object.assign(window, { __trackTime: control });
		Object.defineProperties(HTMLMediaElement.prototype, {
			currentTime: {
				configurable: true,
				get() { return frame(this).time; },
				set(value: number) { control.seekCalls += 1; frame(this).time = value; }
			},
			duration: { configurable: true, get() { return frame(this).duration; } },
			paused: { configurable: true, get() { return frame(this).paused; } },
			currentSrc: { configurable: true, get() { return frame(this).currentSrc ?? this.src; } }
		});
		HTMLMediaElement.prototype.play = function () {
			control.playCalls += 1;
			return new Promise<void>((resolve) => {
				control.resolve = () => { frame(this).paused = false; resolve(); };
			});
		};
		HTMLMediaElement.prototype.pause = function () {
			control.pauseCalls += 1;
			frame(this).paused = true;
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('demo-audio')).toHaveCount(1);
}

async function sample(page: Page, value: Sample) {
	await page.getByTestId('demo-audio').evaluate((el, next) => {
		const state = (window as unknown as HarnessWindow).__trackTime.frame(el as HTMLAudioElement);
		state.time = next.time;
		state.duration = next.duration;
		state.currentSrc = next.currentSrc;
		el.dispatchEvent(new Event(next.event ?? 'timeupdate'));
	}, value);
}

async function staleSample(old: ElementHandle, value: Sample) {
	await old.evaluate((el, next) => {
		const state = (window as unknown as HarnessWindow).__trackTime.frame(el as HTMLAudioElement);
		state.time = next.time;
		state.duration = next.duration;
		el.dispatchEvent(new Event(next.event ?? 'timeupdate'));
	}, value);
}

async function transport(page: Page) {
	return page.evaluate(() => {
		const { playCalls, pauseCalls, seekCalls } = (window as unknown as HarnessWindow).__trackTime;
		const audio = document.querySelector('audio')!;
		return { playCalls, pauseCalls, seekCalls, time: audio.currentTime, paused: audio.paused, rate: audio.playbackRate };
	});
}

async function leavePlayer(page: Page) {
	// Exercise actual SPA teardown while retaining the old element's JS realm.
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.href = '/';
		link.textContent = 'Leave player fixture';
		link.dataset['testid'] = 'track-time-leave-fixture';
		document.body.append(link);
	});
	await page.getByTestId('track-time-leave-fixture').click();
	await expect(page).toHaveURL(`${new URL(page.url()).origin}/`);
	await page.getByTestId('track-time-leave-fixture').evaluate((el) => el.remove());
}

async function startPlaying(page: Page) {
	await page.getByTestId('queue-select-demo-track').click();
	await page.evaluate(() => (window as unknown as HarnessWindow).__trackTime.resolve());
	await page.getByTestId('demo-audio').dispatchEvent('playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}

async function remaining(page: Page) {
	await expect(mode(page)).toBeVisible();
	await mode(page).focus();
	await mode(page).press('End');
	await mode(page).press('Enter');
	await expect(mode(page)).toHaveValue('remaining');
}

async function known(page: Page, coordinate: string, duration: string, label = 'Elapsed') {
	await expect(summary(page)).toBeVisible();
	await expect(summary(page)).toContainText(label);
	// Ordered coordinate then total, allowing punctuation/wording between them.
	const literal = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await expect(summary(page)).toContainText(new RegExp(`${literal(coordinate)}\\D+${literal(duration)}`));
	await expect(summary(page)).not.toContainText(/unavailable|NaN|Infinity/i);
}

async function unknown(page: Page, elapsed = '0:00') {
	await expect(summary(page)).toBeVisible();
	await expect(summary(page)).toContainText(`Elapsed ${elapsed}`);
	await expect(summary(page)).toContainText('Duration unavailable');
	await expect(summary(page)).not.toContainText(/NaN|Infinity/);
}

test('TT-B00 fixture control: real player loads and pending play settles without decoding', async ({ page }) => {
	await openPlayer(page);
	await startPlaying(page);
	await sample(page, { time: 61.9, duration: 240, event: 'loadedmetadata' });
	expect(await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => [el.currentTime, el.duration])).toEqual([61.9, 240]);
	await page.getByTestId('demo-audio').evaluate((el) => {
		(window as unknown as HarnessWindow).__trackTime.frame(el as HTMLAudioElement).paused = true;
		el.dispatchEvent(new Event('pause'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	const old = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('player-next').click();
	expect(await old!.evaluate((el) => el.isConnected)).toBe(false);
	await page.getByTestId('demo-audio').dispatchEvent('error');
	const failed = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('player-retry').click();
	expect(await failed!.evaluate((el) => el.isConnected)).toBe(false);
	const leaving = await page.getByTestId('demo-audio').elementHandle();
	await leavePlayer(page);
	expect(await leaving!.evaluate((el) => el.isConnected)).toBe(false);
	await staleSample(leaving!, { time: 777, duration: 999 });
	await page.goBack();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});

test('TT-B01 default elapsed choice is labeled, keyboard-operable, visibly focused; native controls remain', async ({ page }) => {
	await openPlayer(page);
	await expect(mode(page)).toHaveAccessibleName('Time display');
	await expect(mode(page)).toHaveValue('elapsed');
	await expect(mode(page).getByRole('option', { name: 'Elapsed', exact: true })).toHaveCount(1);
	await expect(mode(page).getByRole('option', { name: 'Remaining', exact: true })).toHaveCount(1);
	await mode(page).focus();
	await expect(mode(page)).toBeFocused();
	expect(await mode(page).evaluate((el) => {
		const style = getComputedStyle(el);
		return (style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== 'none';
	})).toBe(true);
	await remaining(page);
	await mode(page).press('Home');
	await mode(page).press('Enter');
	await expect(mode(page)).toHaveValue('elapsed');
	await unknown(page);
	await expect(page.getByTestId('demo-audio')).toHaveJSProperty('controls', true);
});

test('TT-B02 paused metadata, timeupdate and durationchange show media seconds including hours', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 61.9, duration: 240, event: 'loadedmetadata' });
	await known(page, '1:01', '4:00');
	await sample(page, { time: 62.9, duration: 240 });
	await known(page, '1:02', '4:00');
	await sample(page, { time: 3661.9, duration: 7322, event: 'durationchange' });
	await known(page, '1:01:01', '2:02:02');
	expect((await transport(page)).playCalls).toBe(0);
	expect((await transport(page)).paused).toBe(true);
});

for (const playing of [false, true]) {
	test(`TT-B03-${playing ? 'playing' : 'paused'} keyboard toggle has no play/pause/seek/storage effects at 2x`, async ({ page }) => {
		await openPlayer(page);
		await startPlaying(page);
		if (!playing) {
			await page.getByTestId('demo-audio').evaluate((el) => {
				(window as unknown as HarnessWindow).__trackTime.frame(el as HTMLAudioElement).paused = true;
				el.dispatchEvent(new Event('pause'));
			});
			await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		}
		await sample(page, { time: 61.9, duration: 240, event: 'loadedmetadata' });
		await page.getByTestId('playback-speed').selectOption('2');
		await known(page, '1:01', '4:00');
		const before = await transport(page);
		const storageBefore = await page.evaluate(() => [Object.entries(localStorage), Object.entries(sessionStorage)]);
		const audio = await page.getByTestId('demo-audio').elementHandle();
		await remaining(page);
		await known(page, '2:58', '4:00', 'Remaining');
		expect(await transport(page)).toEqual(before);
		await expect(page.getByTestId('player-status')).toHaveText(`Status: ${playing ? 'playing' : 'paused'}`);
		if (!playing) {
			// A late event must not undo the user's pause intent after a UI toggle.
			await page.getByTestId('demo-audio').dispatchEvent('playing');
			await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
			expect(await transport(page)).toEqual(before);
		}
		expect(await page.evaluate(() => [Object.entries(localStorage), Object.entries(sessionStorage)])).toEqual(storageBefore);
		expect(await audio!.evaluate((el) => el.isConnected)).toBe(true);
		await expect(page.getByTestId('demo-audio')).toHaveCount(1);
		await expect(page.getByTestId('demo-audio')).toHaveJSProperty('controls', true);
	});
}

for (const [id, duration] of [['nan', NaN], ['infinity', Infinity], ['negative-infinity', -Infinity], ['negative', -1]] as const) {
	test(`TT-B04-${id} unknown duration is not zero or a non-finite display`, async ({ page }) => {
		await openPlayer(page);
		await sample(page, { time: 61.9, duration, event: 'loadedmetadata' });
		await unknown(page, '1:01');
		await remaining(page);
		await expect(summary(page)).toContainText('Remaining unavailable');
		await expect(summary(page)).toContainText('Duration unavailable');
		await expect(summary(page)).not.toContainText(/NaN|Infinity|0:00/);
	});
}

test('TT-B05 duration goes unknown to known to unknown; finite zero stays distinguishable', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 32, duration: NaN, event: 'loadedmetadata' });
	await unknown(page, '0:32');
	await sample(page, { time: 32, duration: 180, event: 'durationchange' });
	await known(page, '0:32', '3:00');
	await remaining(page);
	await known(page, '2:28', '3:00', 'Remaining');
	await sample(page, { time: 32, duration: Infinity, event: 'durationchange' });
	await expect(summary(page)).toContainText('Remaining unavailable');
	await expect(summary(page)).toContainText('Duration unavailable');
	await sample(page, { time: 32, duration: 0, event: 'durationchange' });
	await known(page, '0:00', '0:00', 'Remaining');
});

for (const [id, time, elapsed, left] of [
	['nan', NaN, '0:00', '4:00'],
	['infinity', Infinity, '0:00', '4:00'],
	['negative-infinity', -Infinity, '0:00', '4:00'],
	['negative', -21, '0:00', '4:00'],
	['overrun', 999, '4:00', '0:00']
] as const) {
	test(`TT-B06-${id} invalid/overrun elapsed is bounded before formatting`, async ({ page }) => {
		await openPlayer(page);
		await sample(page, { time, duration: 240, event: 'loadedmetadata' });
		await known(page, elapsed, '4:00');
		await remaining(page);
		await known(page, left, '4:00', 'Remaining');
		await expect(summary(page)).not.toContainText(/-\d|NaN|Infinity/);
	});
}

test('TT-B07 a connected audio with mismatched currentSrc cannot supply time or duration', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 61, duration: 240, event: 'loadedmetadata' });
	await known(page, '1:01', '4:00');
	const before = await summary(page).textContent();
	for (const event of ['timeupdate', 'loadedmetadata', 'durationchange']) {
		await sample(page, { time: 777, duration: 999, currentSrc: `${new URL(page.url()).origin}/api/demo/audio?track=third-light`, event });
		await expect(summary(page)).toHaveText(before!);
	}
	await sample(page, { time: 62, duration: 240 });
	await known(page, '1:02', '4:00');
});

test('TT-B08 source replacement resets coordinates, retains choice and fences detached events', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 61, duration: 240, event: 'loadedmetadata' });
	await known(page, '1:01', '4:00');
	await remaining(page);
	const old = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect(await old!.evaluate((el) => el.isConnected)).toBe(false);
	await expect(mode(page)).toHaveValue('remaining');
	await expect(summary(page)).toContainText('Remaining unavailable');
	await expect(summary(page)).toContainText('Duration unavailable');
	await mode(page).press('Home');
	await mode(page).press('Enter');
	await unknown(page);
	await remaining(page);
	for (const event of ['timeupdate', 'loadedmetadata', 'durationchange']) {
		await staleSample(old!, { time: 777, duration: 999, event });
		await expect(summary(page)).toContainText('Duration unavailable');
	}
	await sample(page, { time: 10, duration: 90, event: 'loadedmetadata' });
	await known(page, '1:20', '1:30', 'Remaining');
	await staleSample(old!, { time: 888, duration: 999 });
	await known(page, '1:20', '1:30', 'Remaining');
});

test('TT-B09 retry revision resets even for the same track URL; old metadata stays fenced', async ({ page }) => {
	await openPlayer(page);
	await startPlaying(page);
	await sample(page, { time: 61, duration: 240, event: 'loadedmetadata' });
	await known(page, '1:01', '4:00');
	await remaining(page);
	const failed = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('demo-audio').dispatchEvent('error');
	await expect(page.getByTestId('player-status')).toHaveText('Status: error');
	await page.getByTestId('player-retry').click();
	expect(await failed!.evaluate((el) => el.isConnected)).toBe(false);
	await expect(mode(page)).toHaveValue('remaining');
	await expect(summary(page)).toContainText('Remaining unavailable');
	await expect(summary(page)).toContainText('Duration unavailable');
	await mode(page).press('Home');
	await mode(page).press('Enter');
	await unknown(page);
	for (const event of ['timeupdate', 'loadedmetadata', 'durationchange']) {
		await staleSample(failed!, { time: 200, duration: 240, event });
		await unknown(page);
	}
	await sample(page, { time: 5, duration: 120, event: 'loadedmetadata' });
	await known(page, '0:05', '2:00');
});

test('TT-B10 queue clear removes old time; detached events cannot restore it; re-add starts fresh', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 61, duration: 240, event: 'loadedmetadata' });
	await known(page, '1:01', '4:00');
	const old = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('demo-audio')).toHaveCount(1);
	expect(await old!.evaluate((el) => el.isConnected)).toBe(false);
	await expect(summary(page)).toHaveText('No track selected');
	for (const event of ['timeupdate', 'loadedmetadata', 'durationchange']) {
		await staleSample(old!, { time: 777, duration: 999, event });
		await expect(summary(page)).toHaveText('No track selected');
	}
	await page.getByTestId('queue-add-demo-track').click();
	await unknown(page);
});

test('TT-B11 per-second updates are visible but outside all live regions', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 61, duration: 240, event: 'loadedmetadata' });
	await known(page, '1:01', '4:00');
	const live = '[aria-live]:not([aria-live="off"]),[role="status"]:not([aria-live="off"]),[role="alert"]:not([aria-live="off"]),[role="log"]:not([aria-live="off"])';
	expect(await summary(page).evaluate((el, selector) => !!el.closest(selector) || !!el.querySelector(selector), live)).toBe(false);
	const announcements = await page.locator(live).allTextContents();
	for (const time of [62, 63, 64]) await sample(page, { time, duration: 240 });
	await known(page, '1:04', '4:00');
	expect(await page.locator(live).allTextContents()).toEqual(announcements);
	expect(await summary(page).evaluate((el, selector) => !!el.closest(selector) || !!el.querySelector(selector), live)).toBe(false);
});

test('TT-B13 remaining floors after fractional subtraction, not before', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 0.1, duration: 60.9, event: 'loadedmetadata' });
	await known(page, '0:00', '1:00');
	await remaining(page);
	await known(page, '1:00', '1:00', 'Remaining');
});

test('TT-B12 SPA teardown fences old events and remount resets mode/time without persistence', async ({ page }) => {
	await openPlayer(page);
	await sample(page, { time: 61, duration: 240, event: 'loadedmetadata' });
	await known(page, '1:01', '4:00');
	await remaining(page);
	const old = await page.getByTestId('demo-audio').elementHandle();
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await leavePlayer(page);
	expect(await old!.evaluate((el) => el.isConnected)).toBe(false);
	await expect(summary(page)).toHaveCount(0);
	await staleSample(old!, { time: 999, duration: 999, event: 'durationchange' });
	await page.goBack();
	await expect(mode(page)).toHaveValue('elapsed');
	await unknown(page);
	await staleSample(old!, { time: 888, duration: 999 });
	await unknown(page);
	expect(errors).toEqual([]);
	await remaining(page);
	await page.reload();
	await expect(mode(page)).toHaveValue('elapsed');
	await unknown(page);
});
