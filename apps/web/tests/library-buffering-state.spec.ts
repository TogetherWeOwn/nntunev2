import { expect, test as base, type ElementHandle, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-buffering/mount.js';

/**
 * LIBBUFFER — protected offline native-library observation acceptance.
 * Protect this spec and tests/fixtures/library-buffering/{index.html,mount.ts}.
 * No production change, live backend, decoder stall, source URL or grant needed.
 * The same queue-input seam as library-native-now-playing.spec.ts mounts the
 * REAL route: only an exported call to existing replaceQueue is injected.
 * Metadata-only library selection cannot supply audio, so playable queue input
 * is necessary; no streaming feature/persistence is invented to seed it.
 *
 * Presentation contract: ONE visible polite state-transition region scoped to
 * the existing Player region; state words loading/playing/buffering/paused/ended
 * are semantic, other copy and testids are not pinned. No required error copy,
 * retry UI, internal policy module path or MSE engine API.
 *
 * Breaks caught: play-promise/metadata optimism; initial-wait/stalled false
 * positives; ignored recovery; retained playing history after pause/reset;
 * detached element writes; transport/network/timer side effects; duplicate or
 * progress announcements; stolen focus and narrow-screen unreadability.
 *
 * Run with cached dependencies/browser:
 * CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *   tests/library-buffering-state.spec.ts tests/player-buffering.spec.ts --workers=1 --retries=0
 * The runner owns preview/component servers; no persistent service is created.
 * Each case emits expected/actual/status/error_code/next_action JSON. Snapshot
 * assertions make missing status assertion-RED, not a missing-selector timeout.
 * Existing guard GREENs are intentional, never claimed as new-feature RED.
 */

const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const routePath = `${webRoot}src/routes/library/+page.svelte`;
const TRACKS: Track[] = [
	{ id: 'lb-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '/__acceptance_audio__/coral.wav' },
	{ id: 'lb-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '/__acceptance_audio__/pine.wav' }
];
const queue = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const audio = (page: Page) => player(page).locator('audio[controls]');
const region = (page: Page) => player(page).locator('[role="status"], [aria-live="polite"]');
type Counts = { play: number; pause: number; load: number; seek: number; fetch: number; xhr: number; timers: number };
type Probe = {
	counts: Counts; recording: boolean; calibrating: boolean; eventTimerSite: string | null;
	announcements: string[]; observer?: MutationObserver;
};
declare global { interface Window { libraryBufferingProbe: Probe } }

const test = base.extend<{ libraryPage: Page }, { libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: `${webRoot}tests/fixtures/library-buffering`, publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': `${webRoot}tests/fixtures/library-now-playing/navigation.ts`
			} },
			plugins: [{
				name: 'acceptance-buffering-queue-input', enforce: 'pre',
				transform(code, id) {
					if (id !== routePath) return;
					if (!code.includes('</script>')) throw new Error('library script boundary missing');
					return code.replace('</script>', '\nexport function acceptanceReplaceQueue(next: QueueState) { replaceQueue(next); }\n</script>');
				}
			}, svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [webRoot] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('no fixture TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally { await server.close(); }
	}, { scope: 'worker' }],
	libraryPage: async ({ page, libraryOrigin }, use) => {
		await offline(page, libraryOrigin);
		await mediaBoundary(page);
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryBuffering))).toBe(true);
		await use(page);
	}
});
test.setTimeout(25_000);
test.use({ viewport: { width: 1000, height: 740 } });

async function offline(page: Page, origin: string) {
	const expected = new URL(origin).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (url.origin !== expected || request.resourceType() === 'media') return route.abort();
		if (url.pathname === '/api/library' && request.method() === 'GET') {
			return route.fulfill({ status: 200, json: TRACKS.map(({ id, title, artist }) => ({ id, title, artist })) });
		}
		if (/^\/(?:api|v1|__acceptance_audio__)(?:\/|$)/.test(url.pathname)) return route.abort();
		return route.continue();
	});
}

