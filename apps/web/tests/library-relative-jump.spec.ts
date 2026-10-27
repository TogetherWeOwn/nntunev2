import { expect, test as base, type ElementHandle, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-relative-jump/mount.js';

/**
 * LIBJUMP10 — jump native library playback backward/forward ten seconds.
 *
 * Protected: this spec + tests/fixtures/library-relative-jump/*.
 * Builder-owned implementation: src/routes/library/+page.svelte and, if
 * needed, a bounded src/lib/player/* helper. Existing suites stay unchanged.
 * Continue the handoff branch; open a combined GREEN PR only.
 *
 * Contract the build implements (stable case IDs LIBJUMP10-01..LIBJUMP10-12):
 *
 * Hooks (literal `data-testid`, same convention as the demo jump pins):
 *   player-seek-back ...... native <button>, accessible name matching /back 10/i
 *   player-seek-forward ... native <button>, accessible name matching /forward 10/i
 *   player-seek-feedback .. role="status" one-shot readout in the Player region
 * All three live in the `region` named "Player", beside the native
 * audio[controls]. The buttons are distinct from track transport
 * (player-next), keyboard-focusable with a visible focus indicator,
 * pointer- and touch-usable at 320px width in light and dark.
 *
 * Behavior: each activation moves the CURRENT matching mounted audio
 * element by exactly 10 seconds, clamped to [0, finite duration]. Usable
 * media means a finite positive native duration and a finite in-range
 * currentTime; anything else disables the control (with an honest reason
 * via `title` or `aria-describedby`) or refuses the activation — never a
 * throw through the UI, never fabricated success. Refused native
 * assignments are caught and reported once; no retry loop. Jumps
 * preserve paused/playing intent (never call play/load, never flip
 * paused), volume, playback rate, audio source, current id and queue
 * order. An explicit jump supersedes pending inbound-share seeking, so
 * later metadata events cannot undo the gesture; source change and auth
 * teardown invalidate all pending state (the route already clears pending
 * share seeks there — the pins guard the behavior with the feature on).
 * Missing/stale media and boundary no-ops never report a jump that did
 * not occur: feedback after a no-op/refusal is non-empty and never starts
 * with "jumped"/"seeked"/"moved to", while a real jump names the action
 * (/10 second/i). The demo SeekController, manual fetch bytes, catalog
 * duration invention and the unbuilt timeline stay out; global key
 * semantics are unchanged (the library player has no j/l transport, rate
 * switch or sleep timer to preserve — volume/rate are pinned instead).
 *
 * Pure guard vectors (doubled through the DOM, never source-token pins):
 *   +/-10 from 30 and 30.5 (fraction kept), 3->0 and 115->120 clamps,
 *   no-ops at 0 and duration, NaN/Infinity/zero duration, NaN/negative/
 *   out-of-range time, refusing setter, stale-element metadata/error,
 *   pending-share supersession and source-change/auth cleanup.
 * No unit-test file is pinned: the bounded helper does not exist yet, and
 * a spec importing it would fail on resolution instead of behavior. The
 * builder adds helper unit tests with the implementation.
 *
 * Determinism: fixture cases mount the REAL library page in an ephemeral
 * Vite server (same pre-transform + reducer bridge discipline as the
 * now-playing bank) with the demo-jump pending-media harness (stubbed
 * play/pause/load, per-element duration/currentTime/paused doubles, held
 * routes, no real decoder, no sleeps, no live backend). Share/auth cases
 * use the real /library route with stream-URL catalogue fixtures and a
 * dispatched storage auth-transition event. CTRL-01/CTRL-02 and GUARD-13
 * pass with and without the feature; they prove the harness and the
 * empty-queue contract, not the feature. Every other case is RED on base
 * by an intended assertion naming the missing hook.
 *
 * Setup (cached packages/browser required for offline use):
 *   pnpm --dir apps/web install --offline --frozen-lockfile
 * Run (4173 must be free; CI=1 prevents reuse of another worktree's preview):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-relative-jump.spec.ts --workers=1 --retries=0
 *
 * Every case emits/attaches compact JSON expected/actual/status/error_code/
 * next_action. Missing hooks are assertion-RED, never an import failure.
 */

const routePath = decodeURIComponent(new URL('../src/routes/library/+page.svelte', import.meta.url).pathname);
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const test = base.extend<{ libraryPage: Page }, { libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/library-relative-jump/', import.meta.url).pathname),
			publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': decodeURIComponent(new URL('./fixtures/library-relative-jump/navigation.ts', import.meta.url).pathname)
			} },
			plugins: [{
				name: 'acceptance-library-queue-input', enforce: 'pre',
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
		await stubTransport(page);
		await localOnly(page, libraryOrigin);
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryRelativeJump))).toBe(true);
		await use(page);
	}
});

