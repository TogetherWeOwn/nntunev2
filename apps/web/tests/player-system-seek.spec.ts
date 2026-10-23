import { expect, test, type Page } from '@playwright/test';

/**
 * SYSTEM-SEEK browser acceptance; protected Test Author handoff.
 * Execution/seam/Builder ownership: see the header in
 * src/lib/player/mediaSession.system-seek.acceptance.test.ts (canonical note).
 *
 * Real demo HTMLAudioElement/currentTime and local deterministic PCM fixture;
 * only the OS Media Session boundary is recorded. play() remains pending so
 * intent/seek assertions cannot advance with wall clock. No currentTime mock,
 * no hardware, provider, live stream, or physical lock-screen acceptance.
 * Invalid duration branches use an explicit duration-only override; normal
 * seek/rate/metadata cases use decoded native duration and native currentTime.
 *
 * Expected missing-behavior RED: absent seek callbacks or missing position
 * publication/clear, NOT missing imports, page hooks, selectors or compilation.
 * Mutations caught: seek the wrong element; trust nonnumeric/nonfinite details;
 * use a 0/default offset for invalid input; play/advance on seek; publish stale
 * state; leak events/callbacks across track swap/unmount; poll instead of events.
 */

type Position = { duration: number; position: number; playbackRate: number };
type Details = { action: string; seekTime?: unknown; seekOffset?: unknown; fastSeek?: boolean };
type Callback = (details: Details) => void;
type Probe = {
	handlers: Record<string, Callback | null>;
	attempts: string[];
	positions: (Position | null)[];
	playCalls: number;
	retired: Callback[];
	oldAudio: HTMLAudioElement | null;
};
declare global {
	interface Window { __systemMediaAcceptance: Probe; }
}

type Mode = { rejectAction?: string; positionApi?: 'missing' | 'throwing'; noSession?: boolean };
const SEEK_ACTIONS = ['seekto', 'seekforward', 'seekbackward'];
const ACTIONS = ['nexttrack', 'previoustrack', 'play', 'pause', ...SEEK_ACTIONS];

async function openPlayer(page: Page, mode: Mode = {}, loadMedia = true) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	// Holding the response is used only for unknown-duration/event-loop cases.
	if (!loadMedia) await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript((config: Mode) => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		localStorage.removeItem('nntune:resume:v1');
		localStorage.removeItem('nntune:playback-speed:v1');
		const probe: Probe = {
			handlers: {}, attempts: [], positions: [], playCalls: 0, retired: [], oldAudio: null
		};
		window.__systemMediaAcceptance = probe;
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			return new Promise<void>(() => {});
		};
		if (config.noSession) {
			// Truly absent, not an own property returning undefined.
			let owner: object | null = navigator;
			while (owner && !Object.prototype.hasOwnProperty.call(owner, 'mediaSession')) {
				owner = Object.getPrototypeOf(owner);
			}
			if (owner) Reflect.deleteProperty(owner, 'mediaSession');
			return;
		}
		const session = {
			metadata: null,
			setActionHandler(action: string, callback: Callback | null) {
				probe.attempts.push(action);
				if (config.rejectAction === action) throw new DOMException('unsupported', 'NotSupportedError');
				probe.handlers[action] = callback;
			},
			...(config.positionApi === 'missing' ? {} : {
				setPositionState(state?: Position) {
					probe.positions.push(state && Object.keys(state).length ? { ...state } : null);
					if (config.positionApi === 'throwing') throw new DOMException('unsupported', 'NotSupportedError');
				}
			})
		};
		Object.defineProperty(navigator, 'mediaSession', { configurable: true, value: session });
	}, mode);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	if (!mode.noSession) {
		await expect.poll(() => page.evaluate(() => typeof window.__systemMediaAcceptance.handlers['pause'])).toBe('function');
	}
	if (loadMedia) {
		await expect.poll(() => page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.duration)).toBeGreaterThan(13);
	}
	return errors;
}

async function requireSeek(page: Page, actions = SEEK_ACTIONS) {
	for (const action of actions) {
		expect(await page.evaluate((name) => typeof window.__systemMediaAcceptance.handlers[name], action), `${action} handler`).toBe('function');
	}
}