async function mediaBoundary(page: Page) {
	await page.addInitScript(() => {
		sessionStorage.clear(); localStorage.clear();
		const probe: Probe = {
			counts: { play: 0, pause: 0, load: 0, seek: 0, fetch: 0, xhr: 0, timers: 0 },
			recording: false, calibrating: false, eventTimerSite: null, announcements: []
		};
		window.libraryBufferingProbe = probe;
		const positions = new WeakMap<HTMLMediaElement, number>();
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get() { return positions.get(this) ?? 0; },
			set(value: number) { probe.counts.seek++; positions.set(this, value); }
		});
		// Mock only browser transport; play promise resolution is deliberately NOT
		// an event. Tests deliver actual native play/playing/paused truth separately.
		HTMLMediaElement.prototype.play = function () {
			probe.counts.play++;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.counts.pause++;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		HTMLMediaElement.prototype.load = function () { probe.counts.load++; };
		const fetch = window.fetch.bind(window);
		window.fetch = (...args) => { if (probe.recording) probe.counts.fetch++; return fetch(...args); };
		const send = XMLHttpRequest.prototype.send;
		XMLHttpRequest.prototype.send = function (body) {
			if (probe.recording) probe.counts.xhr++;
			return send.call(this, body);
		};
		const timeout = window.setTimeout.bind(window);
		const interval = window.setInterval.bind(window);
		window.setTimeout = ((handler: TimerHandler, ms?: number, ...args: unknown[]) => {
			if (probe.recording) {
				// Match the established demo acceptance calibration: Svelte schedules
				// event-dispatch cleanup. All other sites and every interval count.
				const site = new Error().stack?.split('\n')[2] ?? null;
				if (probe.calibrating && probe.eventTimerSite === null) probe.eventTimerSite = site;
				if (site === null || site !== probe.eventTimerSite) probe.counts.timers++;
			}
			return timeout(handler, ms, ...args);
		}) as typeof window.setTimeout;
		window.setInterval = ((handler: TimerHandler, ms?: number, ...args: unknown[]) => {
			if (probe.recording) probe.counts.timers++;
			return interval(handler, ms, ...args);
		}) as typeof window.setInterval;
	});
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
async function replace(page: Page, next = queue()) {
	await page.evaluate((value) => window.libraryBuffering.replace(value), next);
	await settle(page);
}
async function event(page: Page, type: string, target?: ElementHandle, calibrating = false) {
	const handle = target ?? await audio(page).elementHandle();
	if (!handle) throw new Error('fixture failed to render native controls');
	await handle.evaluate(async (node, { type, calibrating }) => {
		const media = node as HTMLAudioElement;
		const probe = window.libraryBufferingProbe;
		if (type === 'play') {
			Object.defineProperty(media, 'paused', { value: false, configurable: true });
			Object.defineProperty(media, 'ended', { value: false, configurable: true });
		}
		if (type === 'pause' || type === 'ended') Object.defineProperty(media, 'paused', { value: true, configurable: true });
		if (type === 'ended') Object.defineProperty(media, 'ended', { value: true, configurable: true });
		probe.calibrating = calibrating; probe.recording = true;
		try {
			media.dispatchEvent(new Event(type));
			await Promise.resolve(); await Promise.resolve();
		} finally { probe.recording = false; probe.calibrating = false; }
	}, { type, calibrating });
	await settle(page);
	if (!target) await handle.dispose();
}
async function start(page: Page) {
	await replace(page);
	await event(page, 'play');
	await event(page, 'playing', undefined, true);
}
async function state(page: Page) {
	const texts = await region(page).allTextContents();
	return texts.length === 1 ? texts[0].match(/\b(loading|playing|buffering|paused|ended)\b/i)?.[1].toLowerCase() ?? '<inert>' : '<missing-or-duplicate>';
}
async function observe(page: Page) {
	await page.evaluate(() => {
		const probe = window.libraryBufferingProbe;
		probe.observer?.disconnect(); probe.announcements = [];
		const parent = document.querySelector('[aria-label="Player"]');
		const node = parent?.querySelector('[role="status"], [aria-live="polite"]');
		if (!node) return;
		probe.observer = new MutationObserver(() => probe.announcements.push(node.textContent?.trim() ?? ''));
		probe.observer.observe(node, { childList: true, characterData: true, subtree: true });
	});
}
async function announcements(page: Page) { return page.evaluate(() => [...window.libraryBufferingProbe.announcements]); }
async function snapshot(page: Page) {
	return page.evaluate(() => {
		const el = document.querySelector<HTMLAudioElement>('audio[controls]');
		return {
			counts: { ...window.libraryBufferingProbe.counts }, time: el?.currentTime,
			source: el?.getAttribute('src'), controls: el?.controls,
			queue: Array.from(document.querySelectorAll('[data-testid="queue-item"]'), (row) => row.getAttribute('data-track-id'))
		};
	});
}
async function check(info: TestInfo, expected: unknown, actual: unknown) {
	let passed = false;
	try { expect(actual).toEqual(expected); passed = true; }
	finally {
		const body = JSON.stringify({
			case_id: info.title.split(' ')[0], expected, actual, status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : 'LIBRARY_BUFFERING_CONTRACT',
			next_action: passed ? 'preserve acceptance' : 'implement native observation; do not edit protected tests'
		});
		console.log(body);
		await info.attach('acceptance-case.json', { body, contentType: 'application/json' });
	}
}
test.afterEach(async ({}, info) => {
	if (info.attachments.some((attachment) => attachment.name === 'acceptance-case.json')) return;
	const body = JSON.stringify({
		case_id: info.title.split(' ')[0], expected: 'reach behavioral assertion',
		actual: info.errors.map((error) => error.message), status: 'ERROR', error_code: 'HARNESS_OR_ACTION_FAILURE',
		next_action: 'repair harness/action; not verified feature RED'
	});
	console.log(body); await info.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

test('LIBBUFFER-LB01 initial waits, stalled and a resolved play promise cannot claim playing', async ({ libraryPage: page }, info) => {
	await replace(page);
	const states = [await state(page)];
	await event(page, 'waiting'); await event(page, 'stalled'); states.push(await state(page));
	await event(page, 'playing'); states.push(await state(page)); // Still paused: no active intent.
	await audio(page).evaluate((el: HTMLAudioElement) => el.play());
	await event(page, 'play'); states.push(await state(page));
	await event(page, 'waiting'); states.push(await state(page));
	await check(info, ['loading', 'loading', 'loading', 'loading', 'loading'], states);
});

test('LIBBUFFER-LB02 actual playing then waiting buffers, actual playing recovers', async ({ libraryPage: page }, info) => {
	await start(page);
	const states = [await state(page)];
	await event(page, 'waiting'); states.push(await state(page));
	await event(page, 'playing'); states.push(await state(page));
	const accessibility = await region(page).evaluateAll((nodes) => ({
		count: nodes.length,
		polite: nodes.length === 1 && nodes[0].getAttribute('aria-live') !== 'assertive' && nodes[0].getAttribute('aria-live') !== 'off',
		visible: nodes.length === 1 && nodes[0].getClientRects().length > 0 && nodes[0].getAttribute('aria-hidden') !== 'true',
		compact: nodes.length === 1 && (nodes[0].textContent?.trim().length ?? 0) <= 120
	}));
	await check(info, { states: ['playing', 'buffering', 'playing'], accessibility: { count: 1, polite: true, visible: true, compact: true } }, { states, accessibility });
});

test('LIBBUFFER-LB03 stalled alone is neither buffering nor recovery', async ({ libraryPage: page }, info) => {
	await start(page); await observe(page);
	await event(page, 'stalled'); await event(page, 'stalled');
	const audible = await state(page), quiet = await announcements(page);
	await event(page, 'waiting'); await event(page, 'stalled');
	await check(info, { audible: 'playing', quiet: [], afterWait: 'buffering' }, { audible, quiet, afterWait: await state(page) });
});

test('LIBBUFFER-LB04 duplicate and timeupdate events announce only state transitions', async ({ libraryPage: page }, info) => {
	await start(page); await observe(page);
	for (const type of ['waiting', 'waiting', 'stalled', 'waiting', 'timeupdate', 'timeupdate']) await event(page, type);
	const entered = await announcements(page);
	for (const type of ['playing', 'playing', 'timeupdate', 'timeupdate']) await event(page, type);
	const recovered = await announcements(page);
	await check(info, { entered: 1, buffering: true, recovered: 2, playing: true, state: 'playing', assertive: 0 }, {
		entered: entered.length, buffering: entered.length === 1 && /\bbuffering\b/i.test(entered[0]),
		recovered: recovered.length, playing: recovered.length === 2 && /\bplaying\b/i.test(recovered[1]),
		state: await state(page), assertive: await player(page).locator('[aria-live="assertive"], [role="alert"]').count()
	});
});

test('LIBBUFFER-LB05 pause clears history, stale playing cannot resume, fresh play still needs actual playing', async ({ libraryPage: page }, info) => {
	await start(page); await event(page, 'waiting');
	const states = [await state(page)];
	await event(page, 'pause'); states.push(await state(page));
	for (const type of ['waiting', 'playing', 'stalled']) await event(page, type);
	states.push(await state(page));
	await event(page, 'play'); await event(page, 'waiting'); states.push(await state(page));
	await event(page, 'playing'); states.push(await state(page));
	await event(page, 'waiting'); states.push(await state(page));
	await check(info, ['buffering', 'paused', 'paused', 'loading', 'playing', 'buffering'], states);
});

test('LIBBUFFER-LB06 natural end clears history and queued events cannot revive playback', async ({ libraryPage: page }, info) => {
	await start(page); await event(page, 'waiting');
	const before = await state(page);
	await event(page, 'pause'); await event(page, 'ended');
	const ended = await state(page);
	for (const type of ['waiting', 'playing', 'stalled']) await event(page, type);
	await check(info, { before: 'buffering', ended: 'ended', stale: 'ended' }, { before, ended, stale: await state(page) });
});

for (const [index, reset] of ['error', 'emptied'].entries()) {
	test(`LIBBUFFER-LB${String(7 + index).padStart(2, '0')} ${reset} makes the observer inert until a fresh native play lifecycle`, async ({ libraryPage: page }, info) => {
		await start(page); await event(page, 'waiting');
		const before = await state(page);
		await event(page, reset);
		const cleared = await state(page);
		const transport = await snapshot(page);
		for (const type of ['waiting', 'playing', 'stalled']) await event(page, type);
		const stale = await state(page);
		const unchanged = JSON.stringify(await snapshot(page)) === JSON.stringify(transport);
		await event(page, 'play'); await event(page, 'waiting');
		const newStart = await state(page);
		await event(page, 'playing'); await event(page, 'waiting');
		await check(info, { before: 'buffering', cleared: true, inert: true, unchanged: true, newStart: 'loading', resumed: 'buffering' }, {
			before, cleared: !['playing', 'buffering'].includes(cleared), inert: cleared === stale,
			unchanged, newStart, resumed: await state(page)
		});
	});
}

test('LIBBUFFER-LB09 keyed source replacement fences both detached waits and detached recovery', async ({ libraryPage: page }, info) => {
	await start(page);
	const old = await audio(page).elementHandle();
	await event(page, 'waiting');
	await page.getByTestId('player-next').click(); await settle(page);
	const replaced = !await old!.evaluate((el) => el.isConnected);
	for (const type of ['playing', 'waiting', 'stalled']) await event(page, type, old!);
	await event(page, 'waiting'); const initial = await state(page);
	await event(page, 'play'); await event(page, 'playing');
	await event(page, 'waiting', old!); const playing = await state(page);
	await event(page, 'waiting');
	for (const type of ['playing', 'stalled', 'waiting']) await event(page, type, old!);
	await check(info, { replaced: true, source: '/__acceptance_audio__/pine.wav', initial: 'loading', playing: 'playing', buffering: 'buffering' }, {
		replaced, source: await audio(page).getAttribute('src'), initial, playing, buffering: await state(page)
	});
	await old!.dispose();
});

test('LIBBUFFER-LB10 same track with a new source resets prior-playing history', async ({ libraryPage: page }, info) => {
	await start(page); await event(page, 'waiting');
	const old = await audio(page).elementHandle();
	await replace(page, queue(0, [{ ...TRACKS[0], streamUrl: '/__acceptance_audio__/coral-replaced.wav' }]));
	await event(page, 'playing', old!); await event(page, 'waiting', old!);
	await event(page, 'play'); await event(page, 'waiting');
	await check(info, { replaced: true, state: 'loading' }, { replaced: !await old!.evaluate((el) => el.isConnected), state: await state(page) });
	await old!.dispose();
});

test('LIBBUFFER-LB11 observation never transports, seeks, requests, retries or changes the queue', async ({ libraryPage: page }, info) => {
	await start(page);
	await audio(page).evaluate((el: HTMLAudioElement) => { el.currentTime = 42; });
	const requests: string[] = [];
	page.on('request', (request) => requests.push(new URL(request.url()).pathname));
	const before = await snapshot(page);
	for (const type of ['waiting', 'waiting', 'stalled', 'playing', 'pause', 'waiting', 'playing', 'play', 'waiting', 'playing', 'waiting', 'timeupdate']) await event(page, type);
	await check(info, { unchanged: true, requests: [], recoveryControls: 0, diagnosis: false, state: 'buffering' }, {
		unchanged: JSON.stringify(await snapshot(page)) === JSON.stringify(before), requests,
		recoveryControls: await player(page).getByRole('button', { name: /retry|reload/i }).count(),
		diagnosis: /provider|network|offline|timeout/i.test((await region(page).allTextContents()).join(' ')), state: await state(page)
	});
});

test('LIBBUFFER-LB12 auth teardown and unmount fence detached events without resurrecting a player', async ({ libraryPage: page }, info) => {
	await start(page); await event(page, 'waiting');
	const old = await audio(page).elementHandle();
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1', newValue: 'synthetic-next-account'
	})));
	await settle(page);
	const before = await snapshot(page);
	for (const type of ['playing', 'waiting', 'stalled', 'pause', 'ended']) await event(page, type, old!);
	const auth = { players: await player(page).count(), queue: await page.getByTestId('queue-item').count(), unchanged: JSON.stringify(await snapshot(page)) === JSON.stringify(before) };
	await page.evaluate(() => window.libraryBuffering.destroy()); await settle(page);
	const unmounted = await snapshot(page);
	for (const type of ['playing', 'waiting', 'stalled']) await event(page, type, old!);
	await check(info, { auth: { players: 0, queue: 0, unchanged: true }, unmounted: true, unchanged: true }, {
		auth, unmounted: await page.locator('audio, [aria-label="Player"]').count() === 0,
		unchanged: JSON.stringify(await snapshot(page)) === JSON.stringify(unmounted)
	});
	await old!.dispose();
});

