import { expect, test as base, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-timeline/mount.js';

/**
 * LIBTIME — seek the current library native audio with a truthful accessible
 * timeline, tests-only handoff.
 *
 * Protected: this spec + tests/fixtures/library-timeline/*.
 * Builder-owned implementation: src/routes/library/+page.svelte and, if needed,
 * small src/lib/player/* helpers. Existing suites remain unchanged. Continue the
 * handoff branch; open a combined GREEN PR only.
 *
 * Contract the build implements (stable case IDs LIBTIME-00..LIBTIME-21):
 *
 * Hooks (literal `data-testid`, inside the Player region that already holds
 * the keyed native audio):
 *   library-timeline ...... labelled keyboard-operable seek control: an
 *       <input type="range"> or role="slider" whose accessible name matches
 *       /seek/i, focusable, arrow-key operable with a visible focus indicator.
 *   library-time-readout ... elapsed/total text. Elapsed mode reads like
 *       "Elapsed 1:05 of 3:05"; unknown duration reads a truthful
 *       "Duration unavailable" fragment (case-insensitive), never NaN,
 *       Infinity, undefined, an invented duration or a fake percentage.
 *   library-time-mode ...... elapsed/remaining toggle: a <select> with
 *       elapsed/remaining options, or a button/switch that flips the mode.
 *   library-seek-back ...... native <button>, accessible name /back 10/i.
 *   library-seek-forward ... native <button>, accessible name /forward 10/i.
 *   library-seek-status .... role="status"/"alert" failure text, required only
 *       after a failed seek assignment; non-sensitive, never a success claim.
 * All five hooks are absent (with the whole Player section) when no current
 * resolved stream source exists. Native audio[controls] and inbound share
 * timestamp semantics stay intact.
 *
 * Behavior: every activation reads the STILL-CURRENT matching audio element,
 * moves by exactly 10 seconds (back/forward), clamps to finite duration and
 * zero, and intersects the element's actual seekable ranges. Seeks preserve
 * paused/playing intent and never call play/pause/load, fetch/renew a source,
 * or mutate queue/storage. Unknown/Infinity/unseekable duration disables
 * seeking with truthful copy. Time/metadata/seek events are fenced by current
 * element, source/track identity and auth generation; keyed replacement, empty
 * queue, account transition and teardown reset the readout/controls, and
 * detached-source events are ignored. Visual time updates coalesce bursts and
 * never announce per-tick.
 *
 * Determinism: mount the REAL library page over a fixture Vite server (same
 * pre-transform seam as the native-identity suite: one exported setter calling
 * the page's existing replaceQueue), feed it playable queue states through the
 * real queue reducers, and double duration/currentTime/paused/seekable per
 * element with Object.defineProperty — no real decoder, no sleeps, no network
 * media. HTTP fixtures are local; media, other API and external requests are
 * blocked and every activation asserts zero fetch attempts.
 *
 * Setup (cached packages/browser required for offline use):
 *   pnpm --dir apps/web install --offline --frozen-lockfile
 * Run (CI=1 prevents reuse of another worktree's preview):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-timeline.spec.ts --workers=1 --retries=0
 * Regression: same command with tests/library-share-link.spec.ts,
 * tests/library-share-link-lifecycle.spec.ts,
 * tests/library-native-now-playing.spec.ts, tests/library-player-queue.spec.ts
 * and tests/library-player-queue-pins.spec.ts; plus from apps/web
 * `pnpm check` and `pnpm test`.
 *
 * Every case emits/attaches compact JSON expected/actual/status/error_code/
 * next_action. Missing behavior is assertion-RED naming the hook, never a
 * click timeout/import failure. LIBTIME-00/01/15/16 are guard controls that
 * stay GREEN with and without the feature; they prove the harness, not the
 * feature. All other cases must be RED on base. Never fabricate guard RED.
 */

const routePath = decodeURIComponent(new URL('../src/routes/library/+page.svelte', import.meta.url).pathname);
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const test = base.extend<{ libraryPage: Page }, { libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/library-timeline/', import.meta.url).pathname),
			publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': decodeURIComponent(new URL('./fixtures/library-timeline/navigation.ts', import.meta.url).pathname)
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
		await page.addInitScript(() => {
			const control = { playCalls: 0, pauseCalls: 0, loadCalls: 0 };
			Object.assign(window, { __libtimeLifecycle: control });
			HTMLMediaElement.prototype.play = function (): Promise<void> {
				control.playCalls += 1;
				return new Promise<void>(() => {});
			};
			HTMLMediaElement.prototype.pause = function () {
				control.pauseCalls += 1;
			};
			const rawLoad = HTMLMediaElement.prototype.load;
			HTMLMediaElement.prototype.load = function () {
				control.loadCalls += 1;
				return rawLoad.call(this);
			};
		});
		await localOnly(page, libraryOrigin);
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryTimeline))).toBe(true);
		await use(page);
	}
});

