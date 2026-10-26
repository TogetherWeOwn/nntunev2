import { expect, test as base, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-sleep-deadline/mount.js';

/**
 * LIBSLEEP protected acceptance bank. Tests only; build on the exact handoff SHA.
 * Run: pnpm exec playwright test tests/library-sleep-deadline.spec.ts tests/player-comforts.spec.ts
 *
 * Additive DOM seams IN the real library Player region:
 * library-sleep-panel, library-sleep-presets (labelled group of duration buttons),
 * library-sleep-minutes (labelled number input, finite inclusive min/max, step=any),
 * library-sleep-start (Start/Replace), library-sleep-cancel (disabled when idle),
 * library-sleep-remaining (quiet m:ss / h:mm:ss), library-sleep-status (one polite
 * aggregate transition announcement). Error wording, presets and ceiling are free;
 * the advertised bounds must work and invalid replacement must preserve deadline.
 * No end-of-track/after-N modes. A valid one-minute custom duration is required.
 *
 * Premise: metadata-only library rows deliberately have NO playable source.
 * The standalone Vite fixture mounts the REAL route; a pre-transform exports only
 * its existing replaceQueue input. Real reducers, auth notification, keyed audio,
 * page handlers, helpers, timer and cleanup run unchanged. Do not add production
 * test hooks, activate providers or invent a source-fetch path for this fixture.
 * Only decoder I/O is synthetic. Date/timers are Playwright's real injected clock;
 * setSystemTime jumps without missed tick delivery. No timer helper is mocked.
 * Every case begins with an immediate UI assertion: absent feature is assertion
 * RED, not a missing-selector click timeout. JSON ERROR is never feature RED.
 *
 * Breaks: omitted UI; tick/media-time countdown; invalid/early replacement;
 * captured old audio; duplicate expiry; expired intent replay; cancel/auth/source/
 * unmount leakage; stale feedback; persistence; live chatter; inaccessible controls.
 */

type Operation = { element: number; op: string };
type Task = { fire: () => void; kind: 'timeout' | 'interval' | 'frame' };
type Probe = {
	ops: Operation[]; nextId: number; ids: WeakMap<HTMLMediaElement, number>;
	tasks: Map<number, Task>; stale: (() => void)[]; detached: HTMLAudioElement[];
	announcements: string[]; requests: string[];
};
declare global { interface Window { librarySleepProbe: Probe } }

const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const routePath = `${webRoot}src/routes/library/+page.svelte`;
const TRACKS: Track[] = [
	{ id: 'sleep-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '/__sleep_audio__/coral.wav' },
	{ id: 'sleep-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '/__sleep_audio__/pine.wav' }
];
const state = (index = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex: index });
const epoch = new Date('2026-01-01T00:00:00Z');
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const hook = (page: Page, name: string) => player(page).getByTestId(`library-sleep-${name}`);
const audio = (page: Page) => player(page).locator('audio[controls]');

const test = base.extend<{ library: Page; audit: { requests: string[]; errors: string[] } }, { libraryOrigin: string }>({
	audit: async ({}, use) => { await use({ requests: [], errors: [] }); },
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: `${webRoot}tests/fixtures/library-sleep-deadline`, publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': `${webRoot}tests/fixtures/library-now-playing/navigation.ts`
			} },
			plugins: [{
				name: 'acceptance-library-sleep-input', enforce: 'pre',
				transform(code, id) {
					if (id !== routePath) return;
					if (!code.includes('</script>')) throw new Error('Library script boundary missing');
					return code.replace('</script>', '\nexport function acceptanceReplaceQueue(next: QueueState) { replaceQueue(next); }\n</script>');
				}
			}, svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, hmr: false, fs: { allow: [webRoot] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('Fixture TCP address missing');
			await use(`http://127.0.0.1:${address.port}`);
		} finally { await server.close(); }
	}, { scope: 'worker' }],
	library: async ({ page, libraryOrigin, audit }, use) => {
		page.on('pageerror', (error) => audit.errors.push(error.message));
		await page.clock.install({ time: epoch });
		await page.clock.pauseAt(epoch);
		const expectedOrigin = new URL(libraryOrigin).origin;
		await page.route('**/*', async (route) => {
			const req = route.request();
			const url = new URL(req.url());
			if (url.origin !== expectedOrigin) return route.abort();
			if (url.pathname === '/api/library' && req.method() === 'GET') {
				return route.fulfill({ status: 200, json: TRACKS.map(({ id, title, artist }) => ({ id, title, artist })) });
			}
			if (req.resourceType() === 'media' || /^\/(api|v1|__sleep_audio__)(\/|$)/.test(url.pathname)) return route.abort();
			return route.continue();
		});
		await page.addInitScript(() => {
			const p: Probe = { ops: [], nextId: 0, ids: new WeakMap(), tasks: new Map(), stale: [], detached: [], announcements: [], requests: [] };
			window.librarySleepProbe = p;
			const id = (element: HTMLMediaElement) => {
				if (!p.ids.has(element)) p.ids.set(element, ++p.nextId);
				return p.ids.get(element)!;
			};
			const record = (element: HTMLMediaElement, op: string) => p.ops.push({ element: id(element), op });
			const paused = new WeakMap<HTMLMediaElement, boolean>();
			const positions = new WeakMap<HTMLMediaElement, number>();
			const rates = new WeakMap<HTMLMediaElement, number>();
			Object.defineProperties(HTMLMediaElement.prototype, {
				paused: { configurable: true, get() { return paused.get(this) ?? true; } },
				currentSrc: { configurable: true, get() { return this.src; } },
				currentTime: { configurable: true, get() { return positions.get(this) ?? 0; }, set(value) { positions.set(this, value); record(this, 'seek'); } },
				playbackRate: { configurable: true, get() { return rates.get(this) ?? 1; }, set(value) { rates.set(this, value); record(this, 'rate'); } }
			});
			HTMLMediaElement.prototype.play = function () {
				record(this, 'play'); paused.set(this, false);
				Object.defineProperty(this, 'ended', { configurable: true, value: false });
				this.dispatchEvent(new Event('play')); this.dispatchEvent(new Event('playing'));
				return Promise.resolve();
			};
			HTMLMediaElement.prototype.pause = function () {
				record(this, 'pause'); const changed = !this.paused; paused.set(this, true);
				if (changed) this.dispatchEvent(new Event('pause'));
			};
			HTMLMediaElement.prototype.load = function () { record(this, 'load'); };
			const timeout = window.setTimeout.bind(window), interval = window.setInterval.bind(window);
			const clearTimeout = window.clearTimeout.bind(window), clearInterval = window.clearInterval.bind(window);
			const frame = window.requestAnimationFrame.bind(window), cancelFrame = window.cancelAnimationFrame.bind(window);
			window.setTimeout = ((callback: TimerHandler, delay?: number, ...args: unknown[]) => {
				const fire = () => { if (typeof callback === 'function') callback(...args); };
				const task = timeout(() => { p.tasks.delete(task); fire(); }, delay);
				p.tasks.set(task, { fire, kind: 'timeout' }); return task;
			}) as typeof window.setTimeout;
			window.setInterval = ((callback: TimerHandler, delay?: number, ...args: unknown[]) => {
				const fire = () => { if (typeof callback === 'function') callback(...args); };
				const task = interval(fire, delay); p.tasks.set(task, { fire, kind: 'interval' }); return task;
			}) as typeof window.setInterval;
			window.requestAnimationFrame = (callback) => {
				const fire = () => callback(performance.now());
				const task = frame(() => { p.tasks.delete(task); fire(); });
				p.tasks.set(task, { fire, kind: 'frame' }); return task;
			};
			window.clearTimeout = (task) => { p.tasks.delete(task!); clearTimeout(task); };
			window.clearInterval = (task) => { p.tasks.delete(task!); clearInterval(task); };
			window.cancelAnimationFrame = (task) => { p.tasks.delete(task); cancelFrame(task); };
		});
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		await page.evaluate((next) => window.librarySleep.replace(next), state());
		await expect(audio(page)).toHaveCount(1);
		await audio(page).evaluate((element: HTMLAudioElement) => element.play());
		await page.clock.runFor(100);
		await page.evaluate(() => { window.librarySleepProbe.ops = []; });
		page.on('request', (request) => {
			if (/^\/(api|v1)(\/|$)/.test(new URL(request.url()).pathname)) {
				// Keep the transport audit in Node; page navigation must not erase it.
				audit.requests.push(`${request.method()} ${new URL(request.url()).pathname}`);
			}
		});
		await use(page);
		expect(audit.requests, 'Timer actions must not fetch/renew source or mutate API state').toEqual([]);
		expect(audit.errors, 'No uncaught timer/lifecycle exceptions').toEqual([]);
	}
});
test.use({ actionTimeout: 3_000, reducedMotion: 'reduce', hasTouch: true });
test.setTimeout(25_000);