async function dispatch(page: Page, details: Details) {
	await page.evaluate((input) => window.__systemMediaAcceptance.handlers[input.action]?.(input), details);
}

async function time(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function setTime(page: Page, seconds: number, event = 'timeupdate') {
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement, input) => {
		el.currentTime = input.seconds;
		el.dispatchEvent(new Event(input.event));
	}, { seconds, event });
	await expect.poll(() => time(page)).toBeCloseTo(seconds, 3);
}

async function lastPosition(page: Page) {
	return page.evaluate(() => window.__systemMediaAcceptance.positions.at(-1));
}

async function expectPosition(page: Page, position: number, playbackRate = 1) {
	const duration = await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.duration);
	await expect.poll(() => lastPosition(page)).toEqual({ duration, position, playbackRate });
}

async function expectPausedUnchanged(page: Page, playCalls = 0) {
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	expect(await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.paused)).toBe(true);
	expect(await page.evaluate(() => window.__systemMediaAcceptance.playCalls)).toBe(playCalls);
}

test('MS-B01 seekto uses native currentTime, clamps upper bound, and preserves paused intent at both endpoints', async ({ page }) => {
	await openPlayer(page);
	await dispatch(page, { action: 'pause' });
	await setTime(page, 3);
	await requireSeek(page);
	for (const target of [6.25, 0, 1000]) {
		await dispatch(page, { action: 'seekto', seekTime: target, fastSeek: true });
		const expected = target === 1000
			? await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.duration)
			: target;
		await expect.poll(() => time(page)).toBeCloseTo(expected, 3);
		await expectPosition(page, expected);
		await expectPausedUnchanged(page);
	}
});

for (const sample of [
	{ action: 'seekforward', start: 3, expected: 13 },
	{ action: 'seekbackward', start: 13, expected: 3 },
	{ action: 'seekforward', start: 3, seekOffset: 2.5, expected: 5.5 },
	{ action: 'seekbackward', start: 3, seekOffset: 2.5, expected: 0.5 },
	{ action: 'seekbackward', start: 3, seekOffset: 1000, expected: 0 },
	{ action: 'seekforward', start: 3, seekOffset: 1000, expected: 'duration' }
]) {
	test(`MS-B02 ${sample.action} offset=${sample.seekOffset ?? 'default'} start=${sample.start}`, async ({ page }) => {
		await openPlayer(page);
		await dispatch(page, { action: 'pause' });
		await setTime(page, sample.start);
		await requireSeek(page, [sample.action]);
		await dispatch(page, { action: sample.action, ...(sample.seekOffset === undefined ? {} : { seekOffset: sample.seekOffset }) });
		const expected = sample.expected === 'duration'
			? await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.duration)
			: sample.expected as number;
		await expect.poll(() => time(page)).toBeCloseTo(expected, 3);
		await expectPosition(page, expected);
		await expectPausedUnchanged(page);
	});
}

for (const action of SEEK_ACTIONS) {
	test(`MS-B03 ${action} rejects nonnumeric, negative and nonfinite details without seeking or playing`, async ({ page }) => {
		await openPlayer(page);
		await dispatch(page, { action: 'pause' });
		await setTime(page, 3);
		await requireSeek(page, [action]);
		const field = action === 'seekto' ? 'seekTime' : 'seekOffset';
		const invalid: unknown[] = [NaN, Infinity, -Infinity, -1, '8', '', null, true, {}];
		if (action === 'seekto') invalid.push(undefined);
		else invalid.push(0);
		for (const value of invalid) {
			await dispatch(page, { action, [field]: value });
			expect(await time(page), `${action} ${String(value)}`).toBeCloseTo(3, 3);
			await expectPausedUnchanged(page);
		}
	});
}