test.setTimeout(20_000);
test.use({ viewport: { width: 1000, height: 740 } });

const TRACKS: Track[] = [
	{ id: 'lt-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '/__acceptance_audio__/coral.wav' },
	{ id: 'lt-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '/__acceptance_audio__/pine.wav' },
	{ id: 'lt-ember', title: 'Ember Noon', artist: 'West Quartet', streamUrl: '/__acceptance_audio__/ember.wav' }
];
const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const AUTH_KEY = 'nntune.library.auth-transition.v1';
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const audio = (page: Page) => player(page).locator('audio');

async function localOnly(page: Page, origin: string) {
	const expectedOrigin = new URL(origin).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
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

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

async function replace(page: Page, next = state()) {
	await page.evaluate((value) => window.libraryTimeline.replace(value), next);
	await settle(page);
}

/** Collect backend/media fetch attempts made after this call. */
function watchFetches(page: Page): string[] {
	const seen: string[] = [];
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (request.resourceType() === 'media' || /^\/(?:api|v1|__acceptance_audio__)(?:\/|$)/.test(url.pathname)) {
			seen.push(request.url());
		}
	});
	return seen;
}

type Lifecycle = { playCalls: number; pauseCalls: number; loadCalls: number };
async function lifecycleOf(page: Page): Promise<Lifecycle> {
	return page.evaluate(() => ({ ...(window as unknown as { __libtimeLifecycle: Lifecycle }).__libtimeLifecycle }));
}

/**
 * Deterministic media double on the CURRENT library audio element: finite or
 * sentinel duration, a writable currentTime, a paused flag and explicit
 * seekable ranges. Re-apply after any keyed replacement recreates the element.
 */
async function mockMedia(page: Page, init: {
	currentTime: number; duration: number; paused?: boolean; seekable?: Array<[number, number]>;
}) {
	await audio(page).evaluate((el: HTMLAudioElement, seed: typeof init) => {
		let time = seed.currentTime;
		const ranges = seed.seekable ?? (Number.isFinite(seed.duration) && seed.duration >= 0
			? [[0, seed.duration] as [number, number]] : []);
		Object.defineProperty(el, 'duration', { value: seed.duration, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => time,
			set: (next: number) => { time = next; },
			configurable: true
		});
		Object.defineProperty(el, 'paused', { value: seed.paused ?? true, configurable: true });
		Object.defineProperty(el, 'seekable', {
			value: { length: ranges.length, start: (i: number) => ranges[i]![0], end: (i: number) => ranges[i]![1] },
			configurable: true
		});
	}, init);
}

/** Resolve seekable bounds back out of the live element for range assertions. */
async function seekableOf(page: Page): Promise<Array<[number, number]>> {
	return audio(page).evaluate((el: HTMLAudioElement) => {
		const out: Array<[number, number]> = [];
		for (let i = 0; i < el.seekable.length; i += 1) out.push([el.seekable.start(i), el.seekable.end(i)]);
		return out;
	});
}

async function mediaTime(page: Page): Promise<number> {
	return audio(page).evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function fireAudio(page: Page, type: string) {
	await audio(page).evaluate((el, name) => {
		el.dispatchEvent(new Event(name));
	}, type);
}

/** Seed coordinates then deliver the metadata/time events a real load produces. */
async function loadMedia(page: Page, init: { currentTime: number; duration: number; paused?: boolean; seekable?: Array<[number, number]> }) {
	await mockMedia(page, init);
	for (const type of ['loadedmetadata', 'durationchange', 'timeupdate']) await fireAudio(page, type);
	await settle(page);
}

async function hookState(page: Page, testid: string): Promise<'missing' | 'disabled' | 'enabled'> {
	const hook = page.getByTestId(testid);
	if ((await hook.count()) === 0) return 'missing';
	if (await hook.evaluate((el) => {
		const disabled = (el as HTMLButtonElement | HTMLInputElement | HTMLSelectElement).disabled === true
			|| el.getAttribute('aria-disabled') === 'true';
		return disabled;
	})) return 'disabled';
	return 'enabled';
}

async function requireHook(page: Page, info: TestInfo, caseId: string, testid: string, label: string) {
	const found = await hookState(page, testid);
	if (found === 'missing') {
		// Emit the RED receipt before the failing assertion so a missing hook
		// reads as verified feature RED, never as a harness/action ERROR.
		// The receipt helper throws on mismatch; swallow it so the hook
		// assertion below (with its next action) is the recorded failure.
		await check(info, { hook: `${testid} present in Player` }, { hook: found }).catch(() => {});
	}
	expect(
		found,
		`${caseId}: expected the ${label} hook (data-testid="${testid}") in the Player region, found ${found}. ` +
			`Next action: render the seek timeline bundle on the library native-audio section.`
	).not.toBe('missing');
	return found;
}

async function readoutText(page: Page): Promise<string> {
	const readout = page.getByTestId('library-time-readout');
	if ((await readout.count()) !== 1) return '';
	return (await readout.innerText()).replace(/\s+/g, ' ').trim();
}

async function sliderValue(page: Page): Promise<number | null> {
	const slider = page.getByTestId('library-timeline');
	const raw = await slider.evaluate((el) => {
		if (el instanceof HTMLInputElement) return el.value;
		return el.getAttribute('aria-valuenow');
	});
	if (raw === null || raw === '') return null;
	const parsed = Number(raw);
	return Number.isFinite(parsed) ? parsed : null;
}

/** Resolve the accessible name via aria-label, aria-labelledby or a label element. */
async function accessibleName(page: Page, testid: string): Promise<string> {
	return page.getByTestId(testid).evaluate((el) => {
		const labelled = el.getAttribute('aria-label');
		if (labelled) return labelled;
		const ids = (el.getAttribute('aria-labelledby') ?? '').split(/\s+/).filter(Boolean);
		if (ids.length > 0) {
			return ids.map((id) => document.getElementById(id)?.textContent ?? '').join(' ');
		}
		const labels = (el as HTMLInputElement).labels;
		if (labels && labels.length > 0) return [...labels].map((label) => label.textContent ?? '').join(' ');
		const wrap = el.closest('label');
		if (wrap) return wrap.textContent ?? '';
		return el.textContent ?? '';
	});
}

/** Flip the elapsed/remaining toggle once, whether it is a select or a button. */
async function setTimeMode(page: Page, info: TestInfo, caseId: string, want: 'elapsed' | 'remaining') {
	const control = page.getByTestId('library-time-mode');
	const tag = await control.evaluate((el) => el.tagName.toLowerCase());
	if (tag === 'select') {
		const values = await control.evaluate((el: HTMLSelectElement) => [...el.options].map((option) => option.value));
		if (!values.includes(want)) {
			await check(info, { options: `contain '${want}'` }, { options: values }).catch(() => {});
		}
		expect(
			values,
			`${caseId}: the time-mode select pins elapsed/remaining option values, found [${values.join(', ')}]. ` +
				`Next action: name the options 'elapsed' and 'remaining'.`
		).toEqual(expect.arrayContaining([want]));
		await control.selectOption(want);
	} else {
		for (let attempt = 0; attempt < 2; attempt += 1) {
			if (new RegExp(want, 'i').test(await readoutText(page))) break;
			await control.click();
			await settle(page);
		}
	}
	await settle(page);
}

/** Refuse to click a disabled hook: a wrong disabled state must fail as an assertion, never a click timeout. */
async function requireEnabled(page: Page, info: TestInfo, caseId: string, testid: string, label: string) {
	const found = await requireHook(page, info, caseId, testid, label);
	if (found !== 'enabled') {
		await check(info, { hook: `${testid} enabled with finite seekable media` }, { hook: found }).catch(() => {});
	}
	expect(
		found,
		`${caseId}: expected the ${label} hook (data-testid="${testid}") to be enabled with finite seekable media, found ${found}. ` +
			`Next action: enable seeking only from the live element's finite duration and seekable ranges.`
	).toBe('enabled');
}

function inRanges(time: number, ranges: Array<[number, number]>): boolean {
	return ranges.some(([start, end]) => time >= start - 1e-6 && time <= end + 1e-6);
}

async function check(testInfo: TestInfo, expected: unknown, actual: unknown) {
	let passed = false;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} finally {
		const receipt = {
			case_id: testInfo.title.split(' ')[0], expected, actual,
			status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : 'LIBRARY_TIMELINE_CONTRACT',
			next_action: passed ? 'preserve acceptance' : 'implement the library seek timeline; do not edit protected tests'
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

test('LIBTIME-00 harness double reads and writes the library audio deterministically', async ({ libraryPage: page }, info) => {
	await replace(page);
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/coral.wav');
	expect(await audio(page).evaluate((el: HTMLAudioElement) => el.hasAttribute('controls'))).toBe(true);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await expect.poll(() => mediaTime(page)).toBe(30);
	await audio(page).evaluate((el: HTMLAudioElement) => { el.currentTime = 42.5; });
	await check(info, {
		src: '/__acceptance_audio__/coral.wav', controls: true, read: 30, write: 42.5,
		ranges: [[0, 120]], lifecycle: { playCalls: 0, pauseCalls: 0, loadCalls: 0 }
	}, {
		src: await audio(page).getAttribute('src'),
		controls: await audio(page).evaluate((el: HTMLAudioElement) => el.hasAttribute('controls')),
		read: 30, write: await mediaTime(page),
		ranges: await seekableOf(page), lifecycle: await lifecycleOf(page)
	});
});

test('LIBTIME-01 no custom transport without a current resolved stream source', async ({ libraryPage: page }, info) => {
	const actual = [];
	for (const value of [state(-1, []), state(99), state(0, [{ ...TRACKS[0], streamUrl: '' }])]) {
		await replace(page, value);
		await settle(page);
		actual.push({
			sections: await player(page).count(),
			timeline: await page.getByTestId('library-timeline').count(),
			readout: await page.getByTestId('library-time-readout').count(),
			mode: await page.getByTestId('library-time-mode').count(),
			back: await page.getByTestId('library-seek-back').count(),
			forward: await page.getByTestId('library-seek-forward').count()
		});
	}
	await check(info, Array(3).fill({ sections: 0, timeline: 0, readout: 0, mode: 0, back: 0, forward: 0 }), actual);
});

test('LIBTIME-02 seek timeline bundle renders beside native controls in the Player', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 5, duration: 120 });
	for (const [testid, label] of [
		['library-timeline', 'seek slider'], ['library-time-readout', 'time readout'],
		['library-time-mode', 'elapsed/remaining toggle'], ['library-seek-back', 'Back 10 seconds'],
		['library-seek-forward', 'Forward 10 seconds']
	] as const) await requireHook(page, info, 'LIBTIME-02', testid, label);
	const slider = page.getByTestId('library-timeline');
	await slider.focus();
	const shape = await slider.evaluate((el) =>
		el instanceof HTMLInputElement && el.type === 'range' ? 'range-input' : (el.getAttribute('role') ?? ''));
	const actual = {
		native: await player(page).locator('audio[controls]').count(),
		src: await audio(page).getAttribute('src'),
		sliderShape: shape === 'range-input' || shape === 'slider',
		sliderName: /seek/i.test(await accessibleName(page, 'library-timeline')),
		focused: (await page.evaluate(() => document.activeElement?.getAttribute('data-testid'))) === 'library-timeline',
		inPlayer: {
			timeline: await player(page).getByTestId('library-timeline').count(),
			readout: await player(page).getByTestId('library-time-readout').count(),
			mode: await player(page).getByTestId('library-time-mode').count(),
			back: await player(page).getByTestId('library-seek-back').count(),
			forward: await player(page).getByTestId('library-seek-forward').count()
		},
		backTag: await page.getByTestId('library-seek-back').evaluate((el) => el.tagName.toLowerCase()),
		backName: /back 10/i.test(await accessibleName(page, 'library-seek-back')),
		forwardName: /forward 10/i.test(await accessibleName(page, 'library-seek-forward'))
	};
	await check(info, {
		native: 1, src: '/__acceptance_audio__/coral.wav',
		sliderShape: true, sliderName: true, focused: true,
		inPlayer: { timeline: 1, readout: 1, mode: 1, back: 1, forward: 1 },
		backTag: 'button', backName: true, forwardName: true
	}, actual);
});

test('LIBTIME-03 readout shows canonical elapsed and total including hours', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 65.7, duration: 185 });
	await requireHook(page, info, 'LIBTIME-03', 'library-time-readout', 'time readout');
	const short = await readoutText(page);
	await loadMedia(page, { currentTime: 3723.4, duration: 7325 });
	const hours = await readoutText(page);
	await check(info, {
		short: { elapsed: true, total: true, clean: true },
		hours: { elapsed: true, total: true, clean: true }
	}, {
		short: { elapsed: short.includes('1:05'), total: short.includes('3:05'), clean: !/NaN|Infinity|undefined|null/.test(short) },
		hours: { elapsed: hours.includes('1:02:03'), total: hours.includes('2:02:05'), clean: !/NaN|Infinity|undefined|null/.test(hours) }
	});
});

