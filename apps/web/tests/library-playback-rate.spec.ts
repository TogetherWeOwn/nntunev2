import { expect, test as base, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-playback-rate/mount.js';

/**
 * LIBRATE — library playback-speed acceptance, tests-only handoff.
 *
 * Protected: this spec + tests/fixtures/library-playback-rate/*.
 * Builder-owned implementation: src/routes/library/+page.svelte and, if
 * needed, src/lib/player/*.ts. Existing suites remain unchanged. Continue
 * the handoff branch; open a combined GREEN PR only.
 *
 * The canonical createPlaybackSpeed helper already ships; these cases pin
 * its regressions (H01-H04, expected GREEN) plus the /library wiring
 * (LR01-LR14, assertion-RED until the Builder lands it). RED is always a
 * receipt mismatch on a behavioral assertion, never a missing import, a
 * dead browser or an action timeout: every interaction is gated on the
 * control being present, so a missing control fails the toEqual below it.
 *
 * LOCKED PAGE CONTRACT (new hooks, all inside the Player region that
 * already renders beside the native audio, and ONLY when a source exists):
 * - `library-playback-speed`: a labelled rate select (label, aria-label or
 *   aria-labelledby naming speed/rate). Preset options at least 0.5, 1,
 *   1.5, 2 in ascending order; when the effective rate is a supported
 *   non-preset (restored, native or clamped, e.g. 1.3), its value is that
 *   exact rate with a visible label naming it, never blank or snapped.
 *   Storage holds the last verified actual rate, never an unapplied
 *   request. Device-local preference semantics: storage failures fail
 *   soft in memory, and no media/account IDs are stored.
 * - `library-normal-speed`: an explicit Normal speed action resetting to
 *   1x through the same verified write path.
 * - `library-speed-result`: exactly one polite result (role=status or
 *   aria-live=polite) naming the actual rate after a user action, or
 *   honest unavailable/failure feedback when a request did not apply
 *   (matching /not applied|not set|couldn|failed|rejected|unavailable/i).
 *   It never announces continuously and never names track titles.
 * - A `ratechange` on the current keyed element reconciles selection,
 *   result and storage to the verified readback without writing back
 *   into the element. Detached, pre-swap and post-teardown elements'
 *   events are ignored. Subscriptions die with replacement/unmount.
 * - Changing rate or resetting never calls play, load, seek, source
 *   renewal or queue mutation, and issues no network requests.
 *
 * Setup (cached packages/browser required for offline use):
 *   pnpm --dir apps/web install --offline --frozen-lockfile
 * Run (4173 must be free; CI=1 prevents reuse of another worktree's preview):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-playback-rate.spec.ts --workers=1 --retries=0
 * Regression: same command with tests/player-speed-truth.spec.ts; full
 * suite: pnpm --dir apps/web test; types: pnpm --dir apps/web check.
 * The runner owns preview + ephemeral component server lifetimes. HTTP
 * fixtures are local; media, other API and external requests are blocked.
 * No live backend.
 *
 * Every case emits/attaches compact JSON expected/actual/status/
 * error_code/next_action. Breaks caught: missing/unlabelled selector,
 * snapped non-preset display, unverified persistence, write-back loops,
 * stale detached event adoption, transport/queue side effects, dropped
 * keyboard/focus support, unreadable narrow layouts, chatty or
 * title-leaking announcements.
 */

declare global {
	interface Window {
		__mediaCalls: { play: number; load: number };
	}
}

const routePath = decodeURIComponent(new URL('../src/routes/library/+page.svelte', import.meta.url).pathname);
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const test = base.extend<{ libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/library-playback-rate/', import.meta.url).pathname),
			publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': decodeURIComponent(new URL('./fixtures/library-playback-rate/navigation.ts', import.meta.url).pathname)
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
	}, { scope: 'worker' }]
});

test.setTimeout(20_000);
test.use({ viewport: { width: 1000, height: 740 } });

const SPEED_KEY = 'nntune:playback-speed:v1';
const TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const FAILURE_RE = /not applied|not set|couldn|failed|rejected|unavailable/i;