test.setTimeout(30_000);
test.use({ viewport: { width: 1000, height: 740 }, hasTouch: true });

const TRACKS: Track[] = [
	{ id: 'lj-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '/__acceptance_audio__/coral.wav' },
	{ id: 'lj-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '/__acceptance_audio__/pine.wav' },
	{ id: 'lj-ember', title: 'Ember Noon', artist: 'West Quartet', streamUrl: '/__acceptance_audio__/ember.wav' }
];
const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const audio = (page: Page) => player(page).locator('audio[data-testid="audio-player"]');
const feedback = (page: Page) => player(page).getByTestId('player-seek-feedback');

type MediaControl = { playCalls: number; loadCalls: number; resolve: () => void; reject: () => void };
type Calls = { playCalls: number; loadCalls: number };

/** Stub play/pause/load before navigation: only fixture events settle playback. */
async function stubTransport(page: Page) {
	await page.addInitScript(() => {
		for (const storage of [sessionStorage, localStorage]) {
			try {
				for (let i = storage.length - 1; i >= 0; i -= 1) {
					const key = storage.key(i);
					if (key?.startsWith('nntune.')) storage.removeItem(key);
				}
			} catch { /* private-mode storage stays untouched */ }
		}
		const control: MediaControl = { playCalls: 0, loadCalls: 0, resolve: () => {}, reject: () => {} };
		Object.assign(window, { __jumpLifecycle: control });
		HTMLMediaElement.prototype.play = function (): Promise<void> {
			control.playCalls += 1;
			return new Promise<void>((resolve, reject) => {
				control.resolve = resolve;
				control.reject = () => reject(new Error('private decoder details'));
			});
		};
		HTMLMediaElement.prototype.pause = function () {};
		const rawLoad = HTMLMediaElement.prototype.load;
		HTMLMediaElement.prototype.load = function () {
			control.loadCalls += 1;
			return rawLoad.call(this);
		};
	});
}

async function lifecycleOf(page: Page): Promise<Calls> {
	return page.evaluate(() => {
		const control = (window as unknown as { __jumpLifecycle: MediaControl }).__jumpLifecycle;
		return { playCalls: control.playCalls, loadCalls: control.loadCalls };
	});
}

/** Local preview/fixture traffic only; the catalogue carries stream URLs so row
 *  selection and share links yield the same playable queue the bridge seeds. */
async function localOnly(page: Page, origin: string) {
	const expectedOrigin = new URL(origin).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== expectedOrigin || request.resourceType() === 'media') return route.abort();
		if (target.pathname === '/api/library' && request.method() === 'GET') {
			return route.fulfill({ status: 200, json: TRACKS });
		}
		if (/^\/(?:api|v1|__acceptance_audio__)(?:\/|$)/.test(target.pathname)) return route.abort();
		return route.continue();
	});
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

/** Flush one frame so a refused activation (or a leaked error) can land. */
async function flushFrame(page: Page) {
	await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => done(null))));
}

async function replace(page: Page, next = state()) {
	await page.evaluate((value) => window.libraryRelativeJump.replace(value), next);
	await settle(page);
}

/** Blur whatever hydrated with focus so no player control holds it. */
async function focusBody(page: Page) {
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	expect(await page.evaluate(() => document.activeElement === document.body)).toBe(true);
}

/**
 * Deterministic media double on the CURRENT audio element: finite or
 * sentinel duration, a writable currentTime and a pinned paused flag.
 * Re-apply after any remount (track change, retry) swaps the element.
 */
async function mockMedia(page: Page, init: { currentTime: number; duration: number; paused?: boolean }) {
	await audio(page).evaluate(
		(el: HTMLAudioElement, seed: { currentTime: number; duration: number; paused: boolean }) => {
			let time = seed.currentTime;
			Object.defineProperty(el, 'duration', { value: seed.duration, configurable: true });
			Object.defineProperty(el, 'paused', { value: seed.paused, configurable: true });
			Object.defineProperty(el, 'currentTime', {
				get: () => time,
				set: (next: number) => { time = next; },
				configurable: true
			});
		},
		{ ...init, paused: init.paused ?? true }
	);
}

async function mediaTime(page: Page): Promise<number> {
	return audio(page).evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function mediaPaused(page: Page): Promise<boolean> {
	return audio(page).evaluate((el: HTMLAudioElement) => el.paused);
}

async function queueIds(page: Page): Promise<(string | null)[]> {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((item) => item.getAttribute('data-track-id')));
}

async function currentId(page: Page): Promise<string | null> {
	return page.locator('[data-testid="queue-item"][aria-current="true"]').getAttribute('data-track-id');
}