test('LIBTIME-04 elapsed-versus-remaining toggle flips the readout coordinates', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 65.7, duration: 185 });
	await requireHook(page, info, 'LIBTIME-04', 'library-time-mode', 'elapsed/remaining toggle');
	await requireHook(page, info, 'LIBTIME-04', 'library-time-readout', 'time readout');
	await setTimeMode(page, info, 'LIBTIME-04', 'remaining');
	const remaining = await readoutText(page);
	await setTimeMode(page, info, 'LIBTIME-04', 'elapsed');
	const elapsed = await readoutText(page);
	await check(info, {
		remaining: { mode: true, value: true, total: true },
		elapsed: { mode: true, value: true, total: true }
	}, {
		remaining: { mode: /remaining/i.test(remaining), value: remaining.includes('1:59'), total: remaining.includes('3:05') },
		elapsed: { mode: /elapsed/i.test(elapsed), value: elapsed.includes('1:05'), total: elapsed.includes('3:05') }
	});
});

test('LIBTIME-05 back and forward move the current audio by exactly ten seconds', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 50, duration: 200 });
	await requireEnabled(page, info, 'LIBTIME-05', 'library-seek-back', 'Back 10 seconds');
	await requireEnabled(page, info, 'LIBTIME-05', 'library-seek-forward', 'Forward 10 seconds');
	await page.getByTestId('library-seek-back').click();
	await settle(page);
	const afterBack = await mediaTime(page);
	await page.getByTestId('library-seek-forward').click();
	await settle(page);
	await check(info, { afterBack: 40, afterForward: 50 }, { afterBack, afterForward: await mediaTime(page) });
});