const TRACKS: Track[] = [
	{ id: 'lr-harbor', title: 'Harbor Light', artist: 'Mira North', streamUrl: '/__acceptance_audio__/harbor.wav' },
	{ id: 'lr-fern', title: 'Fern Hollow', artist: 'South Ensemble', streamUrl: '/__acceptance_audio__/fern.wav' },
	{ id: 'lr-cinder', title: 'Cinder Path', artist: 'West Quartet', streamUrl: '/__acceptance_audio__/cinder.wav' }
];
const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const audio = (page: Page) => page.getByTestId('audio-player');
const speedSelect = (page: Page) => page.getByTestId('library-playback-speed');
const normalButton = (page: Page) => page.getByTestId('library-normal-speed');
const speedResult = (page: Page) => page.getByTestId('library-speed-result');

interface RequestStats { count: number }

async function localOnly(page: Page, origin: string, stats?: RequestStats) {
	const expectedOrigin = new URL(origin).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		if (stats) stats.count += 1;
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== expectedOrigin || request.resourceType() === 'media') return route.abort();
		if (target.pathname === '/api/library' && request.method() === 'GET') {
			return route.fulfill({ status: 200, json: TRACKS.map(({ id, title, artist }) => ({ id, title, artist })) });
		}
		if (/^\/(?:api|v1|__acceptance_audio__)(?:\/|$)/.test(target.pathname)) return route.abort();
		return route.continue();
	});
}

async function openLibrary(
	page: Page,
	origin: string,
	options: { seedSpeed?: string; blockStorage?: boolean } = {},
	stats?: RequestStats
) {
	await localOnly(page, origin, stats);
	await page.addInitScript(
		({ seed, block }: { seed: string | null; block: boolean }) => {
			if (block) {
				Object.defineProperty(window, 'localStorage', {
					configurable: true,
					get() {
						throw new DOMException('Storage access blocked', 'SecurityError');
					}
				});
			} else if (seed !== null) {
				localStorage.setItem('nntune:playback-speed:v1', seed);
			}
			window.__mediaCalls = { play: 0, load: 0 };
			HTMLMediaElement.prototype.play = function () {
				window.__mediaCalls.play += 1;
				return new Promise<void>(() => {});
			};
			HTMLMediaElement.prototype.load = function () {
				window.__mediaCalls.load += 1;
			};
		},
		{ seed: options.seedSpeed ?? null, block: options.blockStorage ?? false }
	);
	await page.goto(origin);
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect.poll(() => page.evaluate(() => Boolean(window.libraryPlaybackRate?.ready))).toBe(true);
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

async function replace(page: Page, next = state()) {
	await page.evaluate((value) => window.libraryPlaybackRate.replace(value), next);
	await settle(page);
}

async function helperScenarios(page: Page, names: string[]) {
	return page.evaluate((wanted: string[]) => wanted.map((name) => window.libraryPlaybackRate.helper(name)), names);
}

async function playbackRate(page: Page) {
	return audio(page).evaluate((el: HTMLAudioElement) => el.playbackRate);
}

async function currentTime(page: Page) {
	return audio(page).evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function storedSpeed(page: Page) {
	return page.evaluate((key: string) => localStorage.getItem(key), SPEED_KEY);
}

async function selectedSpeedValue(page: Page) {
	return speedSelect(page).evaluate((el: HTMLSelectElement) => el.value);
}

async function selectedSpeedText(page: Page) {
	return speedSelect(page).evaluate((el: HTMLSelectElement) => el.selectedOptions[0]?.textContent ?? '');
}

async function speedOptionValues(page: Page) {
	return speedSelect(page).evaluate((el: HTMLSelectElement) => Array.from(el.options).map((option) => option.value));
}

async function queueIds(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((item) => item.getAttribute('data-track-id')));
}

async function mediaCalls(page: Page) {
	return page.evaluate(() => ({ ...window.__mediaCalls }));
}

/** Counts seeking/seeked events on the current element from here on. */
async function armSeekSpy(page: Page) {
	await audio(page).evaluate((el: HTMLAudioElement) => {
		const holder = el as HTMLAudioElement & { __seekEvents?: number };
		holder.__seekEvents = 0;
		const bump = () => {
			holder.__seekEvents = (holder.__seekEvents ?? 0) + 1;
		};
		el.addEventListener('seeking', bump);
		el.addEventListener('seeked', bump);
	});
	return () =>
		audio(page).evaluate((el: HTMLAudioElement & { __seekEvents?: number }) => el.__seekEvents ?? -1);
}

/** Counts subsequent playbackRate assignments on the current element. */
async function countRateWrites(page: Page) {
	await audio(page).evaluate((el: HTMLAudioElement) => {
		const holder = el as HTMLAudioElement & { __rateWrites?: number };
		holder.__rateWrites = 0;
		let backing = el.playbackRate;
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => backing,
			set: (value: number) => {
				holder.__rateWrites = (holder.__rateWrites ?? 0) + 1;
				backing = value;
			}
		});
	});
	return () =>
		audio(page).evaluate((el: HTMLAudioElement & { __rateWrites?: number }) => el.__rateWrites ?? -1);
}