/**
 * Presence gate for the new hooks: 'missing' must fail the test via the
 * case receipt (assertion-RED, never an import/compile failure). Never
 * click a disabled button — Playwright would time out waiting for it to
 * enable instead of testing the no-op path.
 */
async function hookState(page: Page, testid: string): Promise<'missing' | 'disabled' | 'enabled'> {
	const button = page.getByTestId(testid);
	if ((await button.count()) === 0) return 'missing';
	return (await button.isDisabled()) ? 'disabled' : 'enabled';
}

/** A disabled jump button must explain why via title or describedby text. */
async function disabledReason(page: Page, testid: string): Promise<string> {
	const button = page.getByTestId(testid);
	const title = (await button.getAttribute('title'))?.trim() ?? '';
	if (title) return title;
	const describedby = (await button.getAttribute('aria-describedby')) ?? '';
	for (const id of describedby.split(/\s+/).filter(Boolean)) {
		const text = ((await page.locator(`#${CSS.escape(id)}`).first().textContent()) ?? '').trim();
		if (text) return text;
	}
	return '';
}

async function feedbackText(page: Page): Promise<string> {
	if ((await feedback(page).count()) !== 1) return '<missing>';
	return (await feedback(page).innerText()).trim();
}

/** Success feedback names the action; no-op/refusal feedback never claims one. */
function namesJump(text: string): boolean {
	return /10 second/i.test(text);
}

function claimsJump(text: string): boolean {
	return /^(jumped|seeked|moved to)/i.test(text);
}

async function check(testInfo: TestInfo, expected: unknown, actual: unknown, errorCode = 'LIBRARY_RELATIVE_JUMP_CONTRACT') {
	let passed = false;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} finally {
		const receipt = {
			case_id: testInfo.title.split(' ')[0], expected, actual,
			status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : errorCode,
			next_action: passed ? 'preserve acceptance' : 'implement library relative jump; do not edit protected tests'
		};
		const body = JSON.stringify(receipt);
		console.log(body);
		await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
	}
}