test('LIBTIME-06 endpoint seeks clamp to zero and finite duration', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 5, duration: 200 });
	await requireEnabled(page, info, 'LIBTIME-06', 'library-seek-back', 'Back 10 seconds');
	await page.getByTestId('library-seek-back').click();
	await settle(page);
	const floored = await mediaTime(page);
	await loadMedia(page, { currentTime: 197, duration: 200 });
	await requireEnabled(page, info, 'LIBTIME-06', 'library-seek-forward', 'Forward 10 seconds');
	await page.getByTestId('library-seek-forward').click();
	await settle(page);
	const ceilinged = await mediaTime(page);
	await check(info, { floored: 0, ceilinged: 200 }, { floored, ceilinged });
});

test('LIBTIME-07 unknown duration gets truthful copy and disabled seeking', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 12, duration: NaN });
	for (const [testid, label] of [
		['library-time-readout', 'time readout'], ['library-seek-back', 'Back 10 seconds'],
		['library-seek-forward', 'Forward 10 seconds'], ['library-timeline', 'seek slider']
	] as const) await requireHook(page, info, 'LIBTIME-07', testid, label);
	const text = await readoutText(page);
	const region = await player(page).innerText();
	await check(info, {
		copy: true, back: 'disabled', forward: 'disabled', slider: 'disabled',
		clean: true, noPercentage: true
	}, {
		copy: /duration unavailable/i.test(text),
		back: await hookState(page, 'library-seek-back'),
		forward: await hookState(page, 'library-seek-forward'),
		slider: await hookState(page, 'library-timeline'),
		clean: !/NaN|Infinity|undefined|null/.test(region),
		noPercentage: !/\d+\s*%/.test(text)
	});
});