for (const duration of [NaN, Infinity, -Infinity, 0, -1]) {
	test(`MS-B04 no seek with invalid/unknown duration ${String(duration)}`, async ({ page }) => {
		await openPlayer(page, {}, false);
		await dispatch(page, { action: 'pause' });
		await setTime(page, 3);
		await page.getByTestId('demo-audio').evaluate((el, value) => {
			Object.defineProperty(el, 'duration', { configurable: true, value });
			el.dispatchEvent(new Event('durationchange'));
		}, duration);
		await requireSeek(page);
		for (const details of [
			{ action: 'seekto', seekTime: 6 }, { action: 'seekforward' }, { action: 'seekbackward' }
		]) {
			await dispatch(page, details);
			expect(await time(page)).toBe(3);
		}
		await expectPausedUnchanged(page);
	});
}

test('MS-B05 loadedmetadata/timeupdate/seeked/ratechange publish finite state from current audio', async ({ page }) => {
	await openPlayer(page);
	const audio = page.getByTestId('demo-audio');
	await audio.evaluate((el) => el.dispatchEvent(new Event('loadedmetadata')));
	await expectPosition(page, 0);
	await setTime(page, 3);
	await expectPosition(page, 3);
	await setTime(page, 6, 'seeked');
	await expectPosition(page, 6);
	await page.getByTestId('playback-speed').selectOption('1.5');
	await expectPosition(page, 6, 1.5);
	await audio.evaluate((el: HTMLAudioElement) => {
		el.playbackRate = 0.75;
		el.dispatchEvent(new Event('ratechange'));
	});
	await expectPosition(page, 6, 0.75);
	const states = await page.evaluate(() => window.__systemMediaAcceptance.positions);
	for (const state of states.filter((value): value is Position => value !== null)) {
		expect(Number.isFinite(state.duration) && state.duration > 0).toBe(true);
		expect(Number.isFinite(state.position) && state.position >= 0 && state.position <= state.duration).toBe(true);
		expect(Number.isFinite(state.playbackRate) && state.playbackRate > 0).toBe(true);
	}
});

test('MS-B06 losing known duration clears stale position, then metadata repopulates it', async ({ page }) => {
	await openPlayer(page);
	await setTime(page, 3);
	// Prepopulate independently so a missing publisher cannot vacuously pass.
	await page.evaluate(() => window.__systemMediaAcceptance.positions.push({ duration: 120, position: 42, playbackRate: 1 }));
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'duration', { configurable: true, value: NaN });
		el.dispatchEvent(new Event('durationchange'));
	});
	await expect.poll(() => lastPosition(page)).toBeNull();
	await page.getByTestId('demo-audio').evaluate((el) => {
		Reflect.deleteProperty(el, 'duration');
		el.dispatchEvent(new Event('loadedmetadata'));
	});
	await expectPosition(page, 3);
});

test('MS-B07 track swap clears stale state before metadata, ignores old callbacks/events, and publishes the new rate/time', async ({ page }) => {
	await openPlayer(page);
	await requireSeek(page);
	await setTime(page, 3);
	await page.getByTestId('playback-speed').selectOption('1.5');
	await page.evaluate(() => {
		const p = window.__systemMediaAcceptance;
		p.oldAudio = document.querySelector('[data-testid="demo-audio"]');
		p.retired = ['seekto', 'seekforward', 'seekbackward'].map((name) => p.handlers[name]!);
		p.positions.push({ duration: 120, position: 42, playbackRate: 1 });
	});
	await page.route('**/api/demo/audio**', () => {});
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => lastPosition(page)).toBeNull();
	// Unknown native duration cannot inherit the previous track's timeline.
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { configurable: true, value: 90 });
		el.currentTime = 5;
		el.dispatchEvent(new Event('loadedmetadata'));
	});
	await expectPosition(page, 5, 1.5);
	const before = await page.evaluate(() => window.__systemMediaAcceptance.playCalls);
	await page.evaluate(() => {
		const p = window.__systemMediaAcceptance;
		p.retired.forEach((callback, index) => callback({
			action: ['seekto', 'seekforward', 'seekbackward'][index]!, seekTime: 70, seekOffset: 20
		}));
		for (const type of ['loadedmetadata', 'timeupdate', 'seeked', 'ratechange', 'ended']) {
			p.oldAudio?.dispatchEvent(new Event(type));
		}
	});
	expect(await time(page)).toBe(5);
	await expectPosition(page, 5, 1.5);
	expect(await page.evaluate(() => window.__systemMediaAcceptance.playCalls)).toBe(before);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	// Fresh actions target ONLY the incoming element and retain speed.
	await dispatch(page, { action: 'seekto', seekTime: 12 });
	await expect.poll(() => time(page)).toBe(12);
	await expectPosition(page, 12, 1.5);
});