/** Make the current element clamp rate writes to a ceiling (verified readback differs). */
async function clampRateWrites(page: Page, limit: number) {
	await audio(page).evaluate((el: HTMLAudioElement, ceiling: number) => {
		let backing = el.playbackRate;
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => backing,
			set: (value: number) => {
				backing = Math.min(Number(value), ceiling);
			}
		});
	}, limit);
}

/** Make the current element throw on every rate write. */
async function throwOnRateWrite(page: Page) {
	await audio(page).evaluate((el: HTMLAudioElement) => {
		const frozen = el.playbackRate;
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => frozen,
			set: (_value: number) => {
				throw new Error('rate setter unavailable');
			}
		});
	});
}

/** Make the current element report a non-finite rate on every read. */
async function nonFiniteRateRead(page: Page) {
	await audio(page).evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => NaN,
			set: (_value: number) => {
				// The write lands; the readback is non-finite.
			}
		});
	});
}

/** Simulate a native-control rate change on the current element. */
async function nativeRateChange(page: Page, rate: number) {
	await audio(page).evaluate((el: HTMLAudioElement, next: number) => {
		el.playbackRate = next;
		el.dispatchEvent(new Event('ratechange'));
	}, rate);
}

async function check(testInfo: TestInfo, expected: unknown, actual: unknown, helper: boolean) {
	let passed = false;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} finally {
		const receipt = {
			case_id: testInfo.title.split(' ')[0], expected, actual,
			status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : helper ? 'LIBRATE_HELPER_CONTRACT' : 'LIBRATE_RATE_CONTRACT',
			next_action: passed
				? 'preserve acceptance'
				: helper
					? 'canonical helper regressed; do not edit protected tests'
					: 'implement library playback-speed wiring; do not edit protected tests'
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

test('LIBRATE-H01 canonical recall parses valid, corrupt and out-of-range shapes', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await check(info, [
		{
			ok: true,
			before: { current: 1.3, element: 1, stored: '1.3' },
			applied: 1.3, current: 1.3, element: 1.3, stored: '1.3'
		},
		{
			ok: true,
			currents: { valid: 1.5, corrupt: 1, blank: 1, spaces: 1, nan: 1, infinite: 1, high: 2, low: 0.5, missing: 1 }
		}
	], await helperScenarios(page, ['recall-valid', 'recall-shapes']), true);
});

test('LIBRATE-H02 denied storage, missing and throwing elements fail soft', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await check(info, [
		{ ok: true, initial: 1, set: 1.5, applied: 1.5, current: 1.5, element: 1.5 },
		{ ok: true, set: 1.5, applied: 1.5, current: 1.5, stored: '1.5' },
		{ ok: true, current: 1.5, set: 1.5, apply: 1.5, stored: '1.5' }
	], await helperScenarios(page, ['storage-denied', 'no-element', 'audio-throws']), true);
});

test('LIBRATE-H03 clamped, throwing and non-finite media verify before persisting', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await check(info, [
		{ ok: true, requested: 1.5, set: 1.25, current: 1.25, element: 1.25, stored: '1.25' },
		{ ok: true, set: 1, current: 1, element: 1, stored: '1' },
		{ ok: true, set: 1, current: 1, stored: '1' },
		{ ok: true, set: 1, current: 1, stored: '1' }
	], await helperScenarios(page, ['clamp-adopt', 'setter-throws', 'getter-throws', 'getter-nonfinite']), true);
});