test('LIBTIME-08 infinite duration gets truthful copy and disabled seeking', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 12, duration: Infinity });
	await requireHook(page, info, 'LIBTIME-08', 'library-time-readout', 'time readout');
	const text = await readoutText(page);
	const region = await player(page).innerText();
	await check(info, {
		copy: true, back: 'disabled', forward: 'disabled', slider: 'disabled',
		clean: true, noPercentage: true
	}, {
		copy: /duration unavailable/i.test(text),
		back: await hookState(page, 'library-seek-back'),
		forward: await hookState(page, 'library-seek-forward'),
		slider: await hookState(page, 'library-timeline'),
		clean: !/NaN|Infinity|undefined|null/.test(region),
		noPercentage: !/\d+\s*%/.test(text)
	});
});

test('LIBTIME-09 unseekable media disables seeking with truthful copy', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 12, duration: 120, seekable: [] });
	for (const [testid, label] of [
		['library-time-readout', 'time readout'], ['library-seek-back', 'Back 10 seconds'],
		['library-seek-forward', 'Forward 10 seconds'], ['library-timeline', 'seek slider']
	] as const) await requireHook(page, info, 'LIBTIME-09', testid, label);
	const text = await readoutText(page);
	const status = page.getByTestId('library-seek-status');
	const combined = `${text} ${(await status.count()) === 1 ? await status.innerText() : ''}`;
	await check(info, { copy: true, back: 'disabled', forward: 'disabled', slider: 'disabled', clean: true }, {
		copy: /unavailable|not seekable/i.test(combined),
		back: await hookState(page, 'library-seek-back'),
		forward: await hookState(page, 'library-seek-forward'),
		slider: await hookState(page, 'library-timeline'),
		clean: !/NaN|Infinity|undefined|null/.test(await player(page).innerText())
	});
});