async function receipt(info: TestInfo, expected: unknown, actual: unknown, status?: string) {
	const outcome = status ?? (info.errors.length ? 'RED' : 'GREEN');
	const body = JSON.stringify({ case_id: info.title.split(' ')[0], expected, actual,
		status: outcome,
		error_code: outcome === 'ERROR' ? 'HARNESS_OR_ACTION_FAILURE' : outcome === 'RED' ? 'LIBRARY_SLEEP_CONTRACT' : null,
		next_action: outcome === 'ERROR' ? 'repair harness/action; not verified feature RED' : outcome === 'RED' ? 'implement real library sleep deadline; preserve protected bank' : 'preserve acceptance' });
	console.log(body);
	await info.attach('acceptance-case.json', { body, contentType: 'application/json' });
}
async function check(info: TestInfo, expected: unknown, actual: unknown) {
	try { expect(actual).toEqual(expected); }
	catch (error) { await receipt(info, expected, actual, 'RED'); throw error; }
}
async function ready(page: Page, info: TestInfo) {
	const count = await hook(page, 'panel').count();
	await check(info, { library_sleep_panels: 1 }, { library_sleep_panels: count });
	await expect(hook(page, 'panel')).toBeVisible();
}
test.afterEach(async ({}, info) => {
	if (info.attachments.some((a) => a.name === 'acceptance-case.json')) return;
	const error = info.errors[0];
	await receipt(info, 'all behavioral assertions pass', error ? info.errors.map((e) => e.message) : 'all behavioral assertions passed',
		error ? (/Timeout|Error:.*(?:launch|import)/.test(error.message ?? '') ? 'ERROR' : 'RED') : 'GREEN');
});
async function arm(page: Page, value = '1') {
	await hook(page, 'minutes').fill(value);
	await hook(page, 'start').click();
	await expect(hook(page, 'cancel')).toBeEnabled();
}
async function resetOps(page: Page) { await page.evaluate(() => { window.librarySleepProbe.ops = []; }); }
async function ops(page: Page) { return page.evaluate(() => window.librarySleepProbe.ops); }
async function currentId(page: Page) { return audio(page).evaluate((e: HTMLAudioElement) => window.librarySleepProbe.ids.get(e)!); }
async function quietRemaining(page: Page, text: RegExp) {
	await expect(hook(page, 'remaining')).toBeVisible();
	await expect(hook(page, 'remaining')).toContainText(text);
	expect(await hook(page, 'remaining').evaluate((e) => Boolean(e.closest('[aria-live]:not([aria-live="off"]),[role="status"],[role="alert"]') || e.querySelector('[aria-live]:not([aria-live="off"]),[role="status"],[role="alert"]')))).toBe(false);
}
async function visibility(page: Page, value: 'hidden' | 'visible') {
	await page.evaluate((value) => {
		Object.defineProperty(document, 'visibilityState', { configurable: true, value });
		Object.defineProperty(document, 'hidden', { configurable: true, value: value === 'hidden' });
		document.dispatchEvent(new Event('visibilitychange'));
	}, value);
}
async function jump(page: Page, seconds: number) {
	const now = await page.evaluate(() => Date.now());
	await page.clock.setSystemTime(now + seconds * 1_000);
}
async function remember(page: Page) {
	await page.evaluate(() => { window.librarySleepProbe.stale.push(...[...window.librarySleepProbe.tasks.values()].map((t) => t.fire)); });
}
async function replay(page: Page) {
	await page.evaluate(() => { for (const fire of [...window.librarySleepProbe.stale]) fire(); });
}
async function queue(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((e) => ({ id: e.getAttribute('data-track-id'), current: e.getAttribute('aria-current') })));
}
async function inlineError(page: Page) {
	const input = hook(page, 'minutes');
	await expect(input).toHaveAttribute('aria-invalid', 'true');
	const text = await input.evaluate((e) => `${e.getAttribute('aria-describedby') ?? ''} ${e.getAttribute('aria-errormessage') ?? ''}`.split(/\s+/).flatMap((id) => {
		const node = document.getElementById(id);
		return node?.getClientRects().length ? [node.textContent ?? ''] : [];
	}).join(' '));
	expect(text).toMatch(/enter|choose|use|provide|try|type|select/i);
	expect(text).toMatch(/minute|duration/i);
	return text;
}
async function noError(page: Page, stale: string) {
	await expect(hook(page, 'minutes')).not.toHaveAttribute('aria-invalid', 'true');
	if (stale) expect((await hook(page, 'panel').innerText()).replace(/\s+/g, ' ')).not.toContain(stale.replace(/\s+/g, ' '));
}
async function observeAnnouncements(page: Page) {
	await page.evaluate(() => {
		const root = document.querySelector('[data-testid="library-sleep-panel"]')!;
		const live = '[aria-live="polite"],[role="status"],[aria-live="assertive"],[role="alert"]';
		const p = window.librarySleepProbe; p.announcements = [];
		new MutationObserver((changes) => {
			const regions = new Set<Element>();
			for (const change of changes) {
				const element = change.target instanceof Element ? change.target : change.target.parentElement;
				const parent = element?.closest(live); if (parent) regions.add(parent);
				for (const added of change.addedNodes) if (added instanceof Element) {
					if (added.matches(live)) regions.add(added);
					for (const child of added.querySelectorAll(live)) regions.add(child);
				}
			}
			for (const region of regions) if (region.textContent?.trim()) p.announcements.push(region.textContent.trim());
		}).observe(root, { subtree: true, childList: true, characterData: true });
	});
}