test('LIBRATE-H04 native adoption persists verified rates and rejects non-finite input', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await check(info, [
		{
			ok: true, adopted: 1.3, storedAfter: '1.3', kept: 1.3, storedKept: '1.3',
			clamped: 2, current: 2, storedFinal: '2', element: 1
		},
		{ ok: true, set: 1, current: 1, element: 1, stored: '1' }
	], await helperScenarios(page, ['adopt-native', 'set-nonfinite']), true);
});

test('LIBRATE-LR01 speed controls appear labelled beside audio only with a source', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await replace(page);
	const present = await speedSelect(page).count();
	const options = present === 1 ? await speedOptionValues(page) : [];
	const presets = options.filter((value) => ['0.5', '1', '1.5', '2'].includes(value));
	let labelled = false;
	let labelNamesRate = false;
	if (present === 1) {
		const labelText = await speedSelect(page).evaluate((el: HTMLSelectElement) => {
			const labelledBy = el.getAttribute('aria-labelledby');
			const referenced = (labelledBy ?? '')
				.split(/\s+/)
				.map((id) => document.getElementById(id)?.textContent ?? '')
				.join(' ');
			return [
				Array.from(el.labels ?? []).map((label) => label.textContent ?? '').join(' '),
				el.getAttribute('aria-label') ?? '',
				referenced
			].join(' ');
		});
		labelled = labelText.trim() !== '';
		labelNamesRate = /speed|rate/i.test(labelText);
	}
	const withSource = {
		audio: await audio(page).count(),
		select: present,
		selectVisible: present === 1 && await speedSelect(page).isVisible(),
		inPlayer: await player(page).getByTestId('library-playback-speed').count(),
		labelled,
		labelNamesRate,
		hasPresets: ['0.5', '1', '1.5', '2'].every((preset) => options.includes(preset)),
		presetsAscending: presets.join(',') === '0.5,1,1.5,2',
		normal: await normalButton(page).count(),
		normalInPlayer: await player(page).getByTestId('library-normal-speed').count()
	};
	const withoutSource = [];
	for (const value of [state(-1, []), state(0, [{ ...TRACKS[0], streamUrl: '' }])]) {
		await replace(page, value);
		withoutSource.push({
			audio: await audio(page).count(),
			select: await speedSelect(page).count(),
			normal: await normalButton(page).count()
		});
	}
	await check(info, {
		withSource: {
			audio: 1, select: 1, selectVisible: true, inPlayer: 1, labelled: true,
			labelNamesRate: true, hasPresets: true, presetsAscending: true, normal: 1, normalInPlayer: 1
		},
		withoutSource: [{ audio: 0, select: 0, normal: 0 }, { audio: 0, select: 0, normal: 0 }]
	}, { withSource, withoutSource }, false);
});

test('LIBRATE-LR02 a recalled 1.3 applies to the keyed element with a truthful selection', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin, { seedSpeed: '1.3' });
	await replace(page);
	const present = await speedSelect(page).count();
	let value = '';
	let text = '';
	let rate = -1;
	let stored: string | null = null;
	if (present === 1) {
		value = await selectedSpeedValue(page);
		text = await selectedSpeedText(page);
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.3);
		rate = await playbackRate(page);
		stored = await storedSpeed(page);
	}
	await check(info,
		{ present: 1, value: '1.3', namesRate: true, rate: 1.3, stored: '1.3' },
		{ present, value, namesRate: text.includes('1.3'), rate, stored }, false);
});