test('LIBTIME-10 seeks intersect the actual disjoint seekable ranges', async ({ libraryPage: page }, info) => {
	await replace(page);
	const ranges: Array<[number, number]> = [[0, 30], [60, 120]];
	await loadMedia(page, { currentTime: 70, duration: 120, seekable: ranges });
	await requireEnabled(page, info, 'LIBTIME-10', 'library-seek-back', 'Back 10 seconds');
	await requireEnabled(page, info, 'LIBTIME-10', 'library-seek-forward', 'Forward 10 seconds');
	await page.getByTestId('library-seek-back').click();
	await settle(page);
	const steppedBack = await mediaTime(page);
	await page.getByTestId('library-seek-forward').click();
	await settle(page);
	const steppedForward = await mediaTime(page);
	// 25 + 10 lands in the gap: the build clamps to a seekable edge (30 or 60).
	await loadMedia(page, { currentTime: 25, duration: 120, seekable: ranges });
	await page.getByTestId('library-seek-forward').click();
	await settle(page);
	const gapTarget = await mediaTime(page);
	await loadMedia(page, { currentTime: 115, duration: 120, seekable: ranges });
	await page.getByTestId('library-seek-forward').click();
	await settle(page);
	const ceilinged = await mediaTime(page);
	await check(info, {
		steppedBack: 60, steppedForward: 70, gapInRange: true, gapValue: gapTarget, ceilinged: 120
	}, {
		steppedBack, steppedForward,
		gapInRange: inRanges(gapTarget, ranges) && (gapTarget === 30 || gapTarget === 60),
		gapValue: gapTarget, ceilinged
	});
});

test('LIBTIME-11 seeks preserve intent and never play, load or fetch', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 50, duration: 200, paused: true });
	await requireEnabled(page, info, 'LIBTIME-11', 'library-seek-back', 'Back 10 seconds');
	await requireEnabled(page, info, 'LIBTIME-11', 'library-seek-forward', 'Forward 10 seconds');
	const fetches = watchFetches(page);
	const before = await lifecycleOf(page);
	await page.getByTestId('library-seek-back').click();
	await settle(page);
	const afterBack = await mediaTime(page);
	const pausedAfterBack = await audio(page).evaluate((el: HTMLAudioElement) => el.paused);
	await audio(page).evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'paused', { value: false, configurable: true });
	});
	await page.getByTestId('library-seek-forward').click();
	await settle(page);
	const afterForward = await mediaTime(page);
	const playingAfterForward = await audio(page).evaluate((el: HTMLAudioElement) => el.paused);
	await check(info, {
		afterBack: 40, pausedStaysTrue: true, afterForward: 50, pausedStaysFalse: true,
		lifecycle: before, fetches: []
	}, {
		afterBack, pausedStaysTrue: pausedAfterBack === true,
		afterForward, pausedStaysFalse: playingAfterForward === false,
		lifecycle: await lifecycleOf(page), fetches
	});
});

test('LIBTIME-12 a rejected assignment reports failure without claiming success', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 50, duration: 200 });
	await requireEnabled(page, info, 'LIBTIME-12', 'library-seek-forward', 'Forward 10 seconds');
	await requireHook(page, info, 'LIBTIME-12', 'library-time-readout', 'time readout');
	const before = await readoutText(page);
	await audio(page).evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'currentTime', {
			get: () => 50,
			set: () => { throw new Error('boom-private-decoder-detail'); },
			configurable: true
		});
	});
	await page.getByTestId('library-seek-forward').click();
	await settle(page);
	const status = page.getByTestId('library-seek-status');
	const statusText = (await status.count()) === 1 ? (await status.innerText()).replace(/\s+/g, ' ').trim() : '';
	const after = await readoutText(page);
	await check(info, {
		visible: true, failureCopy: true, nonSensitive: true, timeHeld: true, readoutHeld: true
	}, {
		visible: (await status.count()) === 1 && await status.isVisible(),
		failureCopy: /couldn|fail|unable|error/i.test(statusText),
		nonSensitive: !/boom|private|Error:|at |\.ts|stack/i.test(statusText) && statusText.length > 0 && statusText.length < 300,
		timeHeld: await mediaTime(page) === 50,
		readoutHeld: after === before && before.length > 0
	});
});

test('LIBTIME-13 time bursts coalesce visually and never announce per tick', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 0, duration: 300 });
	await requireHook(page, info, 'LIBTIME-13', 'library-time-readout', 'time readout');
	await page.evaluate(() => {
		const readout = document.querySelector('[data-testid="library-time-readout"]')!;
		(window as unknown as { __libtimeBursts: { visual: number; announcements: string[] } }).__libtimeBursts = { visual: 0, announcements: [] };
		const seen = (window as unknown as { __libtimeBursts: { visual: number; announcements: string[] } }).__libtimeBursts;
		new MutationObserver(() => { seen.visual += 1; }).observe(readout, { childList: true, characterData: true, subtree: true });
		const region = readout.closest('[role="region"]') ?? document.body;
		new MutationObserver(() => {
			for (const node of region.querySelectorAll('[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]')) {
				if (node.getAttribute('aria-live') === 'off') continue;
				const text = (node.textContent ?? '').replace(/\s+/g, ' ').trim();
				if (text && seen.announcements[seen.announcements.length - 1] !== text) seen.announcements.push(text);
			}
		}).observe(region, { childList: true, characterData: true, subtree: true });
	});
	for (const base of [10, 20, 30]) {
		await audio(page).evaluate((el: HTMLAudioElement, start: number) => {
			for (let i = 0; i < 4; i += 1) {
				(el as unknown as { currentTime: number }).currentTime = start + i;
				el.dispatchEvent(new Event('timeupdate'));
			}
		}, base);
		await settle(page);
	}
	const bursts = await page.evaluate(() => (window as unknown as { __libtimeBursts: { visual: number; announcements: string[] } }).__libtimeBursts);
	const final = await readoutText(page);
	// Three synchronous 4-tick bursts must land as at most one visual update
	// each (any batching/throttle coalesces a same-task burst) and zero live
	// announcements; the readout still converges on the last tick.
	await check(info, { bursts: 3, coalesced: true, visual: bursts.visual, announcements: 0, finalElapsed: true }, {
		bursts: 3, coalesced: bursts.visual <= 3, visual: bursts.visual,
		announcements: bursts.announcements.length, finalElapsed: final.includes('0:33')
	});
});