// Each test names the realistic production mutation it detects in its title.
test('LIBSLEEP-01 duration-only labelled controls and truthful suspension guidance', async ({ library: page }, info) => {
	await ready(page, info);
	await expect(hook(page, 'minutes')).toHaveAccessibleName(/minute|duration/i);
	await expect(hook(page, 'start')).toHaveAccessibleName(/start|set/i);
	await expect(hook(page, 'cancel')).toHaveAccessibleName(/cancel/i);
	await expect(hook(page, 'cancel')).toBeDisabled();
	await expect(hook(page, 'status')).toHaveAttribute('aria-live', 'polite');
	await expect(hook(page, 'panel')).toContainText(/wall.clock|even (while|when) paused|continues? (while|when) paused/i);
	await expect(hook(page, 'panel')).toContainText(/background|suspend|return|visible|wake/i);
	expect(await hook(page, 'panel').innerText()).not.toMatch(/stop after|end of (the )?track|exact.*background/i);
	const bounds = await hook(page, 'minutes').evaluate((e: HTMLInputElement) => ({ min: Number(e.min), max: Number(e.max), bounded: e.min !== '' && e.max !== '' }));
	expect(bounds.bounded && Number.isFinite(bounds.min) && Number.isFinite(bounds.max) && bounds.min > 0 && bounds.min <= 1 && bounds.max > 1).toBe(true);
});