test('LIBRATE-LR03 corrupt storage falls back to 1x and denied storage works in memory', async ({ page, context, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin, { seedSpeed: 'garbage' });
	await replace(page);
	const corruptPresent = await speedSelect(page).count();
	let corruptValue = '';
	let corruptRate = -1;
	if (corruptPresent === 1) {
		corruptValue = await selectedSpeedValue(page);
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1);
		corruptRate = await playbackRate(page);
	}
	const denied = await context.newPage();
	const pageerrors: Error[] = [];
	denied.on('pageerror', (error) => pageerrors.push(error));
	await openLibrary(denied, libraryOrigin, { blockStorage: true });
	await replace(denied);
	const deniedPresent = await speedSelect(denied).count();
	let deniedRate = -1;
	if (deniedPresent === 1) {
		await speedSelect(denied).selectOption('1.5');
		await expect.poll(() => playbackRate(denied), { timeout: 5000 }).toBe(1.5);
		deniedRate = await playbackRate(denied);
	}
	await denied.close();
	await check(info,
		{
			corrupt: { present: 1, value: '1', rate: 1 },
			denied: { present: 1, rate: 1.5, pageerrors: 0 }
		},
		{
			corrupt: { present: corruptPresent, value: corruptValue, rate: corruptRate },
			denied: { present: deniedPresent, rate: deniedRate, pageerrors: pageerrors.length }
		}, false);
});

test('LIBRATE-LR04 a chosen preset applies and survives reload', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await replace(page);
	const present = await speedSelect(page).count();
	let storedBefore: string | null = null;
	let value = '';
	let rate = -1;
	let stored: string | null = null;
	if (present === 1) {
		await speedSelect(page).selectOption('1.5');
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.5);
		storedBefore = await storedSpeed(page);
		await page.reload();
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryPlaybackRate?.ready)), { timeout: 5000 }).toBe(true);
		await replace(page);
		value = await selectedSpeedValue(page);
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.5);
		rate = await playbackRate(page);
		stored = await storedSpeed(page);
	}
	await check(info,
		{ present: 1, storedBefore: '1.5', value: '1.5', rate: 1.5, stored: '1.5' },
		{ present, storedBefore, value, rate, stored }, false);
});

test('LIBRATE-LR05 a keyed track change applies the recalled rate to the new element', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin, { seedSpeed: '1.3' });
	await replace(page);
	const present = await speedSelect(page).count();
	let before = -1;
	let after = -1;
	let value = '';
	let stored: string | null = null;
	let queued = -1;
	if (present === 1) {
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.3);
		before = await playbackRate(page);
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Fern Hollow');
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.3);
		after = await playbackRate(page);
		value = await selectedSpeedValue(page);
		stored = await storedSpeed(page);
		queued = await page.getByTestId('queue-item').count();
	}
	await check(info,
		{ present: 1, before: 1.3, after: 1.3, value: '1.3', stored: '1.3', queued: 3 },
		{ present, before, after, value, stored, queued }, false);
});

test('LIBRATE-LR06 native ratechange reconciles the verified rate without writing back', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await replace(page);
	const present = await speedSelect(page).count();
	let writes = -1;
	let value = '';
	let text = '';
	let stored: string | null = null;
	let rate = -1;
	let calls = { play: -1, load: -1 };
	let time = -1;
	if (present === 1) {
		await audio(page).evaluate((el: HTMLAudioElement) => void (el.playbackRate = 1.3));
		const readWrites = await countRateWrites(page);
		await audio(page).evaluate((el: HTMLAudioElement) => void el.dispatchEvent(new Event('ratechange')));
		await page.waitForTimeout(300);
		writes = await readWrites();
		value = await selectedSpeedValue(page);
		text = await selectedSpeedText(page);
		stored = await storedSpeed(page);
		rate = await playbackRate(page);
		calls = await mediaCalls(page);
		time = await currentTime(page);
	}
	await check(info,
		{ present: 1, writes: 0, value: '1.3', namesRate: true, stored: '1.3', rate: 1.3, play: 0, load: 0, time: 0 },
		{ present, writes, value, namesRate: text.includes('1.3'), stored, rate, play: calls.play, load: calls.load, time }, false);
});

test('LIBRATE-LR07 a clamped media request surfaces the actual rate everywhere', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await replace(page);
	const present = await speedSelect(page).count();
	let rate = -1;
	let value = '';
	let text = '';
	let stored: string | null = null;
	if (present === 1) {
		await clampRateWrites(page, 1.25);
		await speedSelect(page).selectOption('1.5');
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.25);
		rate = await playbackRate(page);
		value = await selectedSpeedValue(page);
		text = await selectedSpeedText(page);
		stored = await storedSpeed(page);
	}
	await check(info,
		{ present: 1, rate: 1.25, value: '1.25', namesRate: true, stored: '1.25' },
		{ present, rate, value, namesRate: text.includes('1.25'), stored }, false);
});