test('LIBBUFFER-LB13 lifecycle transitions preserve keyboard focus and the same native controls', async ({ libraryPage: page }, info) => {
	await start(page);
	const native = await audio(page).elementHandle();
	const next = page.getByTestId('player-next');
	await next.focus(); await page.keyboard.press('Tab'); await page.keyboard.press('Shift+Tab');
	const states: string[] = [];
	let retained = await next.evaluate((el) => el === document.activeElement);
	for (const type of ['waiting', 'playing', 'pause', 'waiting']) {
		await event(page, type); states.push(await state(page));
		retained &&= await next.evaluate((el) => el === document.activeElement);
	}
	await check(info, { states: ['buffering', 'playing', 'paused', 'paused'], retained: true, same: true, controls: true, enabled: true }, {
		states, retained, same: await native!.evaluate((el) => el === document.querySelector('audio')),
		controls: await audio(page).evaluate((el: HTMLAudioElement) => el.controls), enabled: await next.isEnabled()
	});
	await native!.dispose();
});

test('LIBBUFFER-LB14 metadata-only library selection never claims native playing', async ({ libraryPage: page }, info) => {
	await page.locator('[data-testid="library-item"][data-id="lb-coral"] button.nt-library-row').click();
	await settle(page);
	await check(info, { tracks: 1, audio: 0, nativeStates: 0 }, {
		tracks: await page.getByTestId('queue-item').count(), audio: await page.locator('audio').count(), nativeStates: await region(page).count()
	});
});