test('LIBSLEEP-02 wall clock, not tick count or media position; expiry is once-only and transport-limited', async ({ library: page }, info) => {
	await ready(page, info); await arm(page);
	await quietRemaining(page, /\b1:00\b/);
	const id = await currentId(page), before = await queue(page);
	await audio(page).evaluate((e: HTMLAudioElement) => { e.currentTime = 900; e.playbackRate = 2; e.dispatchEvent(new Event('timeupdate')); });
	await resetOps(page);
	await page.clock.runFor(15_000); await quietRemaining(page, /\b0:45\b/);
	expect(await ops(page)).toEqual([]);
	await remember(page); await page.clock.runFor(45_000);
	await expect.poll(() => audio(page).evaluate((e: HTMLAudioElement) => e.paused)).toBe(true);
	await expect(hook(page, 'cancel')).toBeDisabled();
	await expect(hook(page, 'status')).toContainText(/expir|paused|fired|ended/i);
	await replay(page); await page.clock.runFor(120_000);
	await check(info, [{ element: id, op: 'pause' }], await ops(page));
	expect(await queue(page)).toEqual(before);
});

test('LIBSLEEP-03 valid replacement owns a NEW deadline; old callbacks cannot fire early', async ({ library: page }, info) => {
	await ready(page, info); await arm(page); await remember(page);
	await page.clock.runFor(30_000); await arm(page, '2'); await remember(page);
	await expect(hook(page, 'start')).toHaveAccessibleName(/replace|set/i);
	await quietRemaining(page, /\b2:00\b/); await resetOps(page);
	await page.clock.runFor(31_000); await replay(page);
	expect(await ops(page)).toEqual([]); await quietRemaining(page, /\b1:29\b/);
	await page.clock.runFor(89_000);
	await check(info, [{ element: await currentId(page), op: 'pause' }], await ops(page));
});