test('LIBRATE-LR08 a throwing media setter keeps the last accepted rate with honest feedback', async ({ page, libraryOrigin }, info) => {
	const pageerrors: Error[] = [];
	page.on('pageerror', (error) => pageerrors.push(error));
	await openLibrary(page, libraryOrigin);
	await replace(page);
	const present = await speedSelect(page).count();
	let rate = -1;
	let value = '';
	let storedIs15 = true;
	let failureNamed = false;
	if (present === 1) {
		await throwOnRateWrite(page);
		await speedSelect(page).selectOption('1.5');
		await page.waitForTimeout(200);
		rate = await playbackRate(page);
		value = await selectedSpeedValue(page);
		storedIs15 = (await storedSpeed(page)) === '1.5';
		const resultCount = await speedResult(page).count();
		failureNamed = resultCount === 1 && FAILURE_RE.test((await speedResult(page).textContent()) ?? '');
	}
	await check(info,
		{ present: 1, rate: 1, value: '1', storedIs15: false, failureNamed: true, pageerrors: 0 },
		{ present, rate, value, storedIs15, failureNamed, pageerrors: pageerrors.length }, false);
});

test('LIBRATE-LR09 a non-finite native reading keeps the last accepted rate and storage', async ({ page, libraryOrigin }, info) => {
	const pageerrors: Error[] = [];
	page.on('pageerror', (error) => pageerrors.push(error));
	await openLibrary(page, libraryOrigin, { seedSpeed: '1' });
	await replace(page);
	const present = await speedSelect(page).count();
	let value = '';
	let stored: string | null = null;
	if (present === 1) {
		await nonFiniteRateRead(page);
		await speedSelect(page).selectOption('1.5');
		await page.waitForTimeout(200);
		value = await selectedSpeedValue(page);
		stored = await storedSpeed(page);
	}
	await check(info,
		{ present: 1, value: '1', stored: '1', pageerrors: 0 },
		{ present, value, stored, pageerrors: pageerrors.length }, false);
});

test('LIBRATE-LR10 the Normal action resets to 1x with no transport or queue effects', async ({ page, libraryOrigin }, info) => {
	const stats: RequestStats = { count: 0 };
	await openLibrary(page, libraryOrigin, { seedSpeed: '1.5' }, stats);
	await replace(page);
	const present = await normalButton(page).count();
	const actual = {
		present, rate: -1, value: '', stored: null as string | null, namesNormal: false,
		playDelta: -1, loadDelta: -1, seekEvents: -1, timeSame: false,
		srcSame: false, queueSame: false, requestsDelta: -1
	};
	if (present === 1) {
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.5);
		const readSeeks = await armSeekSpy(page);
		const timeBefore = await currentTime(page);
		const srcBefore = await audio(page).getAttribute('src');
		const queueBefore = await queueIds(page);
		const callsBefore = await mediaCalls(page);
		await settle(page);
		const requestsBefore = stats.count;
		await normalButton(page).click();
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1);
		actual.rate = await playbackRate(page);
		actual.value = await selectedSpeedValue(page);
		actual.stored = await storedSpeed(page);
		const resultCount = await speedResult(page).count();
		actual.namesNormal = resultCount === 1 && /1x|normal/i.test((await speedResult(page).textContent()) ?? '');
		const callsAfter = await mediaCalls(page);
		actual.playDelta = callsAfter.play - callsBefore.play;
		actual.loadDelta = callsAfter.load - callsBefore.load;
		actual.seekEvents = await readSeeks();
		actual.timeSame = (await currentTime(page)) === timeBefore;
		actual.srcSame = (await audio(page).getAttribute('src')) === srcBefore;
		actual.queueSame = JSON.stringify(await queueIds(page)) === JSON.stringify(queueBefore);
		actual.requestsDelta = stats.count - requestsBefore;
	}
	await check(info,
		{
			present: 1, rate: 1, value: '1', stored: '1', namesNormal: true,
			playDelta: 0, loadDelta: 0, seekEvents: 0, timeSame: true,
			srcSame: true, queueSame: true, requestsDelta: 0
		}, actual, false);
});