test('LIBBUFFER-LB17 unmount while buffering removes the observer and fences detached lifecycle events', async ({ libraryPage: page }, info) => {
	await start(page); await event(page, 'waiting');
	const before = await state(page);
	const old = await audio(page).elementHandle();
	await page.evaluate(() => window.libraryBuffering.destroy()); await settle(page);
	const removed = !await old!.evaluate((el) => el.isConnected);
	const transport = await snapshot(page);
	for (const type of ['playing', 'waiting', 'stalled', 'pause', 'ended', 'error', 'emptied']) await event(page, type, old!);
	await check(info, { before: 'buffering', removed: true, players: 0, unchanged: true }, {
		before, removed, players: await player(page).count(), unchanged: JSON.stringify(await snapshot(page)) === JSON.stringify(transport)
	});
	await old!.dispose();
});

for (const [index, theme] of ['light', 'dark'].entries()) {
	test(`LIBBUFFER-LB${15 + index} ${theme} narrow buffering status is readable, contained and nonassertive`, async ({ libraryPage: page }, info) => {
		await page.setViewportSize({ width: 360, height: 740 });
		await page.emulateMedia({ colorScheme: theme as 'light' | 'dark' });
		await page.evaluate((value) => document.documentElement.setAttribute('data-appearance', value), theme);
		await start(page); await event(page, 'waiting');
		const layout = await region(page).evaluateAll((nodes) => {
			if (nodes.length !== 1) return { contained: false, readable: false };
			const node = nodes[0], box = node.getBoundingClientRect();
			const rgb = (value: string) => value.match(/[\d.]+/g)?.map(Number) ?? [];
			const luminance = (color: number[]) => color.slice(0, 3).map((n) => {
				const c = n / 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
			}).reduce((sum, n, i) => sum + n * [0.2126, 0.7152, 0.0722][i], 0);
			let background: number[] = [];
			for (let el: Element | null = node; el; el = el.parentElement) {
				const value = rgb(getComputedStyle(el).backgroundColor);
				if (value.length === 3 || value[3] === 1) { background = value; break; }
			}
			const values = [luminance(rgb(getComputedStyle(node).color)), luminance(background)].sort((a, b) => b - a);
			return {
				contained: box.width > 0 && box.x >= 0 && box.right <= innerWidth + 1 && document.documentElement.scrollWidth <= innerWidth,
				readable: background.length >= 3 && (values[0] + 0.05) / (values[1] + 0.05) >= 4.5
			};
		});
		await info.attach(`native-buffering-${theme}.png`, { body: await page.screenshot(), contentType: 'image/png' });
		await check(info, { state: 'buffering', layout: { contained: true, readable: true }, assertive: 0 }, {
			state: await state(page), layout, assertive: await player(page).locator('[aria-live="assertive"], [role="alert"]').count()
		});
	});
}