for (const invalid of ['', '0', '-1', '1e309', '1e308', 'above-max', 'below-min']) {
	test(`LIBSLEEP-04-${invalid || 'blank'} invalid replacement keeps armed deadline and actionable associated feedback`, async ({ library: page }, info) => {
		await ready(page, info); await arm(page); await page.clock.runFor(15_000);
		const input = hook(page, 'minutes');
		let value = invalid;
		if (value === 'above-max') value = String(Number(await input.getAttribute('max')) + 1);
		if (value === 'below-min') value = String(Number(await input.getAttribute('min')) / 2);
		await input.fill('');
		if (invalid === '1e309') await input.pressSequentially(value); else await input.fill(value);
		await hook(page, 'start').focus(); await page.keyboard.press('Enter');
		await expect(hook(page, 'start')).toBeFocused(); await inlineError(page);
		await quietRemaining(page, /\b0:45\b/);
		await resetOps(page); await page.clock.runFor(44_000); expect(await ops(page)).toEqual([]);
		await page.clock.runFor(1_000);
		await check(info, [{ element: await currentId(page), op: 'pause' }], await ops(page));
	});
}

test('LIBSLEEP-05 advertised inclusive custom bounds work; fractional duration formats canonically', async ({ library: page }, info) => {
	await ready(page, info);
	const input = hook(page, 'minutes');
	const min = Number(await input.getAttribute('min')), max = Number(await input.getAttribute('max'));
	for (const value of [min, max]) {
		expect(value > 0 && Number.isFinite(value)).toBe(true);
		await arm(page, String(value));
		await expect(input).not.toHaveAttribute('aria-invalid', 'true');
	}
	// 1.5 is inside any advertised range that permits it; no arbitrary ceiling.
	if (max >= 1.5) { await arm(page, '1.5'); await quietRemaining(page, /\b1:30\b/); }
	if (max >= 61.5) { await arm(page, '61.5'); await quietRemaining(page, /\b1:01:30\b/); }
	await resetOps(page); await page.clock.runFor(500); expect(await ops(page)).toEqual([]);
});