test('LIBTIME-14 keyed replacement resets state and detached events are ignored', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 50, duration: 120 });
	await requireHook(page, info, 'LIBTIME-14', 'library-time-readout', 'time readout');
	const before = await readoutText(page);
	const old = await audio(page).elementHandle();
	await page.evaluate(() => window.libraryTimeline.select(1));
	await settle(page);
	await loadMedia(page, { currentTime: 0, duration: 200 });
	const reset = await readoutText(page);
	await old!.evaluate((el) => {
		(el as HTMLAudioElement).dispatchEvent(new Event('timeupdate'));
		(el as HTMLAudioElement).dispatchEvent(new Event('loadedmetadata'));
	});
	await settle(page);
	const afterDetached = await readoutText(page);
	await check(info, {
		seeded: true, resetElapsed: true, resetTotal: true, noStale: true,
		detached: true, ignored: true
	}, {
		seeded: before.includes('0:50'),
		resetElapsed: reset.includes('0:00'), resetTotal: reset.includes('3:20'),
		noStale: !reset.includes('0:50'),
		detached: await old!.evaluate((el) => !el.isConnected),
		ignored: afterDetached === reset
	});
});

test('LIBTIME-15 empty queue removes the seek timeline bundle', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 50, duration: 120 });
	const seeded = { audio: await audio(page).count() };
	await replace(page, state(-1, []));
	await settle(page);
	await check(info, {
		seeded: { audio: 1 },
		cleared: { sections: 0, timeline: 0, readout: 0, mode: 0, back: 0, forward: 0, audio: 0 }
	}, {
		seeded,
		cleared: {
			sections: await player(page).count(),
			timeline: await page.getByTestId('library-timeline').count(),
			readout: await page.getByTestId('library-time-readout').count(),
			mode: await page.getByTestId('library-time-mode').count(),
			back: await page.getByTestId('library-seek-back').count(),
			forward: await page.getByTestId('library-seek-forward').count(),
			audio: await page.locator('audio').count()
		}
	});
});

test('LIBTIME-16 account transition clears the queue and the seek timeline', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 50, duration: 120 });
	const seeded = { audio: await audio(page).count(), rows: await page.getByTestId('queue-item').count() };
	await page.evaluate((key) => window.dispatchEvent(new StorageEvent('storage', { key })), AUTH_KEY);
	await settle(page);
	await check(info, {
		seeded: { audio: 1, rows: 3 },
		cleared: { rows: 0, sections: 0, timeline: 0, readout: 0, back: 0, forward: 0, audio: 0 }
	}, {
		seeded,
		cleared: {
			rows: await page.getByTestId('queue-item').count(),
			sections: await player(page).count(),
			timeline: await page.getByTestId('library-timeline').count(),
			readout: await page.getByTestId('library-time-readout').count(),
			back: await page.getByTestId('library-seek-back').count(),
			forward: await page.getByTestId('library-seek-forward').count(),
			audio: await page.locator('audio').count()
		}
	});
});

test('LIBTIME-17 share-style programmatic seeks reflect without extra fetch or play', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 100, duration: 300 });
	await requireHook(page, info, 'LIBTIME-17', 'library-time-readout', 'time readout');
	const fetches = watchFetches(page);
	const before = await lifecycleOf(page);
	await audio(page).evaluate((el: HTMLAudioElement) => {
		el.currentTime = 45;
		el.dispatchEvent(new Event('seeking'));
		el.dispatchEvent(new Event('seeked'));
		el.dispatchEvent(new Event('timeupdate'));
	});
	await settle(page);
	await check(info, {
		follows: true, lifecycle: before, fetches: [],
		back: 'enabled', forward: 'enabled'
	}, {
		follows: (await readoutText(page)).includes('0:45'),
		lifecycle: await lifecycleOf(page), fetches,
		back: await hookState(page, 'library-seek-back'),
		forward: await hookState(page, 'library-seek-forward')
	});
});