test('MS-B08 queue clear removes all seek handlers/state; retained callbacks cannot resurrect playback', async ({ page }) => {
	await openPlayer(page);
	await requireSeek(page);
	await setTime(page, 3);
	await page.evaluate(() => {
		const p = window.__systemMediaAcceptance;
		p.retired = ['seekto', 'seekforward', 'seekbackward'].map((name) => p.handlers[name]!);
		p.positions.push({ duration: 120, position: 42, playbackRate: 1 });
	});
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect.poll(() => lastPosition(page)).toBeNull();
	for (const action of ACTIONS) {
		expect(await page.evaluate((name) => window.__systemMediaAcceptance.handlers[name], action)).toBeNull();
	}
	await page.evaluate(() => window.__systemMediaAcceptance.retired.forEach((callback) => callback({ action: 'seekto', seekTime: 8, seekOffset: 2 })));
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	expect(await page.evaluate(() => window.__systemMediaAcceptance.playCalls)).toBe(0);
	await expect.poll(() => lastPosition(page)).toBeNull();
});

test('MS-B09 client unmount clears global handlers/state and fences stored callbacks/events', async ({ page }) => {
	await openPlayer(page);
	await requireSeek(page);
	await setTime(page, 3);
	await page.evaluate(() => {
		const p = window.__systemMediaAcceptance;
		p.oldAudio = document.querySelector('[data-testid="demo-audio"]');
		p.retired = ['seekto', 'seekforward', 'seekbackward'].map((name) => p.handlers[name]!);
		p.positions.push({ duration: 120, position: 42, playbackRate: 1 });
		const link = document.createElement('a');
		link.href = '/'; link.textContent = 'Leave player'; link.dataset.testid = 'system-media-leave';
		document.querySelector('main')!.append(link);
	});
	await page.getByTestId('system-media-leave').click();
	await expect(page.getByTestId('app-root')).toBeVisible();
	// Verify a client transition kept the same probe, not a fresh document.
	expect(await page.evaluate(() => window.__systemMediaAcceptance.retired.length)).toBe(3);
	await expect.poll(() => lastPosition(page)).toBeNull();
	for (const action of ACTIONS) {
		expect(await page.evaluate((name) => window.__systemMediaAcceptance.handlers[name], action)).toBeNull();
	}
	const before = await page.evaluate(() => ({
		plays: window.__systemMediaAcceptance.playCalls,
		writes: window.__systemMediaAcceptance.positions.length,
		time: window.__systemMediaAcceptance.oldAudio?.currentTime
	}));
	await page.evaluate(() => {
		const p = window.__systemMediaAcceptance;
		p.retired.forEach((callback) => callback({ action: 'seekto', seekTime: 10, seekOffset: 5 }));
		for (const type of ['loadedmetadata', 'timeupdate', 'seeked', 'ratechange']) p.oldAudio?.dispatchEvent(new Event(type));
	});
	expect(await page.evaluate(() => ({
		plays: window.__systemMediaAcceptance.playCalls,
		writes: window.__systemMediaAcceptance.positions.length,
		time: window.__systemMediaAcceptance.oldAudio?.currentTime
	}))).toEqual(before);
	// Re-enter a new host in the SAME document. An old callback that consults
	// a mutable/global audio getter must not start driving this new host.
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.href = '/demo/player'; link.textContent = 'Return to player';
		link.dataset.testid = 'system-media-return';
		document.querySelector('main')!.append(link);
	});
	await page.getByTestId('system-media-return').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect.poll(() => page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.duration)).toBeGreaterThan(13);
	await dispatch(page, { action: 'pause' });
	await setTime(page, 5);
	await expectPosition(page, 5);
	const plays = await page.evaluate(() => window.__systemMediaAcceptance.playCalls);
	await page.evaluate(() => window.__systemMediaAcceptance.retired.forEach((callback, index) => callback({
		action: ['seekto', 'seekforward', 'seekbackward'][index]!, seekTime: 10, seekOffset: 5
	})));
	expect(await time(page)).toBe(5);
	await expectPosition(page, 5);
	await expectPausedUnchanged(page, plays);
});