test('LIBSLEEP-06 labelled presets start/replace the duration they advertise, not a hidden mode', async ({ library: page }, info) => {
	await ready(page, info);
	const presets = hook(page, 'presets'); await expect(presets).toHaveAccessibleName(/sleep|minute|duration|preset/i);
	const buttons = presets.getByRole('button'); expect(await buttons.count()).toBeGreaterThanOrEqual(2);
	const durations: number[] = [];
	for (const button of await buttons.all()) {
		const label = await button.getAttribute('aria-label') || await button.innerText();
		const match = label.match(/(\d+(?:\.\d+)?)\s*(?:minutes?|mins?)\b/i);
		expect(match, 'Presets advertise a duration in minutes').not.toBeNull();
		const minutes = Number(match![1]); durations.push(minutes);
		expect(Number.isFinite(minutes) && minutes > 0).toBe(true);
		await button.tap(); await expect(hook(page, 'cancel')).toBeEnabled();
		const seconds = minutes * 60;
		const display = seconds < 3600 ? `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}` : `${Math.floor(seconds / 3600)}:${String(Math.floor(seconds / 60) % 60).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
		await expect(hook(page, 'remaining')).toContainText(display);
	}
	expect(new Set(durations).size).toBe(durations.length);
});

test('LIBSLEEP-07 cancellation clears invalid feedback and queued work; later callbacks cannot pause', async ({ library: page }, info) => {
	await ready(page, info);
	const idleTasks = await page.evaluate(() => window.librarySleepProbe.tasks.size);
	await arm(page); await remember(page);
	await hook(page, 'minutes').fill('0'); await hook(page, 'start').click(); const error = await inlineError(page);
	await hook(page, 'cancel').click(); await noError(page, error);
	await expect(hook(page, 'cancel')).toBeDisabled();
	await expect(hook(page, 'status')).toContainText(/cancel|off/i);
	await page.clock.runFor(100);
	expect(await page.evaluate(() => window.librarySleepProbe.tasks.size), 'Cancel releases timer-owned work').toBeLessThanOrEqual(idleTasks);
	await resetOps(page); await jump(page, 180); await replay(page); await page.clock.runFor(1_000);
	expect(await ops(page)).toEqual([]);
	expect(await page.evaluate(() => window.librarySleepProbe.tasks.size)).toBeLessThanOrEqual(idleTasks);
	await arm(page, '2'); await quietRemaining(page, /\b2:00\b/); await noError(page, error);
});

test('LIBSLEEP-08 successful replacement clears stale inline validation', async ({ library: page }, info) => {
	await ready(page, info); await arm(page);
	await hook(page, 'minutes').fill('0'); await hook(page, 'start').click(); const stale = await inlineError(page);
	await arm(page, '2'); await noError(page, stale); await quietRemaining(page, /\b2:00\b/);
});

test('LIBSLEEP-09 overdue visibility reconciles wall time immediately and only once', async ({ library: page }, info) => {
	await ready(page, info); await arm(page, '2'); await remember(page); await resetOps(page);
	await visibility(page, 'hidden'); await jump(page, 75); await visibility(page, 'visible');
	await quietRemaining(page, /\b0:45\b/); expect(await ops(page)).toEqual([]);
	await visibility(page, 'hidden'); await jump(page, 90); await visibility(page, 'visible');
	await expect(hook(page, 'cancel')).toBeDisabled();
	await visibility(page, 'visible'); await replay(page); await page.clock.runFor(1_000);
	await check(info, [{ element: await currentId(page), op: 'pause' }], await ops(page));
	expect((await hook(page, 'remaining').allTextContents()).join(' ')).not.toMatch(/-\d|NaN|Infinity/);
});

test('LIBSLEEP-10 selection replaces the native element but never resets the listening deadline', async ({ library: page }, info) => {
	await ready(page, info); await arm(page); await remember(page);
	await page.clock.runFor(20_000);
	await audio(page).evaluate((e: HTMLAudioElement) => window.librarySleepProbe.detached.push(e));
	const oldId = await currentId(page);
	await page.evaluate(() => window.librarySleep.select(1));
	await expect(page.getByTestId('library-now-playing')).toContainText('Pine Morning');
	await audio(page).evaluate((e: HTMLAudioElement) => e.play()); const newId = await currentId(page);
	expect(newId).not.toBe(oldId); await quietRemaining(page, /\b0:40\b/);
	await resetOps(page); const before = await queue(page);
	await page.clock.runFor(40_000); await replay(page);
	await check(info, [{ element: newId, op: 'pause' }], await ops(page));
	expect(await queue(page)).toEqual(before);
});

test('LIBSLEEP-11 manual native pause/resume does not extend or silently rearm deadline', async ({ library: page }, info) => {
	await ready(page, info); await arm(page);
	await page.clock.runFor(15_000); await audio(page).evaluate((e: HTMLAudioElement) => e.pause());
	await page.clock.runFor(15_000); await quietRemaining(page, /\b0:30\b/);
	await audio(page).evaluate((e: HTMLAudioElement) => e.play()); await resetOps(page);
	await page.clock.runFor(30_000); await expect(hook(page, 'cancel')).toBeDisabled();
	await check(info, [{ element: await currentId(page), op: 'pause' }], await ops(page));
	await audio(page).evaluate((e: HTMLAudioElement) => e.play()); await resetOps(page);
	await page.clock.runFor(120_000); expect(await ops(page)).toEqual([]);
	await expect(hook(page, 'cancel')).toBeDisabled();
});

test('LIBSLEEP-12 expiry while native media is already paused stays disarmed and never plays', async ({ library: page }, info) => {
	await ready(page, info); await arm(page);
	await audio(page).evaluate((e: HTMLAudioElement) => e.pause()); await resetOps(page);
	await page.clock.runFor(60_000); await expect(hook(page, 'cancel')).toBeDisabled();
	await expect(hook(page, 'status')).toContainText(/expir|paused|fired|ended/i);
	const atExpiry = await ops(page);
	expect(atExpiry.every((operation) => operation.op === 'pause')).toBe(true); expect(atExpiry.length).toBeLessThanOrEqual(1);
	await page.clock.runFor(120_000); expect(await ops(page)).toEqual(atExpiry);
});

for (const race of ['expiry-then-ended', 'overdue-then-ended', 'error-then-expiry'] as const) {
	test(`LIBSLEEP-13-${race} invalidated playback intent never replays or advances queue`, async ({ library: page }, info) => {
		await ready(page, info); await arm(page); const before = await queue(page); await resetOps(page);
		if (race === 'error-then-expiry') await audio(page).dispatchEvent('error');
		if (race === 'overdue-then-ended') await jump(page, 61); else await page.clock.runFor(60_000);
		await audio(page).evaluate((e: HTMLAudioElement) => {
			Object.defineProperty(e, 'ended', { configurable: true, value: true });
			// A natural end reports native paused=true before the ended callback.
			e.pause(); e.dispatchEvent(new Event('ended')); e.dispatchEvent(new Event('ended'));
		});
		await page.clock.runFor(1_000); await expect(hook(page, 'cancel')).toBeDisabled();
		expect((await ops(page)).filter((o) => o.op !== 'pause')).toEqual([]);
		expect(await queue(page)).toEqual(before); expect(await audio(page).evaluate((e: HTMLAudioElement) => e.paused)).toBe(true);
	});
}

for (const boundary of ['clear', 'source-loss', 'local-auth', 'account-storage', 'unmount'] as const) {
	test(`LIBSLEEP-14-${boundary} boundary cancels work/feedback; queued callbacks cannot touch old or unrelated audio`, async ({ library: page }, info) => {
		await ready(page, info); await arm(page); await remember(page);
		await hook(page, 'minutes').fill('0'); await hook(page, 'start').click(); const staleError = await inlineError(page);
		await audio(page).evaluate((e: HTMLAudioElement) => window.librarySleepProbe.detached.push(e));
		if (boundary === 'clear') await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
		if (boundary === 'source-loss') await page.evaluate((next) => window.librarySleep.replace(next), state(0, [{ ...TRACKS[0], streamUrl: '' }]));
		if (boundary === 'local-auth') await page.evaluate(() => window.librarySleep.auth());
		if (boundary === 'account-storage') await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1', newValue: 'synthetic-next-account' })));
		if (boundary === 'unmount') await page.evaluate(() => window.librarySleep.leave());
		await expect(player(page)).toHaveCount(0);
		await page.clock.runFor(100);
		expect(await page.evaluate(() => window.librarySleepProbe.tasks.size), 'No refresh work remains after source/session/component teardown').toBe(0);
		// Reset after legitimate boundary teardown, then introduce a newly unrelated
		// native element. Old queued closures must not target it or detached audio.
		await page.evaluate(async () => { const e = document.createElement('audio'); e.src = '/__sleep_audio__/unrelated.wav'; document.body.append(e); await e.play(); window.librarySleepProbe.ops = []; });
		await jump(page, 180); await replay(page); await page.clock.runFor(2_000);
		expect(await ops(page)).toEqual([]);
		expect(await page.evaluate(() => window.librarySleepProbe.tasks.size)).toBe(0);
		if (boundary === 'clear' || boundary === 'source-loss') {
			await page.evaluate((next) => window.librarySleep.replace(next), state());
			await expect(hook(page, 'cancel')).toBeDisabled(); await noError(page, staleError);
			await audio(page).evaluate((e: HTMLAudioElement) => e.play()); await resetOps(page);
			await page.clock.runFor(120_000); expect(await ops(page)).toEqual([]);
		}
	});
}

for (const mismatch of ['detached', 'source-mismatch'] as const) {
	test(`LIBSLEEP-15-${mismatch} expiry pauses only a connected current matching source`, async ({ library: page }, info) => {
		await ready(page, info); await arm(page); await remember(page);
		await audio(page).evaluate((e: HTMLAudioElement, mismatch) => {
			window.librarySleepProbe.detached.push(e);
			if (mismatch === 'detached') e.remove(); else e.src = '/__sleep_audio__/unrelated.wav';
		}, mismatch);
		await resetOps(page); await page.clock.runFor(60_000); await replay(page);
		expect(await ops(page)).toEqual([]);
	});
}

test('LIBSLEEP-16 one polite aggregate announcement per start/replace/cancel/fire, no ticking chatter', async ({ library: page }, info) => {
	await ready(page, info); await observeAnnouncements(page);
	await arm(page); await page.clock.runFor(3_000);
	expect(await page.evaluate(() => window.librarySleepProbe.announcements.length)).toBe(1);
	await arm(page, '2');
	expect(await page.evaluate(() => window.librarySleepProbe.announcements.length)).toBe(2);
	await hook(page, 'cancel').click();
	expect(await page.evaluate(() => window.librarySleepProbe.announcements.length)).toBe(3);
	await arm(page); await page.clock.runFor(60_000);
	expect(await page.evaluate(() => window.librarySleepProbe.announcements.length)).toBe(5);
	await page.clock.runFor(120_000);
	expect(await page.evaluate(() => window.librarySleepProbe.announcements.length)).toBe(5);
	const live = await hook(page, 'panel').locator('[aria-live="polite"], [role="status"]').count();
	expect(live).toBe(1);
	expect(await hook(page, 'panel').locator('[aria-live="assertive"], [role="alert"]').count()).toBe(0);
});

test('LIBSLEEP-17 deadline is never persisted or revived after reload', async ({ library: page, audit }, info) => {
	await ready(page, info);
	const saved = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }));
	await arm(page); await page.clock.runFor(5_000);
	expect(await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }))).toEqual(saved);
	expect(audit.requests).toEqual([]);
	await page.reload(); await expect(page.getByTestId('library-item')).toHaveCount(2);
	// A document reload legitimately reloads the mocked metadata, never a source.
	expect(audit.requests).toEqual(['GET /api/library']); audit.requests.length = 0;
	await page.evaluate((next) => window.librarySleep.replace(next), state());
	await expect(hook(page, 'cancel')).toBeDisabled();
	await audio(page).evaluate((e: HTMLAudioElement) => e.play()); await resetOps(page);
	await page.clock.runFor(120_000); expect(await ops(page)).toEqual([]);
});

test('LIBSLEEP-19 overdue start action reconciles old expiry before arming a new deadline', async ({ library: page }, info) => {
	await ready(page, info); await arm(page); await remember(page); await resetOps(page);
	// No refresh tick or visibility event occurs between suspension and action.
	await jump(page, 61); await arm(page, '2');
	await check(info, [{ element: await currentId(page), op: 'pause' }], await ops(page));
	await quietRemaining(page, /\b2:00\b/);
	await replay(page); await page.clock.runFor(1_000);
	expect(await ops(page)).toEqual([{ element: await currentId(page), op: 'pause' }]);
});

test('LIBSLEEP-20 repeated replacement then cancel releases every refresh resource', async ({ library: page }, info) => {
	await ready(page, info);
	const idle = await page.evaluate(() => window.librarySleepProbe.tasks.size);
	for (const value of ['1', '2', '3', '4', '5']) { await arm(page, value); await remember(page); }
	await hook(page, 'cancel').click(); await page.clock.runFor(100);
	expect(await page.evaluate(() => window.librarySleepProbe.tasks.size)).toBeLessThanOrEqual(idle);
	await resetOps(page); await jump(page, 600); await replay(page); await page.clock.runFor(1_000);
	expect(await ops(page)).toEqual([]);
	expect(await page.evaluate(() => window.librarySleepProbe.tasks.size)).toBeLessThanOrEqual(idle);
});

for (const theme of ['light', 'dark'] as const) {
	test(`LIBSLEEP-18-${theme} 360px keyboard/touch controls keep visible focus and contained readable state`, async ({ library: page }, info) => {
		await ready(page, info); await page.setViewportSize({ width: 360, height: 800 });
		await page.emulateMedia({ colorScheme: theme });
		await page.evaluate((theme) => document.documentElement.setAttribute('data-appearance', theme), theme);
		await hook(page, 'minutes').fill('1'); await hook(page, 'start').focus();
		await page.keyboard.press('Enter'); await expect(hook(page, 'cancel')).toBeEnabled();
		const reached = new Set<string>();
		for (let n = 0; n < 50 && reached.size < 3; n += 1) {
			await page.keyboard.press('Tab');
			const id = await page.evaluate(() => document.activeElement?.getAttribute('data-testid'));
			if (id && ['library-sleep-minutes', 'library-sleep-start', 'library-sleep-cancel'].includes(id)) reached.add(id);
		}
		expect([...reached].sort()).toEqual(['library-sleep-cancel', 'library-sleep-minutes', 'library-sleep-start']);
		const focus = await page.evaluate(() => {
			const e = document.activeElement!; const style = getComputedStyle(e);
			return { visible: e.matches(':focus-visible'), ring: parseFloat(style.outlineWidth) > 0 || style.boxShadow !== 'none', inPlayer: Boolean(e.closest('[aria-label="Player"]')) };
		});
		expect(focus).toEqual({ visible: true, ring: true, inPlayer: true });
		await hook(page, 'minutes').focus(); await page.clock.runFor(2_000);
		await expect(hook(page, 'minutes')).toBeFocused(); await quietRemaining(page, /\b0:58\b/);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
		const contained = await hook(page, 'panel').evaluate((e) => { const b = e.getBoundingClientRect(); return b.left >= 0 && b.right <= innerWidth + 1; }); expect(contained).toBe(true);
		const contrast = await hook(page, 'remaining').evaluate((node) => {
			const rgb = (color: string) => color.match(/[\d.]+/g)?.map(Number) ?? [];
			const luminance = (color: number[]) => color.slice(0, 3).map((n) => {
				const c = n / 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
			}).reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i], 0);
			let background: number[] = [];
			for (let e: Element | null = node; e; e = e.parentElement) {
				const color = rgb(getComputedStyle(e).backgroundColor);
				if (color.length === 3 || color[3] === 1) { background = color; break; }
			}
			if (background.length < 3) return 0;
			const values = [luminance(rgb(getComputedStyle(node).color)), luminance(background)].sort((a, b) => b - a);
			return (values[0] + 0.05) / (values[1] + 0.05);
		});
		expect(contrast, 'Remaining time has readable text contrast').toBeGreaterThanOrEqual(4.5);
		await info.attach(`library-sleep-${theme}.png`, { body: await hook(page, 'panel').screenshot(), contentType: 'image/png' });
		await hook(page, 'cancel').tap(); await expect(hook(page, 'cancel')).toBeDisabled();
	});
}