test('LIBRATE-LR11 changing rate never plays, loads, seeks, renews or mutates the queue', async ({ page, libraryOrigin }, info) => {
	const stats: RequestStats = { count: 0 };
	await openLibrary(page, libraryOrigin, {}, stats);
	await replace(page);
	const present = await speedSelect(page).count();
	const actual = {
		present, rate: -1, value: '', stored: null as string | null,
		playDelta: -1, loadDelta: -1, seekEvents: -1, timeSame: false,
		srcSame: false, queueSame: false, requestsDelta: -1
	};
	if (present === 1) {
		const readSeeks = await armSeekSpy(page);
		const timeBefore = await currentTime(page);
		const srcBefore = await audio(page).getAttribute('src');
		const queueBefore = await queueIds(page);
		const callsBefore = await mediaCalls(page);
		await settle(page);
		const requestsBefore = stats.count;
		await speedSelect(page).selectOption('1.5');
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.5);
		actual.rate = await playbackRate(page);
		actual.value = await selectedSpeedValue(page);
		actual.stored = await storedSpeed(page);
		const callsAfter = await mediaCalls(page);
		actual.playDelta = callsAfter.play - callsBefore.play;
		actual.loadDelta = callsAfter.load - callsBefore.load;
		actual.seekEvents = await readSeeks();
		actual.timeSame = (await currentTime(page)) === timeBefore;
		actual.srcSame = (await audio(page).getAttribute('src')) === srcBefore;
		actual.queueSame = JSON.stringify(await queueIds(page)) === JSON.stringify(queueBefore);
		actual.requestsDelta = stats.count - requestsBefore;
	}
	await check(info,
		{
			present: 1, rate: 1.5, value: '1.5', stored: '1.5',
			playDelta: 0, loadDelta: 0, seekEvents: 0, timeSame: true,
			srcSame: true, queueSame: true, requestsDelta: 0
		}, actual, false);
});

test('LIBRATE-LR12 detached and stale ratechange events never move the selection', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin, { seedSpeed: '1.5' });
	await replace(page);
	const present = await speedSelect(page).count();
	const actual = {
		present,
		afterGhost: { value: '', stored: null as string | null, rate: -1 },
		afterStale: { value: '', stored: null as string | null, rate: -1 }
	};
	if (present === 1) {
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.5);
		await page.evaluate(() => {
			const ghost = document.createElement('audio');
			ghost.playbackRate = 1.75;
			ghost.dispatchEvent(new Event('ratechange'));
		});
		await page.waitForTimeout(200);
		actual.afterGhost = {
			value: await selectedSpeedValue(page),
			stored: await storedSpeed(page),
			rate: await playbackRate(page)
		};
		const oldAudio = await audio(page).elementHandle();
		if (!oldAudio) throw new Error('harness: audio handle missing');
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Fern Hollow');
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.5);
		await oldAudio.evaluate((node) => {
			const el = node as HTMLAudioElement;
			el.playbackRate = 1.75;
			el.dispatchEvent(new Event('ratechange'));
		});
		await page.waitForTimeout(200);
		actual.afterStale = {
			value: await selectedSpeedValue(page),
			stored: await storedSpeed(page),
			rate: await playbackRate(page)
		};
	}
	await check(info,
		{
			present: 1,
			afterGhost: { value: '1.5', stored: '1.5', rate: 1.5 },
			afterStale: { value: '1.5', stored: '1.5', rate: 1.5 }
		}, actual, false);
});

test('LIBRATE-LR13 auth teardown unmounts speed UI and ignores the detached element', async ({ page, libraryOrigin }, info) => {
	const pageerrors: Error[] = [];
	page.on('pageerror', (error) => pageerrors.push(error));
	await openLibrary(page, libraryOrigin, { seedSpeed: '1.5' });
	await replace(page);
	const presentBefore = await speedSelect(page).count();
	const actual = {
		presentBefore,
		after: { audio: -1, select: -1, normal: -1, stored: null as string | null, errors: -1 }
	};
	if (presentBefore === 1) {
		const detached = await audio(page).elementHandle();
		if (!detached) throw new Error('harness: audio handle missing');
		await page.evaluate((key: string) => {
			window.dispatchEvent(new StorageEvent('storage', { key }));
		}, TRANSITION_KEY);
		await expect.poll(() => audio(page).count(), { timeout: 5000 }).toBe(0);
		actual.after.audio = await audio(page).count();
		actual.after.select = await speedSelect(page).count();
		actual.after.normal = await normalButton(page).count();
		await detached.evaluate((node) => {
			const el = node as HTMLAudioElement;
			el.playbackRate = 1.75;
			el.dispatchEvent(new Event('ratechange'));
		});
		await page.waitForTimeout(200);
		actual.after.stored = await storedSpeed(page);
		actual.after.errors = pageerrors.length;
	}
	await check(info,
		{ presentBefore: 1, after: { audio: 0, select: 0, normal: 0, stored: '1.5', errors: 0 } },
		actual, false);
});