test.afterEach(async ({}, testInfo) => {
	if (testInfo.attachments.some((attachment) => attachment.name === 'acceptance-case.json')) return;
	const body = JSON.stringify({
		case_id: testInfo.title.split(' ')[0], expected: 'reach behavioral assertion',
		actual: testInfo.errors.map((error) => error.message), status: 'ERROR',
		error_code: 'HARNESS_OR_ACTION_FAILURE', next_action: 'repair harness/action; not verified feature RED'
	});
	console.log(body);
	await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

test('LIBJUMP10-CTRL-01 harness: native audio and queue are intact without the feature', async ({ libraryPage: page }, info) => {
	await replace(page);
	await check(info, {
		src: '/__acceptance_audio__/coral.wav', controls: true, identity: true,
		queue: ['lj-coral', 'lj-pine', 'lj-ember'], current: 'lj-coral', next: 1
	}, {
		src: await audio(page).getAttribute('src'),
		controls: await audio(page).evaluate((el: HTMLAudioElement) => el.hasAttribute('controls')),
		identity: await player(page).getByTestId('library-now-playing').count() === 1,
		queue: await queueIds(page),
		current: await currentId(page),
		next: await page.getByTestId('player-next').count()
	}, 'HARNESS_CONTRACT');
});

test('LIBJUMP10-CTRL-02 harness: the media double reads and writes deterministically', async ({ libraryPage: page }, info) => {
	await replace(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	const before = await mediaTime(page);
	await audio(page).evaluate((el: HTMLAudioElement) => { el.currentTime = 42.5; });
	await check(info, { before: 30, after: 42.5 }, { before, after: await mediaTime(page) }, 'HARNESS_CONTRACT');
});

test('LIBJUMP10-01 back/forward buttons are labeled natives in the Player region', async ({ libraryPage: page }, info) => {
	await replace(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	let detail = { backName: false, forwardName: false, native: false, inPlayer: false, distinct: false, focusable: false, controls: false };
	if (backState !== 'missing' && forwardState !== 'missing') {
		const back = page.getByTestId('player-seek-back');
		const forward = page.getByTestId('player-seek-forward');
		detail = {
			backName: /back 10/i.test((await back.getAttribute('aria-label')) ?? (await back.innerText())),
			forwardName: /forward 10/i.test((await forward.getAttribute('aria-label')) ?? (await forward.innerText())),
			native: (await back.evaluate((el) => el.tagName)) === 'BUTTON' && (await forward.evaluate((el) => el.tagName)) === 'BUTTON',
			inPlayer: await player(page).getByTestId('player-seek-back').count() === 1 && await player(page).getByTestId('player-seek-forward').count() === 1,
			distinct: (await page.getByTestId('player-next').getAttribute('aria-label')) !== (await back.getAttribute('aria-label')),
			focusable: await back.evaluate((el: HTMLElement) => { el.focus(); return document.activeElement === el; })
				&& await forward.evaluate((el: HTMLElement) => { el.focus(); return document.activeElement === el; }),
			controls: await audio(page).evaluate((el: HTMLAudioElement) => el.hasAttribute('controls'))
		};
	}
	await check(info, {
		back: 'enabled', forward: 'enabled',
		detail: { backName: true, forwardName: true, native: true, inPlayer: true, distinct: true, focusable: true, controls: true }
	}, { back: backState, forward: forwardState, detail });
});

for (const [index, theme] of ['light', 'dark'].entries()) {
	test(`LIBJUMP10-02${index === 0 ? 'a' : 'b'} ${theme} 320px keeps both buttons visible, disjoint and keyboard-reachable`, async ({ libraryPage: page }, info) => {
		await page.setViewportSize({ width: 320, height: 740 });
		await page.emulateMedia({ colorScheme: theme as 'light' | 'dark' });
		await page.evaluate((mode) => document.documentElement.setAttribute('data-appearance', mode), theme);
		await replace(page, state(0, [TRACKS[0]!]));
		await mockMedia(page, { currentTime: 30, duration: 120 });
		const backState = await hookState(page, 'player-seek-back');
		const forwardState = await hookState(page, 'player-seek-forward');
		let layout = { visible: false, contained: false, disjoint: false, tabReached: false, ring: false };
		if (backState !== 'missing' && forwardState !== 'missing') {
			const back = page.getByTestId('player-seek-back');
			const forward = page.getByTestId('player-seek-forward');
			const backBox = await back.boundingBox();
			const forwardBox = await forward.boundingBox();
			const visible = await back.isVisible() && await forward.isVisible();
			const contained = !!backBox && !!forwardBox
				&& backBox.x >= 0 && backBox.x + backBox.width <= 320
				&& forwardBox.x >= 0 && forwardBox.x + forwardBox.width <= 320;
			const disjoint = !!backBox && !!forwardBox
				&& (backBox.x + backBox.width <= forwardBox.x || forwardBox.x + forwardBox.width <= backBox.x
					|| backBox.y + backBox.height <= forwardBox.y || forwardBox.y + forwardBox.height <= backBox.y);
			await focusBody(page);
			let tabReached = false;
			for (let tab = 0; tab < 150; tab += 1) {
				await page.keyboard.press('Tab');
				const reached = await page.evaluate(() => (document.activeElement as HTMLElement | null)?.dataset?.testid ?? null);
				if (reached === 'player-seek-back' || reached === 'player-seek-forward') { tabReached = true; break; }
			}
			const ring = tabReached && await page.evaluate(() => {
				const style = getComputedStyle(document.activeElement as HTMLElement);
				return style.outlineStyle !== 'none' || style.boxShadow !== 'none';
			});
			layout = { visible, contained, disjoint, tabReached, ring };
		}
		await check(info, {
			back: 'enabled', forward: 'enabled',
			layout: { visible: true, contained: true, disjoint: true, tabReached: true, ring: true }
		}, { back: backState, forward: forwardState, layout });
	});
}

test('LIBJUMP10-03 Back 10 seconds preserves paused intent, volume, rate, source and queue', async ({ libraryPage: page }, info) => {
	await replace(page);
	await mockMedia(page, { currentTime: 30, duration: 120, paused: true });
	await audio(page).evaluate((el: HTMLAudioElement) => { el.volume = 0.35; el.playbackRate = 1.5; });
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const backState = await hookState(page, 'player-seek-back');
	const before = await lifecycleOf(page);
	let after = { time: -1, paused: false, volume: -1, rate: -1, src: '', current: '', queue: [] as (string | null)[], calls: before, feedbackNames: false, identity: false };
	if (backState === 'enabled') {
		await page.getByTestId('player-seek-back').click();
		await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(20);
		after = {
			time: await mediaTime(page),
			paused: await mediaPaused(page),
			volume: await audio(page).evaluate((el: HTMLAudioElement) => el.volume),
			rate: await audio(page).evaluate((el: HTMLAudioElement) => el.playbackRate),
			src: (await audio(page).getAttribute('src')) ?? '',
			current: (await currentId(page)) ?? '',
			queue: await queueIds(page),
			calls: await lifecycleOf(page),
			feedbackNames: namesJump(await feedbackText(page)),
			identity: ((await player(page).getByTestId('library-now-playing').innerText()) ?? '').includes('Coral Night')
		};
	}
	await check(info, {
		back: 'enabled', errors: [],
		after: { time: 20, paused: true, volume: 0.35, rate: 1.5, src: '/__acceptance_audio__/coral.wav', current: 'lj-coral', queue: ['lj-coral', 'lj-pine', 'lj-ember'], calls: before, feedbackNames: true, identity: true }
	}, { back: backState, errors, after });
});

test('LIBJUMP10-04 touch tap activates Forward 10 seconds without extra transport calls', async ({ libraryPage: page }, info) => {
	await replace(page);
	await mockMedia(page, { currentTime: 30, duration: 120, paused: true });
	const forwardState = await hookState(page, 'player-seek-forward');
	const before = await lifecycleOf(page);
	let after = { time: -1, paused: false, current: '', calls: before, feedbackNames: false };
	if (forwardState === 'enabled') {
		await page.getByTestId('player-seek-forward').tap();
		await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(40);
		after = {
			time: await mediaTime(page),
			paused: await mediaPaused(page),
			current: (await currentId(page)) ?? '',
			calls: await lifecycleOf(page),
			feedbackNames: namesJump(await feedbackText(page))
		};
	}
	await check(info, {
		forward: 'enabled',
		after: { time: 40, paused: true, current: 'lj-coral', calls: before, feedbackNames: true }
	}, { forward: forwardState, after });
});

test('LIBJUMP10-05 fractional positions keep their fraction across both directions', async ({ libraryPage: page }, info) => {
	await replace(page);
	await mockMedia(page, { currentTime: 30.5, duration: 120 });
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	const before = await lifecycleOf(page);
	let times = { back: -1, forward: -1 };
	if (backState === 'enabled' && forwardState === 'enabled') {
		await page.getByTestId('player-seek-back').click();
		await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBeCloseTo(20.5, 5);
		times.back = await mediaTime(page);
		await page.getByTestId('player-seek-forward').click();
		await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBeCloseTo(30.5, 5);
		times.forward = await mediaTime(page);
	}
	await check(info, {
		back: 'enabled', forward: 'enabled', calls: before,
		times: { back: 20.5, forward: 30.5 }
	}, {
		back: backState, forward: forwardState, calls: await lifecycleOf(page),
		times: { back: Math.round(times.back * 1e5) / 1e5, forward: Math.round(times.forward * 1e5) / 1e5 }
	});
});

test('LIBJUMP10-06 seeks clamp to zero and duration, then no-op honestly at the bounds', async ({ libraryPage: page }, info) => {
	await replace(page);
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const actual: Record<string, unknown> = { back: backState, forward: forwardState, errors };
	if (backState !== 'missing' && forwardState !== 'missing') {
		// Start bound: 3s back lands on 0.
		await mockMedia(page, { currentTime: 3, duration: 120 });
		if (backState === 'enabled') {
			await page.getByTestId('player-seek-back').click();
			await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
		}
		actual.startClamp = await mediaTime(page);
		// At 0, Back is disabled-with-reason or an enabled no-op with honest feedback.
		const atZero = await hookState(page, 'player-seek-back');
		actual.atZero = atZero;
		if (atZero === 'enabled') {
			const calls = await lifecycleOf(page);
			await page.getByTestId('player-seek-back').click();
			await flushFrame(page);
			actual.zeroNoOp = { time: await mediaTime(page), calls: await lifecycleOf(page), callsBefore: calls, honest: !claimsJump(await feedbackText(page)) && (await feedbackText(page)).length > 0 };
		} else if (atZero === 'disabled') {
			actual.zeroNoOp = { time: await mediaTime(page), reason: (await disabledReason(page, 'player-seek-back')).length > 0 };
		}
		// End bound: 115s forward lands on the 120s duration.
		await mockMedia(page, { currentTime: 115, duration: 120 });
		if (forwardState === 'enabled') {
			await page.getByTestId('player-seek-forward').click();
			await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(120);
		}
		actual.endClamp = await mediaTime(page);
		const atEnd = await hookState(page, 'player-seek-forward');
		actual.atEnd = atEnd;
		if (atEnd === 'enabled') {
			const calls = await lifecycleOf(page);
			await page.getByTestId('player-seek-forward').click();
			await flushFrame(page);
			actual.endNoOp = { time: await mediaTime(page), calls: await lifecycleOf(page), callsBefore: calls, honest: !claimsJump(await feedbackText(page)) && (await feedbackText(page)).length > 0 };
		} else if (atEnd === 'disabled') {
			actual.endNoOp = { time: await mediaTime(page), reason: (await disabledReason(page, 'player-seek-forward')).length > 0 };
		}
	}
	const zeroNoOp = actual.atZero === 'disabled'
		? { time: 0, reason: true }
		: { time: 0, calls: (actual.zeroNoOp as { callsBefore: Calls } | undefined)?.callsBefore, callsBefore: (actual.zeroNoOp as { callsBefore: Calls } | undefined)?.callsBefore, honest: true };
	const endNoOp = actual.atEnd === 'disabled'
		? { time: 120, reason: true }
		: { time: 120, calls: (actual.endNoOp as { callsBefore: Calls } | undefined)?.callsBefore, callsBefore: (actual.endNoOp as { callsBefore: Calls } | undefined)?.callsBefore, honest: true };
	await check(info, {
		back: 'enabled', forward: 'enabled', errors: [],
		startClamp: 0, atZero: actual.atZero, zeroNoOp,
		endClamp: 120, atEnd: actual.atEnd, endNoOp
	}, actual);
});

test('LIBJUMP10-07 Enter and Space activate both buttons under playing intent', async ({ libraryPage: page }, info) => {
	await replace(page);
	await mockMedia(page, { currentTime: 30, duration: 120, paused: false });
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	const before = await lifecycleOf(page);
	let after = { back: -1, forward: -1, paused: true };
	if (backState === 'enabled' && forwardState === 'enabled') {
		await page.getByTestId('player-seek-back').focus();
		await page.keyboard.press('Enter');
		await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(20);
		after.back = await mediaTime(page);
		await page.getByTestId('player-seek-forward').focus();
		await page.keyboard.press('Space');
		await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(30);
		after.forward = await mediaTime(page);
		after.paused = await mediaPaused(page);
	}
	await check(info, {
		back: 'enabled', forward: 'enabled', calls: before,
		after: { back: 20, forward: 30, paused: false }
	}, { back: backState, forward: forwardState, calls: await lifecycleOf(page), after });
});

test('LIBJUMP10-08 unusable duration or time refuses, and known duration seeks', async ({ libraryPage: page }, info) => {
	await replace(page);
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const vectors: { name: string; time: number; duration: number; hook: 'player-seek-back' | 'player-seek-forward' }[] = [
		{ name: 'nan-duration', time: 30, duration: Number.NaN, hook: 'player-seek-forward' },
		{ name: 'infinite-duration', time: 30, duration: Number.POSITIVE_INFINITY, hook: 'player-seek-back' },
		{ name: 'zero-duration', time: 30, duration: 0, hook: 'player-seek-forward' },
		{ name: 'nan-time', time: Number.NaN, duration: 120, hook: 'player-seek-forward' },
		{ name: 'negative-time', time: -5, duration: 120, hook: 'player-seek-back' },
		{ name: 'past-end-time', time: 999, duration: 120, hook: 'player-seek-forward' }
	];
	const refusals: Record<string, unknown> = {};
	if (backState !== 'missing' && forwardState !== 'missing') {
		for (const vector of vectors) {
			await mockMedia(page, { currentTime: vector.time, duration: vector.duration });
			await audio(page).evaluate((el: HTMLAudioElement) => {
				el.dispatchEvent(new Event('loadedmetadata'));
				el.dispatchEvent(new Event('timeupdate'));
			});
			await flushFrame(page);
			const stateNow = await hookState(page, vector.hook);
			if (stateNow === 'enabled') {
				const beforeTime = await mediaTime(page);
				const calls = await lifecycleOf(page);
				await page.getByTestId(vector.hook).click();
				await flushFrame(page);
				const afterTime = await mediaTime(page);
				const unchanged = Number.isNaN(beforeTime) ? Number.isNaN(afterTime) : afterTime === beforeTime;
				refusals[vector.name] = { state: stateNow, unchanged, calls: await lifecycleOf(page), callsBefore: calls, honest: !claimsJump(await feedbackText(page)) && (await feedbackText(page)).length > 0 };
			} else {
				refusals[vector.name] = { state: stateNow, reason: stateNow === 'disabled' ? (await disabledReason(page, vector.hook)).length > 0 : false };
			}
		}
	}
	// Duration becoming known enables real seeking on the same element.
	let known: Record<string, unknown> = {};
	if (forwardState !== 'missing') {
		await mockMedia(page, { currentTime: 30, duration: 120 });
		await audio(page).evaluate((el: HTMLAudioElement) => { el.dispatchEvent(new Event('loadedmetadata')); });
		await flushFrame(page);
		const enabledNow = await hookState(page, 'player-seek-forward');
		let time = -1;
		if (enabledNow === 'enabled') {
			await page.getByTestId('player-seek-forward').click();
			await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(40);
			time = await mediaTime(page);
		}
		known = { state: enabledNow, time };
	}
	const expectedRefusals: Record<string, unknown> = {};
	for (const [name, refusal] of Object.entries(refusals)) {
		const entry = refusal as { state: string; callsBefore?: Calls };
		expectedRefusals[name] = entry.state === 'enabled'
			? { state: 'enabled', unchanged: true, calls: entry.callsBefore, callsBefore: entry.callsBefore, honest: true }
			: entry.state === 'disabled' ? { state: 'disabled', reason: true } : { state: 'missing' };
	}
	await check(info, {
		back: 'enabled', forward: 'enabled', errors: [],
		refusals: expectedRefusals, known: { state: 'enabled', time: 40 }
	}, { back: backState, forward: forwardState, errors, refusals, known });
});

test('LIBJUMP10-09 a rejecting seek setter is caught without throwing through the UI', async ({ libraryPage: page }, info) => {
	await replace(page);
	await audio(page).evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { value: 120, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => 30,
			set: () => { throw new DOMException('seek refused by media', 'InvalidStateError'); },
			configurable: true
		});
	});
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const before = await lifecycleOf(page);
	let after = { time: -1, backFeedback: '', forwardFeedback: '' };
	if (backState === 'enabled' && forwardState === 'enabled') {
		await page.getByTestId('player-seek-back').click();
		await flushFrame(page);
		after.backFeedback = await feedbackText(page);
		await page.getByTestId('player-seek-forward').click();
		await flushFrame(page);
		after.forwardFeedback = await feedbackText(page);
		after.time = await mediaTime(page);
	}
	await check(info, {
		back: 'enabled', forward: 'enabled', errors: [], calls: before,
		after: { time: 30, backExplains: true, forwardExplains: true, current: 'lj-coral' }
	}, {
		back: backState, forward: forwardState, errors, calls: await lifecycleOf(page),
		after: {
			time: after.time,
			backExplains: after.backFeedback.length > 0 && !claimsJump(after.backFeedback),
			forwardExplains: after.forwardFeedback.length > 0 && !claimsJump(after.forwardFeedback),
			current: await currentId(page)
		}
	});
});

test('LIBJUMP10-10 seeks follow the current element across source replacement, stale events ignored', async ({ libraryPage: page }, info) => {
	await replace(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	const before = await lifecycleOf(page);
	let actual: Record<string, unknown> = { back: backState, forward: forwardState };
	if (backState === 'enabled' && forwardState === 'enabled') {
		const stale = (await audio(page).elementHandle()) as ElementHandle<HTMLAudioElement> | null;
		if (stale) {
			await stale.evaluate((el: HTMLAudioElement) => { (el as unknown as { __jumpMarker?: boolean }).__jumpMarker = true; });
			await page.getByTestId('player-next').click();
			await expect(player(page).getByTestId('library-now-playing')).toContainText('Pine Morning');
			const replaced = await audio(page).evaluate((el: HTMLAudioElement) => (el as unknown as { __jumpMarker?: boolean }).__jumpMarker ?? false);
			// The replacement element seeks from ITS position, not the old one.
			await mockMedia(page, { currentTime: 50, duration: 200 });
			await page.getByTestId('player-seek-back').click();
			await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(40);
			const staleTime = await stale.evaluate((el: HTMLAudioElement) => el.currentTime);
			// Stale events from the detached element cannot disable or redirect.
			await stale.evaluate((el: HTMLAudioElement) => {
				Object.defineProperty(el, 'duration', { value: Number.NaN, configurable: true });
				el.dispatchEvent(new Event('loadedmetadata'));
				el.dispatchEvent(new Event('error'));
			});
			await flushFrame(page);
			const stillEnabled = await hookState(page, 'player-seek-forward');
			if (stillEnabled === 'enabled') {
				await page.getByTestId('player-seek-forward').click();
				await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(50);
			}
			actual = {
				back: backState, forward: forwardState, replaced,
				time: await mediaTime(page), staleTime, stillEnabled,
				calls: await lifecycleOf(page), callsBefore: before, current: await currentId(page)
			};
		}
	}
	await check(info, {
		back: 'enabled', forward: 'enabled', replaced: false,
		time: 50, staleTime: 30, stillEnabled: 'enabled',
		calls: before, callsBefore: before, current: 'lj-pine'
	}, actual);
});

test('LIBJUMP10-11 an explicit jump supersedes pending inbound-share seeking', async ({ page, baseURL }, info) => {
	await stubTransport(page);
	await localOnly(page, baseURL!);
	await page.goto('/library?track=lj-pine&t=45');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(player(page).getByTestId('library-now-playing')).toContainText('Pine Morning');
	const backState = await hookState(page, 'player-seek-back');
	let actual: Record<string, unknown> = { back: backState };
	if (backState !== 'missing') {
		// Unknown duration: the share seek applies but stays pending.
		await mockMedia(page, { currentTime: 45, duration: Number.NaN });
		await audio(page).evaluate((el: HTMLAudioElement) => { el.dispatchEvent(new Event('loadedmetadata')); });
		await flushFrame(page);
		const shared = await mediaTime(page);
		// Duration reveals silently (no event), so the pending seek is still
		// armed when the explicit gesture lands.
		await audio(page).evaluate((el: HTMLAudioElement) => {
			Object.defineProperty(el, 'duration', { value: 120, configurable: true });
		});
		const armed = await hookState(page, 'player-seek-back');
		let jumped = -1;
		let afterMetadata = -1;
		if (armed === 'enabled') {
			await page.getByTestId('player-seek-back').click();
			await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(35);
			jumped = await mediaTime(page);
			// Late metadata must not undo the explicit gesture.
			await audio(page).evaluate((el: HTMLAudioElement) => {
				el.dispatchEvent(new Event('durationchange'));
				el.dispatchEvent(new Event('loadedmetadata'));
			});
			await flushFrame(page);
			afterMetadata = await mediaTime(page);
		}
		actual = { back: backState, shared, armed, jumped, afterMetadata, current: await currentId(page) };
	}
	await check(info, {
		back: 'enabled', shared: 45, armed: 'enabled', jumped: 35, afterMetadata: 35, current: 'lj-pine'
	}, actual);
});

test('LIBJUMP10-12 source change and auth teardown invalidate pending share state', async ({ page, baseURL }, info) => {
	await stubTransport(page);
	await localOnly(page, baseURL!);
	await page.goto('/library?track=lj-coral&t=45');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(player(page).getByTestId('library-now-playing')).toContainText('Coral Night');
	const backState = await hookState(page, 'player-seek-back');
	const forwardState = await hookState(page, 'player-seek-forward');
	let actual: Record<string, unknown> = { back: backState, forward: forwardState };
	if (backState !== 'missing' && forwardState !== 'missing') {
		// Phase 1: arm a pending share seek, then walk the source away and
		// back with the real Next transport (bulk-appended rows carry no
		// stream, so the middle stops hide the Player entirely).
		await mockMedia(page, { currentTime: 45, duration: Number.NaN });
		await audio(page).evaluate((el: HTMLAudioElement) => { el.dispatchEvent(new Event('loadedmetadata')); });
		await flushFrame(page);
		await page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true }).click();
		await page.getByTestId('player-next').click();
		await expect(player(page)).toHaveCount(0);
		await page.getByTestId('player-next').click();
		await page.getByTestId('player-next').click();
		await expect(player(page).getByTestId('library-now-playing')).toContainText('Coral Night');
		await mockMedia(page, { currentTime: 5, duration: 120 });
		await audio(page).evaluate((el: HTMLAudioElement) => {
			el.dispatchEvent(new Event('loadedmetadata'));
			el.dispatchEvent(new Event('durationchange'));
		});
		await flushFrame(page);
		const afterSourceChange = await mediaTime(page);
		// Phase 2: a fresh share seek, then the auth boundary clears
		// everything; Retry recovers the next session without replaying it.
		await page.goto('/library?track=lj-ember&t=50');
		await expect(player(page).getByTestId('library-now-playing')).toContainText('Ember Noon');
		await mockMedia(page, { currentTime: 50, duration: Number.NaN });
		await audio(page).evaluate((el: HTMLAudioElement) => { el.dispatchEvent(new Event('loadedmetadata')); });
		await flushFrame(page);
		await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
		await expect(player(page)).toHaveCount(0);
		await expect(page.getByRole('link', { name: /sign in/i })).toBeVisible();
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await page.locator('[data-testid="library-item"][data-id="lj-coral"] button.nt-library-row').click();
		await expect(player(page).getByTestId('library-now-playing')).toContainText('Coral Night');
		await mockMedia(page, { currentTime: 5, duration: 120 });
		await audio(page).evaluate((el: HTMLAudioElement) => {
			el.dispatchEvent(new Event('loadedmetadata'));
			el.dispatchEvent(new Event('durationchange'));
		});
		await flushFrame(page);
		const afterAuth = await mediaTime(page);
		// The player still jumps after teardown + reselect.
		let reselectedJump = -1;
		if ((await hookState(page, 'player-seek-back')) === 'enabled') {
			await page.getByTestId('player-seek-back').click();
			await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
			reselectedJump = await mediaTime(page);
		}
		actual = { back: backState, forward: forwardState, afterSourceChange, afterAuth, reselectedJump };
	}
	await check(info, {
		back: 'enabled', forward: 'enabled', afterSourceChange: 5, afterAuth: 5, reselectedJump: 0
	}, actual);
});

test('LIBJUMP10-GUARD-13 an empty queue hides the Player instead of stranding jump buttons', async ({ libraryPage: page }, info) => {
	await replace(page);
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await check(info, {
		sections: 0, backs: 0, forwards: 0, audio: 0
	}, {
		sections: await player(page).count(),
		backs: await page.getByTestId('player-seek-back').count(),
		forwards: await page.getByTestId('player-seek-forward').count(),
		audio: await page.locator('audio').count()
	}, 'HARNESS_CONTRACT');
});