test('LIBTIME-18 slider is keyboard operable with a visible focus indicator', async ({ libraryPage: page }, info) => {
	await replace(page);
	await loadMedia(page, { currentTime: 60, duration: 300 });
	await requireHook(page, info, 'LIBTIME-18', 'library-timeline', 'seek slider');
	const slider = page.getByTestId('library-timeline');
	// Tab from the body so keyboard modality (and :focus-visible styling)
	// applies; programmatic .focus() would not exercise the real path.
	await page.evaluate(() => { (document.activeElement as HTMLElement | null)?.blur?.(); });
	let tabs = 0;
	while (tabs < 80 && (await page.evaluate(() => document.activeElement?.getAttribute('data-testid'))) !== 'library-timeline') {
		await page.keyboard.press('Tab');
		tabs += 1;
	}
	const focused = await page.evaluate(() => document.activeElement?.getAttribute('data-testid'));
	const indicator = await slider.evaluate((el) => {
		const style = getComputedStyle(el);
		return style.outlineStyle !== 'none' && style.outlineWidth !== '0px' ? 'outline' : style.boxShadow !== 'none' ? 'shadow' : 'none';
	});
	const before = await mediaTime(page);
	await page.keyboard.press('ArrowRight');
	await settle(page);
	const raised = await mediaTime(page);
	await page.keyboard.press('ArrowLeft');
	await settle(page);
	const lowered = await mediaTime(page);
	await check(info, {
		focused: 'library-timeline', indicatorFound: true,
		raised: true, lowered: true, bounded: true, valueSynced: true
	}, {
		focused, indicatorFound: indicator !== 'none',
		raised: raised > before, lowered: lowered < raised,
		bounded: raised <= 300 && lowered >= 0,
		valueSynced: (await sliderValue(page)) !== null
	});
});

test.describe('touch seek', () => {
	test.use({ hasTouch: true });
	test('LIBTIME-19 tapping the slider seeks toward the tapped position', async ({ libraryPage: page }, info) => {
		await replace(page);
		await loadMedia(page, { currentTime: 30, duration: 300 });
		await requireHook(page, info, 'LIBTIME-19', 'library-timeline', 'seek slider');
		const box = await page.getByTestId('library-timeline').boundingBox();
		expect(box, 'LIBTIME-19: seek slider has a tappable box. Next action: render a visible slider.').not.toBeNull();
		await page.touchscreen.tap(box!.x + box!.width * 0.8, box!.y + box!.height / 2);
		await settle(page);
		const tapped = await mediaTime(page);
		await check(info, { moved: true, towardTap: true, bounded: true, lifecycle: { playCalls: 0, pauseCalls: 0, loadCalls: 0 } }, {
			moved: tapped !== 30, towardTap: tapped > 100, bounded: tapped >= 0 && tapped <= 300,
			lifecycle: await lifecycleOf(page)
		});
	});
});

for (const [index, theme] of ['light', 'dark'].entries()) {
	test(`LIBTIME-${20 + index} ${theme} narrow seek timeline stays readable and contained`, async ({ libraryPage: page }, info) => {
		await page.setViewportSize({ width: 360, height: 740 });
		await page.emulateMedia({ colorScheme: theme as 'light' | 'dark' });
		await page.evaluate((mode) => document.documentElement.setAttribute('data-appearance', mode), theme);
		await replace(page, state(0, [{ ...TRACKS[0], title: 'Coral Night '.repeat(12).trim(), artist: 'Mira North '.repeat(8).trim() }]));
		await loadMedia(page, { currentTime: 65.7, duration: 185 });
		for (const [testid, label] of [
			['library-timeline', 'seek slider'], ['library-time-readout', 'time readout'],
			['library-seek-back', 'Back 10 seconds'], ['library-seek-forward', 'Forward 10 seconds']
		] as const) await requireHook(page, info, `LIBTIME-${20 + index}`, testid, label);
		const layout = await page.getByTestId('library-time-readout').evaluate((node) => {
			const box = node.getBoundingClientRect();
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
			const hooks = ['library-timeline', 'library-seek-back', 'library-seek-forward'].map((testid) =>
				document.querySelector(`[data-testid="${testid}"]`)?.getBoundingClientRect());
			return {
				contained: box.x >= 0 && box.right <= innerWidth + 1
					&& hooks.every((rect) => rect && rect.x >= 0 && rect.right <= innerWidth + 1)
					&& document.documentElement.scrollWidth <= innerWidth + 1,
				readable: background.length >= 3 && (values[0] + 0.05) / (values[1] + 0.05) >= 4.5
			};
		});
		await check(info, { layout: { contained: true, readable: true } }, { layout });
	});
}