test('LIBRATE-LR14 keyboard, focus, narrow themes and one polite rate result', async ({ page, libraryOrigin }, info) => {
	await openLibrary(page, libraryOrigin);
	await replace(page);
	const present = await speedSelect(page).count();
	const actual = {
		present, focused: '', valueAfter: '', rate: -1, focusVisible: false,
		resultCount: -1, polite: false, namesRate: false, resultHasTitle: true,
		layout: { light: { contained: false, readable: false }, dark: { contained: false, readable: false } }
	};
	if (present === 1) {
		await speedSelect(page).focus();
		actual.focused = await page.evaluate(() => document.activeElement?.getAttribute('data-testid') ?? '');
		actual.focusVisible = await speedSelect(page).evaluate((el) => {
			const style = getComputedStyle(el);
			return style.outlineStyle !== 'none' || style.boxShadow !== 'none';
		});
		await speedSelect(page).press('ArrowDown');
		await expect.poll(() => playbackRate(page), { timeout: 5000 }).toBe(1.5);
		actual.valueAfter = await selectedSpeedValue(page);
		actual.rate = await playbackRate(page);
		actual.resultCount = await player(page).getByTestId('library-speed-result').count();
		if (actual.resultCount === 1) {
			const result = speedResult(page);
			const role = await result.getAttribute('role');
			const live = await result.getAttribute('aria-live');
			actual.polite = role === 'status' || live === 'polite';
			const text = (await result.textContent()) ?? '';
			actual.namesRate = text.includes('1.5');
			actual.resultHasTitle = text.includes('Harbor Light');
		}
		for (const theme of ['light', 'dark'] as const) {
			await page.setViewportSize({ width: 360, height: 740 });
			await page.emulateMedia({ colorScheme: theme });
			await page.evaluate((name) => document.documentElement.setAttribute('data-appearance', name), theme);
			await settle(page);
			actual.layout[theme] = await speedResult(page).evaluate((node) => {
				const controls = [
					node.getBoundingClientRect(),
					...(node.parentElement?.querySelectorAll('[data-testid="library-playback-speed"], [data-testid="library-normal-speed"]') ?? [])
				].map((item) => (item instanceof Element ? item.getBoundingClientRect() : item));
				const rgb = (value: string) => value.match(/[\d.]+/g)?.map(Number) ?? [];
				const luminance = (color: number[]) => color.slice(0, 3).map((n) => {
					const c = n / 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
				}).reduce((sum, value, i) => sum + value * [0.2126, 0.7152, 0.0722][i], 0);
				let background: number[] = [];
				for (let element: Element | null = node; element; element = element.parentElement) {
					const value = rgb(getComputedStyle(element).backgroundColor);
					if (value.length === 3 || value[3] === 1) { background = value; break; }
				}
				const foreground = rgb(getComputedStyle(node).color);
				const values = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
				return {
					contained: controls.every((box) => box.x >= 0 && box.right <= innerWidth + 1) &&
						document.documentElement.scrollWidth <= innerWidth,
					readable: background.length >= 3 && (values[0] + 0.05) / (values[1] + 0.05) >= 4.5
				};
			});
		}
	}
	await check(info,
		{
			present: 1, focused: 'library-playback-speed', valueAfter: '1.5', rate: 1.5, focusVisible: true,
			resultCount: 1, polite: true, namesRate: true, resultHasTitle: false,
			layout: { light: { contained: true, readable: true }, dark: { contained: true, readable: true } }
		}, actual, false);
});