for (const mode of [{ positionApi: 'missing' }, { positionApi: 'throwing' }, { rejectAction: 'seekto' }] as const) {
	test(`MS-B10 individual unsupported APIs remain isolated ${JSON.stringify(mode)}`, async ({ page }) => {
		const errors = await openPlayer(page, mode);
		await setTime(page, 3);
		await requireSeek(page, ['seekforward', 'seekbackward']);
		await dispatch(page, { action: 'pause' });
		await dispatch(page, { action: 'seekforward', seekOffset: 2 });
		await expect.poll(() => time(page)).toBeCloseTo(5, 3);
		await dispatch(page, { action: 'seekbackward', seekOffset: 1 });
		await expect.poll(() => time(page)).toBeCloseTo(4, 3);
		await expectPausedUnchanged(page);
		await dispatch(page, { action: 'nexttrack' });
		await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
		await dispatch(page, { action: 'previoustrack' });
		await expect(page.getByTestId('now-playing')).toContainText('Demo track');
		await dispatch(page, { action: 'play' });
		await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
		await dispatch(page, { action: 'pause' });
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		expect(errors).toEqual([]);
	});
}

test('MS-B11 whole Media Session absent does not break page, speed, or keyboard seeking', async ({ page }) => {
	const errors = await openPlayer(page, { noSession: true });
	await setTime(page, 3);
	await page.evaluate(() => { if (document.activeElement instanceof HTMLElement) document.activeElement.blur(); });
	await page.keyboard.press('l');
	await expect.poll(() => time(page)).toBeCloseTo(13, 3);
	await page.keyboard.press('j');
	await expect.poll(() => time(page)).toBeCloseTo(3, 3);
	await page.getByTestId('playback-speed').selectOption('1.5');
	expect(await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate)).toBe(1.5);
	// Passing base control for the client-unmount/re-entry fixture, independent
	// of new seek registrations: verify the probe survives both transitions.
	await page.evaluate(() => {
		window.__systemMediaAcceptance.oldAudio = document.querySelector('[data-testid="demo-audio"]');
		const link = document.createElement('a');
		link.href = '/'; link.textContent = 'Leave player'; link.dataset.testid = 'system-media-leave';
		document.querySelector('main')!.append(link);
	});
	await page.getByTestId('system-media-leave').click();
	await expect(page.getByTestId('app-root')).toBeVisible();
	expect(await page.evaluate(() => window.__systemMediaAcceptance.oldAudio?.isConnected)).toBe(false);
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.href = '/demo/player'; link.textContent = 'Return to player';
		link.dataset.testid = 'system-media-return';
		document.querySelector('main')!.append(link);
	});
	await page.getByTestId('system-media-return').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect.poll(() => page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.duration)).toBeGreaterThan(13);
	await setTime(page, 5);
	expect(await page.evaluate(() => window.__systemMediaAcceptance.oldAudio?.isConnected)).toBe(false);
	expect(errors).toEqual([]);
});

test('MS-B12 position updates are event-driven, not timer/per-frame reads or publishes', async ({ page }) => {
	await page.clock.install();
	await openPlayer(page, {}, false);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		let reads = 0;
		Object.defineProperty(el, 'duration', { configurable: true, get() { reads += 1; return 120; } });
		Object.defineProperty(el, '__durationReads', { get: () => reads });
		el.currentTime = 3;
		el.dispatchEvent(new Event('loadedmetadata'));
	});
	await expectPosition(page, 3);
	const counts = () => page.getByTestId('demo-audio').evaluate((el) => ({
		reads: (el as HTMLAudioElement & { __durationReads: number }).__durationReads,
		writes: window.__systemMediaAcceptance.positions.length
	}));
	const before = await counts();
	await page.clock.runFor(2000);
	expect(await counts()).toEqual(before);
});
