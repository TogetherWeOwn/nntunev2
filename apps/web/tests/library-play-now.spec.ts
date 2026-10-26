import { expect, test, type Locator, type Page, type TestInfo } from '@playwright/test';

/**
 * LIBPLAYNOW — play a displayed library track immediately without rebuilding the queue.
 *
 * Protected acceptance bank, tests-only handoff. The Builder implements the
 * feature and turns this bank GREEN without editing this file; a wrong test
 * comes back to the acceptance card instead. No live providers, no audio
 * hardware, no production checks: `/api/library` is route-mocked, every
 * foreign host and every media fetch is aborted, and `HTMLMediaElement.play`
 * is stubbed to record calls (resolving, or rejecting in the rejection case).
 *
 * Contract under test (each library row grows one native control):
 * - Every displayed library row renders a real focusable `<button>` named
 *   exactly `Play {display title}` (the same display title the row's `Select`
 *   checkbox uses, e.g. `Play Dusk Song`). Scoped to `library-item` rows so
 *   it never collides with the queue panel's `Play {title} next` buttons.
 * - Activating it on an UNQUEUED track inserts that track immediately after
 *   the current queue slot and selects it there; on a QUEUED track it
 *   selects it in place. Nothing else is removed, reordered or duplicated.
 * - With an EMPTY queue the track becomes the sole selected track. A
 *   metadata-only track (no usable stream URL) still selects honestly but
 *   never attempts playback and never claims to be playing.
 * - After selecting a playable track the page calls `play()` on the library
 *   audio element exactly once, inside the user gesture. A rejected `play()`
 *   leaves the track selected-but-paused with truthful feedback (names the
 *   title, never claims playing, names the failure) and schedules no
 *   out-of-gesture retry: the call count stays frozen.
 * - Reload, library snapshot restore, Refresh/Retry and auth transitions
 *   never start audio: zero `play()` calls on those paths.
 * - A row that disappears before activation (stale handle) and any handle
 *   held across the auth boundary are inert: no queue change, no playback,
 *   no new announcement, no next-account leakage.
 * - Display state is untouched beyond the intended queue/select change:
 *   favorites (set and shelf order), checkbox subset, find query and scope,
 *   ordering, view and share feedback all survive a Play activation.
 * - Exactly one concise polite outcome per real activation, in the existing
 *   `queue-announcement` live region (`role=status`, names the title,
 *   at most 140 characters). No-ops announce nothing new.
 *
 * Audible playback itself is not observable in the headless offline runner,
 * so "plays" is pinned as: correct queue/select/src state plus exactly one
 * in-gesture `play()` call. Cases C00 and C06 are guards that stay GREEN on
 * the pre-implementation base (existing selection stays metadata-only; the
 * lifecycle never autoplays). Every other case is assertion-RED on base via
 * the compact JSON verdict below (`status: RED`, `error_code:
 * LIBPLAYNOW_CONTRACT`) — the missing Play control is probed by count, so
 * RED is a failed equality, never a click timeout, import, browser or pool
 * failure. The runner owns preview lifetimes; run from `apps/web` with a
 * free preview port:
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm exec playwright test tests/library-play-now.spec.ts --workers=1
 * Regression: the neighbouring `library-player-queue`, `library-queue-manage`,
 * `library-queue-direct-selection`, `library-native-now-playing` and
 * `library-selected-view` suites stay untouched and green.
 *
 * Breaks caught: missing/misnamed Play control, rebuild-the-queue play,
 * duplicate insert of a queued track, empty-queue no-op, streamless track
 * claiming playback, unhandled play() rejection, out-of-gesture retry,
 * autoplay on reload/refresh/restore/auth change, stale-handle activation,
 * cross-account queue leakage, favorites/subset/find/order/view/share
 * mutation, double or assertive announcements, focus theft.
 */

test.setTimeout(20_000);

interface LibraryPayload {
	current: unknown;
	status: number;
}

interface MediaProbe {
	play: number;
	pause: number;
	load: number;
	storage: string[];
}

declare global {
	interface Window {
		__libPlayNow?: MediaProbe;
		__libPlayNowMode?: string;
		__libPlayNowNotes?: string[];
		__libPlayNowObserver?: MutationObserver;
	}
}

// API order is deliberately unsorted; only Dusk Song matches a title find
// for 'song', and only Silent Sketch lacks a stream URL (metadata-only).
const ITEMS = [
	{
		id: 'now-ember',
		title: 'Ember Noon',
		artist: 'West Quartet',
		streamUrl: '/__acceptance_audio__/ember.wav'
	},
	{
		id: 'now-coral',
		title: 'Coral Night',
		artist: 'Mira North',
		streamUrl: '/__acceptance_audio__/coral.wav'
	},
	{ id: 'now-silent', title: 'Silent Sketch', artist: 'North Solo' },
	{
		id: 'now-pine',
		title: 'Pine Morning',
		artist: 'South Ensemble',
		streamUrl: '/__acceptance_audio__/pine.wav'
	},
	{
		id: 'now-dusk',
		title: 'Dusk Song',
		artist: 'East Trio',
		streamUrl: '/__acceptance_audio__/dusk.wav'
	}
];

const NEXT_ACCOUNT = [
	{
		id: 'now-bravo',
		title: 'Bravo Song',
		artist: 'Next Account',
		streamUrl: '/__acceptance_audio__/bravo.wav'
	}
];

const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';

/** Abort every foreign host and every media fetch; fulfill the library locally. */
async function localOnly(
	page: Page,
	origin: string,
	payload: LibraryPayload = { current: ITEMS, status: 200 },
	calls: string[] = []
): Promise<void> {
	const local = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(local.hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (
			url.origin !== local.origin ||
			url.pathname.startsWith('/api/') ||
			request.resourceType() === 'media'
		) {
			calls.push(`${request.method()} ${url.origin}${url.pathname}`);
			if (
				url.origin === local.origin &&
				url.pathname === '/api/library' &&
				request.method() === 'GET'
			) {
				return route.fulfill({
					status: payload.status,
					contentType: 'application/json',
					body: JSON.stringify(payload.current)
				});
			}
			return route.abort();
		}
		return route.continue();
	});
}

/** Stub transport + storage writes; `reject` makes play() reject like a blocked gesture. */
async function instrumentMedia(page: Page, mode: 'resolve' | 'reject' = 'resolve'): Promise<void> {
	await page.addInitScript((stubMode: string) => {
		const probe = { play: 0, pause: 0, load: 0, storage: [] as string[] };
		const scope = window as unknown as {
			__libPlayNow: typeof probe;
			__libPlayNowMode: string;
		};
		scope.__libPlayNow = probe;
		scope.__libPlayNowMode = stubMode;
		HTMLMediaElement.prototype.play = function () {
			probe.play += 1;
			if (scope.__libPlayNowMode === 'reject') {
				return Promise.reject(new DOMException('play() rejected by acceptance stub', 'NotAllowedError'));
			}
			return Promise.resolve();
		} as () => Promise<void>;
		HTMLMediaElement.prototype.pause = function () {
			probe.pause += 1;
		};
		HTMLMediaElement.prototype.load = function () {
			probe.load += 1;
		};
		const methods = ['setItem', 'removeItem', 'clear'] as const;
		for (const method of methods) {
			const original = Storage.prototype[method] as (...args: string[]) => void;
			Storage.prototype[method] = function (this: Storage, ...args: string[]) {
				probe.storage.push(method);
				return original.apply(this, args);
			} as never;
		}
	}, mode);
}

async function readProbe(page: Page): Promise<MediaProbe> {
	return page.evaluate(() => ({ ...(window.__libPlayNow as MediaProbe) }));
}

async function resetProbe(page: Page): Promise<void> {
	await page.evaluate(() => {
		const probe = window.__libPlayNow as MediaProbe;
		probe.play = 0;
		probe.pause = 0;
		probe.load = 0;
		probe.storage = [];
	});
}

async function settle(page: Page): Promise<void> {
	await page.evaluate(
		() =>
			new Promise<void>((resolve) =>
				requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
			)
	);
}

interface Seed {
	calls: string[];
	errors: string[];
	payload: LibraryPayload;
}

async function seed(
	page: Page,
	baseURL: string,
	options: { payload?: LibraryPayload; mode?: 'resolve' | 'reject' } = {}
): Promise<Seed> {
	const payload = options.payload ?? { current: ITEMS, status: 200 };
	const calls: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String((error as Error)?.message ?? error)));
	await instrumentMedia(page, options.mode ?? 'resolve');
	await localOnly(page, baseURL, payload, calls);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount((payload.current as unknown[]).length);
	return { calls, errors, payload };
}

function libraryRow(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function rowSelectButton(page: Page, id: string): Locator {
	return libraryRow(page, id).locator('button.nt-library-row');
}

function playButton(page: Page, id: string, title: string): Locator {
	return libraryRow(page, id).getByRole('button', { name: `Play ${title}`, exact: true });
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

async function currentId(page: Page): Promise<string | null> {
	return page
		.locator('[data-testid="queue-item"][aria-current="true"]')
		.evaluateAll((items) =>
			items.length === 1
				? (items[0]?.getAttribute('data-track-id') ?? items[0]?.getAttribute('data-id'))
				: `__count_${items.length}`
		)
		.then((value) => value as unknown as string | null);
}

async function announcement(page: Page): Promise<{ text: string; role: string | null }> {
	const region = page.getByTestId('queue-announcement');
	return {
		text: ((await region.textContent()) ?? '').replace(/\s+/g, ' ').trim(),
		role: await region.getAttribute('role')
	};
}

async function audioState(page: Page): Promise<{ count: number; src: string | null; autoplay: boolean }> {
	const audio = page.getByTestId('audio-player');
	const count = await audio.count();
	if (count !== 1) return { count, src: null, autoplay: false };
	return {
		count,
		src: await audio.getAttribute('src'),
		autoplay: await audio.evaluate((element) => (element as HTMLAudioElement).hasAttribute('autoplay'))
	};
}

/**
 * Visible live regions whose text names the title, excluding the persistent
 * `now-playing` selection readout: exactly one (the outcome announcement) is
 * the contract; zero means silence and two or more means double chatter.
 */
async function liveOutcomePaths(page: Page, title: string): Promise<{ count: number; paths: string[] }> {
	return page.evaluate((needle) => {
		const nodes = [
			...document.querySelectorAll('[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]')
		];
		const paths: string[] = [];
		for (const node of nodes) {
			if (node.closest('[data-testid="now-playing"]')) continue;
			if (node.getAttribute('aria-live') === 'off') continue;
			if (node.getClientRects().length === 0) continue;
			if ((node.textContent ?? '').includes(needle)) {
				paths.push(node.getAttribute('data-testid') ?? node.tagName.toLowerCase());
			}
		}
		return { count: paths.length, paths };
	}, title);
}

/** Record distinct `queue-announcement` texts across one activation ('' + outcome only). */
async function observeAnnouncements(page: Page): Promise<() => Promise<string[]>> {
	await page.evaluate(() => {
		window.__libPlayNowNotes = [];
		const region = document.querySelector('[data-testid="queue-announcement"]') as Element;
		const observer = new MutationObserver(() => {
			window.__libPlayNowNotes?.push(((region.textContent ?? '').replace(/\s+/g, ' ').trim()));
		});
		observer.observe(region, { childList: true, characterData: true, subtree: true });
		window.__libPlayNowObserver = observer;
	});
	return () =>
		page.evaluate(() => {
			window.__libPlayNowObserver?.disconnect();
			return [...new Set(window.__libPlayNowNotes ?? [])];
		});
}

const PLAYING_PATTERN = /playing|now playing/i;
const FAILURE_PATTERN = /couldn|unable|didn|not start|paused|try again|tap|press play|start playback|failed|blocked|unavailable/i;

/**
 * Requests that would hit a real provider: the route-mocked library fetch
 * and the stubbed acceptance-audio loads (always aborted, never fetched)
 * are harness traffic, not provider traffic.
 */
function providerCalls(calls: string[]): string[] {
	return calls.filter((call) => !call.includes('/api/library') && !call.includes('/__acceptance_audio__'));
}

async function positionText(page: Page): Promise<string> {
	return ((await page.getByTestId('queue-position').textContent()) ?? '').replace(/\s+/g, ' ').trim();
}

async function check(
	info: TestInfo,
	expected: unknown,
	actual: unknown,
	evidence: Record<string, unknown> = {}
): Promise<void> {
	let passed = false;
	let failure: unknown = null;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} catch (error) {
		failure = error;
	} finally {
		const receipt = {
			case_id: info.title.split(' ')[0],
			expected,
			actual: { ...(actual as Record<string, unknown>), ...evidence },
			status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : 'LIBPLAYNOW_CONTRACT',
			next_action: passed
				? 'preserve acceptance'
				: 'implement library play-now; do not edit protected tests'
		};
		const body = JSON.stringify(receipt);
		console.log(body);
		await info.attach('acceptance-case.json', { body, contentType: 'application/json' });
	}
	if (failure) throw failure;
}

test.afterEach(async ({}, info) => {
	if (info.attachments.some((attachment) => attachment.name === 'acceptance-case.json')) return;
	const body = JSON.stringify({
		case_id: info.title.split(' ')[0],
		expected: 'reach behavioral assertion',
		actual: info.errors.map((error) => error.message),
		status: 'ERROR',
		error_code: 'HARNESS_OR_ACTION_FAILURE',
		next_action: 'repair harness/action; not verified feature RED'
	});
	console.log(body);
	await info.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

// Guard (GREEN before and after): plain row selection stays metadata-only —
// the queue fills and the carried stream URL renders the native player, but
// nothing plays, nothing autoplays and no provider is fetched.
test('LIBPLAYNOW-C00 control: row selection stays metadata-only with zero transport', async ({
	page,
	baseURL
}) => {
	const { calls, errors } = await seed(page, baseURL!);
	await rowSelectButton(page, 'now-coral').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await settle(page);
	const probe = await readProbe(page);
	const audio = await audioState(page);
	const actual = {
		order: await queueOrder(page),
		current: await currentId(page),
		play: probe.play,
		pause: probe.pause,
		audioElements: audio.count,
		src: audio.src,
		autoplay: audio.autoplay,
		providerCalls: providerCalls(calls),
		errors
	};
	await check(
		test.info(),
		{
			order: ['now-coral'],
			current: 'now-coral',
			play: 0,
			pause: 0,
			audioElements: 1,
			src: '/__acceptance_audio__/coral.wav',
			autoplay: false,
			providerCalls: [],
			errors: []
		},
		actual,
		{ probe, calls }
	);
});

test('LIBPLAYNOW-C01 play unqueued track inserts after current, selects and plays once', async ({
	page,
	baseURL
}, info) => {
	const { calls, errors } = await seed(page, baseURL!);
	for (const id of ['now-coral', 'now-pine', 'now-ember']) await rowSelectButton(page, id).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	calls.length = 0;
	await resetProbe(page);
	const stopObserving = await observeAnnouncements(page);
	const control = playButton(page, 'now-dusk', 'Dusk Song');
	const playControl = await control.count();
	if (playControl === 1) await control.click();
	await settle(page);
	const notes = await stopObserving();
	const announced = await announcement(page);
	const probe = await readProbe(page);
	const audio = await audioState(page);
	const live = await liveOutcomePaths(page, 'Dusk Song');
	const actual = {
		playControl,
		order: await queueOrder(page),
		current: await currentId(page),
		positionNames: (await positionText(page)).includes('Track 2 of 4'),
		announceNames: announced.text.includes('Dusk Song'),
		announcePlaying: PLAYING_PATTERN.test(announced.text),
		announceNoFailure: !FAILURE_PATTERN.test(announced.text),
		announceConcise: announced.text.length >= 1 && announced.text.length <= 140,
		announceRole: announced.role,
		announceSingleOutcome:
			announced.text.length > 0 && notes.every((note) => note === '' || note === announced.text),
		livePaths: live.count,
		src: audio.src,
		audioCount: audio.count,
		autoplay: audio.autoplay,
		play: probe.play,
		pause: probe.pause,
		load: probe.load,
		storageWrites: probe.storage.length,
		providerCalls: providerCalls(calls),
		focusKept: playControl === 1 ? await control.evaluate((element) => element === document.activeElement) : false,
		alerts: await page.getByRole('alert').count(),
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			order: ['now-coral', 'now-dusk', 'now-pine', 'now-ember'],
			current: 'now-dusk',
			positionNames: true,
			announceNames: true,
			announcePlaying: true,
			announceNoFailure: true,
			announceConcise: true,
			announceRole: 'status',
			announceSingleOutcome: true,
			livePaths: 1,
			src: '/__acceptance_audio__/dusk.wav',
			audioCount: 1,
			autoplay: false,
			play: 1,
			pause: 0,
			load: 0,
			storageWrites: 0,
			providerCalls: [],
			focusKept: true,
			alerts: 0,
			errors: []
		},
		actual,
		{ announced: announced.text, notes, live: live.paths, probe, calls }
	);
});

test('LIBPLAYNOW-C02 play queued track selects in place without insert, move or duplicate', async ({
	page,
	baseURL
}, info) => {
	const { calls, errors } = await seed(page, baseURL!);
	for (const id of ['now-coral', 'now-pine', 'now-ember']) await rowSelectButton(page, id).click();
	calls.length = 0;
	await resetProbe(page);
	const control = playButton(page, 'now-pine', 'Pine Morning');
	const playControl = await control.count();
	if (playControl === 1) await control.click();
	await settle(page);
	const announced = await announcement(page);
	const probe = await readProbe(page);
	const audio = await audioState(page);
	const actual = {
		playControl,
		order: await queueOrder(page),
		current: await currentId(page),
		positionNames: (await positionText(page)).includes('Track 2 of 3'),
		announceNames: announced.text.includes('Pine Morning'),
		announcePlaying: PLAYING_PATTERN.test(announced.text),
		announceRole: announced.role,
		src: audio.src,
		play: probe.play,
		providerCalls: providerCalls(calls),
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			order: ['now-coral', 'now-pine', 'now-ember'],
			current: 'now-pine',
			positionNames: true,
			announceNames: true,
			announcePlaying: true,
			announceRole: 'status',
			src: '/__acceptance_audio__/pine.wav',
			play: 1,
			providerCalls: [],
			errors: []
		},
		actual,
		{ announced: announced.text, probe }
	);
});

test('LIBPLAYNOW-C03 empty queue: play becomes the sole selected track honestly', async ({
	page,
	baseURL
}, info) => {
	const { calls, errors } = await seed(page, baseURL!);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
	calls.length = 0;
	await resetProbe(page);
	const control = playButton(page, 'now-coral', 'Coral Night');
	const playControl = await control.count();
	if (playControl === 1) await control.click();
	await settle(page);
	const announced = await announcement(page);
	const probe = await readProbe(page);
	const audio = await audioState(page);
	const actual = {
		playControl,
		order: await queueOrder(page),
		current: await currentId(page),
		positionNames: (await positionText(page)).includes('Track 1 of 1'),
		nextDisabled: await page.getByTestId('player-next').isDisabled(),
		announceNames: announced.text.includes('Coral Night'),
		announcePlaying: PLAYING_PATTERN.test(announced.text),
		src: audio.src,
		play: probe.play,
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			order: ['now-coral'],
			current: 'now-coral',
			positionNames: true,
			nextDisabled: true,
			announceNames: true,
			announcePlaying: true,
			src: '/__acceptance_audio__/coral.wav',
			play: 1,
			errors: []
		},
		actual,
		{ announced: announced.text, probe }
	);
});

test('LIBPLAYNOW-C04 streamless track selects honestly with no playback attempt or claim', async ({
	page,
	baseURL
}, info) => {
	const { errors } = await seed(page, baseURL!);
	await rowSelectButton(page, 'now-coral').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await resetProbe(page);
	const control = playButton(page, 'now-silent', 'Silent Sketch');
	const playControl = await control.count();
	if (playControl === 1) await control.click();
	await settle(page);
	const announced = await announcement(page);
	const probe = await readProbe(page);
	const audio = await audioState(page);
	const actual = {
		playControl,
		order: await queueOrder(page),
		current: await currentId(page),
		nowPlayingNames: ((await page.getByTestId('now-playing').textContent()) ?? '').includes('Silent Sketch'),
		announceNames: announced.text.includes('Silent Sketch'),
		announceClaimsPlaying: PLAYING_PATTERN.test(announced.text),
		audioCount: audio.count,
		play: probe.play,
		pause: probe.pause,
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			order: ['now-coral', 'now-silent'],
			current: 'now-silent',
			nowPlayingNames: true,
			announceNames: true,
			announceClaimsPlaying: false,
			audioCount: 0,
			play: 0,
			pause: 0,
			errors: []
		},
		actual,
		{ announced: announced.text, probe }
	);
});

test('LIBPLAYNOW-C05 rejected play() stays selected-but-paused with truthful feedback and no retry', async ({
	page,
	baseURL
}, info) => {
	const { errors } = await seed(page, baseURL!, { mode: 'reject' });
	for (const id of ['now-coral', 'now-pine']) await rowSelectButton(page, id).click();
	await resetProbe(page);
	const control = playButton(page, 'now-ember', 'Ember Noon');
	const playControl = await control.count();
	if (playControl === 1) await control.click();
	await settle(page);
	const promptly = await readProbe(page);
	await page.waitForTimeout(500);
	const settled = await readProbe(page);
	const announced = await announcement(page);
	const audio = await audioState(page);
	const actual = {
		playControl,
		order: await queueOrder(page),
		current: await currentId(page),
		src: audio.src,
		audioCount: audio.count,
		autoplay: audio.autoplay,
		playPromptly: promptly.play,
		playSettled: settled.play,
		pauseSettled: settled.pause,
		announceNames: announced.text.includes('Ember Noon'),
		announceTruthfulFailure: FAILURE_PATTERN.test(announced.text),
		announceClaimsPlaying: PLAYING_PATTERN.test(announced.text),
		announceConcise: announced.text.length >= 1 && announced.text.length <= 140,
		announceRole: announced.role,
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			order: ['now-coral', 'now-ember', 'now-pine'],
			current: 'now-ember',
			src: '/__acceptance_audio__/ember.wav',
			audioCount: 1,
			autoplay: false,
			playPromptly: 1,
			playSettled: 1,
			pauseSettled: 0,
			announceNames: true,
			announceTruthfulFailure: true,
			announceClaimsPlaying: false,
			announceConcise: true,
			announceRole: 'status',
			errors: []
		},
		actual,
		{ announced: announced.text, promptly, settled }
	);
});

// Guard (GREEN before and after): lifecycle paths never start audio.
test('LIBPLAYNOW-C06 reload, refresh, restore and auth change never start audio', async ({
	page,
	baseURL
}, info) => {
	const { errors, payload } = await seed(page, baseURL!);
	await rowSelectButton(page, 'now-coral').click();
	await rowSelectButton(page, 'now-pine').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await resetProbe(page);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	const afterRefresh = await readProbe(page).then((probe) => ({
		play: probe.play,
		pause: probe.pause,
		load: probe.load
	}));
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	const afterReload = await readProbe(page).then((probe) => ({
		play: probe.play,
		pause: probe.pause,
		load: probe.load
	}));
	await rowSelectButton(page, 'now-coral').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await page.evaluate(
		(key) => window.dispatchEvent(new StorageEvent('storage', { key, newValue: 'changed' })),
		AUTH_TRANSITION_KEY
	);
	await expect(page.getByRole('alert')).toContainText(/session changed|signed out|do not have access/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	// Media silence only: the refresher legitimately persists library
	// snapshots on these paths, so storage writes are not asserted here.
	const afterAuth = await readProbe(page).then((probe) => ({
		play: probe.play,
		pause: probe.pause,
		load: probe.load
	}));
	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	const afterRetry = await readProbe(page).then((probe) => ({
		play: probe.play,
		pause: probe.pause,
		load: probe.load
	}));
	const actual = {
		afterRefresh,
		afterReload,
		afterAuth,
		afterRetry,
		queueAfterRetry: await queueOrder(page),
		errors
	};
	const silent = { play: 0, pause: 0, load: 0 };
	await check(
		info,
		{
			afterRefresh: silent,
			afterReload: silent,
			afterAuth: silent,
			afterRetry: silent,
			queueAfterRetry: [],
			errors: []
		},
		actual,
		{}
	);
});

test('LIBPLAYNOW-C07 row removed before activation leaves queue, playback and feedback untouched', async ({
	page,
	baseURL
}, info) => {
	const { errors, payload } = await seed(page, baseURL!);
	await rowSelectButton(page, 'now-coral').click();
	const control = playButton(page, 'now-dusk', 'Dusk Song');
	const playControl = await control.count();
	const staleHandle = playControl === 1 ? await control.elementHandle() : null;
	payload.current = ITEMS.filter((item) => item.id !== 'now-dusk');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - 1);
	const before = await announcement(page);
	await resetProbe(page);
	if (staleHandle) await staleHandle.evaluate((button) => (button as HTMLButtonElement).click());
	await settle(page);
	const after = await announcement(page);
	const probe = await readProbe(page);
	const live = await liveOutcomePaths(page, 'Dusk Song');
	const actual = {
		playControl,
		rowGone: (await playButton(page, 'now-dusk', 'Dusk Song').count()) === 0,
		order: await queueOrder(page),
		current: await currentId(page),
		play: probe.play,
		announcementUnchanged: after.text === before.text,
		livePaths: live.count,
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			rowGone: true,
			order: ['now-coral'],
			current: 'now-coral',
			play: 0,
			announcementUnchanged: true,
			livePaths: 0,
			errors: []
		},
		actual,
		{ before: before.text, after: after.text, probe }
	);
});

test('LIBPLAYNOW-C08 play preserves favorites, subset, find, order, view and share state', async ({
	page,
	baseURL
}, info) => {
	const { errors } = await seed(page, baseURL!);
	for (const id of ['now-coral', 'now-pine', 'now-ember']) await rowSelectButton(page, id).click();
	for (const [id, title] of [['now-pine', 'Pine Morning'], ['now-dusk', 'Dusk Song']] as const) {
		await libraryRow(page, id).getByRole('button', { name: `Star ${title}`, exact: true }).click();
	}
	for (const id of ['now-coral', 'now-ember']) await libraryRow(page, id).locator('input.nt-library-select').check();
	await page.locator('#library-order').selectOption('artist');
	await page.locator('#library-find-scope').selectOption('title');
	await page.locator('#library-find').fill('o');
	// Title find for 'o' keeps every fixture except the streamless sketch.
	await expect(page.getByTestId('library-item')).toHaveCount(4);
	const snapshot = {
		favorites: await page.getByTestId('favorite-item').evaluateAll((items) => items.map((item) => item.getAttribute('data-id'))),
		checked: await page.locator('input.nt-library-select:checked').count(),
		selectionCount: ((await page.getByTestId('selection-count').textContent()) ?? '').replace(/\s+/g, ' ').trim(),
		order: await page.locator('#library-order').inputValue(),
		scope: await page.locator('#library-find-scope').inputValue(),
		find: await page.locator('#library-find').inputValue(),
		view: await page.locator('#library-view').inputValue(),
		shareStatus: ((await page.getByTestId('library-share-status').textContent()) ?? '').replace(/\s+/g, ' ').trim(),
		shareFallback: await page.getByTestId('library-share-fallback').count(),
		staleBadges: await page.getByTestId('library-stale-badge').count(),
		appendResults: await page.getByTestId('append-result').count()
	};
	const control = playButton(page, 'now-dusk', 'Dusk Song');
	const playControl = await control.count();
	await resetProbe(page);
	if (playControl === 1) await control.click();
	await settle(page);
	const announced = await announcement(page);
	const probe = await readProbe(page);
	const live = await liveOutcomePaths(page, 'Dusk Song');
	const actual = {
		playControl,
		order: await queueOrder(page),
		current: await currentId(page),
		favorites: await page.getByTestId('favorite-item').evaluateAll((items) => items.map((item) => item.getAttribute('data-id'))),
		checked: await page.locator('input.nt-library-select:checked').count(),
		selectionCount: ((await page.getByTestId('selection-count').textContent()) ?? '').replace(/\s+/g, ' ').trim(),
		orderValue: await page.locator('#library-order').inputValue(),
		scopeValue: await page.locator('#library-find-scope').inputValue(),
		findValue: await page.locator('#library-find').inputValue(),
		viewValue: await page.locator('#library-view').inputValue(),
		shareStatus: ((await page.getByTestId('library-share-status').textContent()) ?? '').replace(/\s+/g, ' ').trim(),
		shareFallback: await page.getByTestId('library-share-fallback').count(),
		staleBadges: await page.getByTestId('library-stale-badge').count(),
		appendResults: await page.getByTestId('append-result').count(),
		announceNames: announced.text.includes('Dusk Song'),
		announcePlaying: PLAYING_PATTERN.test(announced.text),
		announceConcise: announced.text.length >= 1 && announced.text.length <= 140,
		livePaths: live.count,
		play: probe.play,
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			order: ['now-coral', 'now-dusk', 'now-pine', 'now-ember'],
			current: 'now-dusk',
			favorites: snapshot.favorites,
			checked: snapshot.checked,
			selectionCount: snapshot.selectionCount,
			orderValue: snapshot.order,
			scopeValue: snapshot.scope,
			findValue: snapshot.find,
			viewValue: snapshot.view,
			shareStatus: snapshot.shareStatus,
			shareFallback: snapshot.shareFallback,
			staleBadges: snapshot.staleBadges,
			appendResults: snapshot.appendResults,
			announceNames: true,
			announcePlaying: true,
			announceConcise: true,
			livePaths: 1,
			play: 1,
			errors: []
		},
		actual,
		{ announced: announced.text, snapshot, live: live.paths, probe }
	);
});

test('LIBPLAYNOW-C09 auth teardown fences stale handles and the next account starts clean', async ({
	page,
	baseURL
}, info) => {
	const { errors, payload } = await seed(page, baseURL!);
	await rowSelectButton(page, 'now-coral').click();
	const control = playButton(page, 'now-dusk', 'Dusk Song');
	const playControl = await control.count();
	const staleHandle = playControl === 1 ? await control.elementHandle() : null;
	await page.evaluate(
		(key) => window.dispatchEvent(new StorageEvent('storage', { key, newValue: 'changed' })),
		AUTH_TRANSITION_KEY
	);
	await expect(page.getByRole('alert')).toContainText(/session changed|signed out|do not have access/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await resetProbe(page);
	if (staleHandle) await staleHandle.evaluate((button) => (button as HTMLButtonElement).click());
	await settle(page);
	const afterStale = { order: await queueOrder(page), play: (await readProbe(page)).play };
	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	const probe = await readProbe(page);
	const actual = {
		playControl,
		staleOrder: afterStale.order,
		stalePlay: afterStale.play,
		nextAccountOrder: await queueOrder(page),
		nextAccountPlay: await playButton(page, 'now-bravo', 'Bravo Song').count(),
		staleControlGone: (await playButton(page, 'now-dusk', 'Dusk Song').count()) === 0,
		play: probe.play,
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			staleOrder: [],
			stalePlay: 0,
			nextAccountOrder: [],
			nextAccountPlay: 1,
			staleControlGone: true,
			play: 0,
			errors: []
		},
		actual,
		{ probe }
	);
});

test('LIBPLAYNOW-C10 keyboard Enter and Space on Play match click without focus theft', async ({
	page,
	baseURL
}, info) => {
	const { errors } = await seed(page, baseURL!);
	await rowSelectButton(page, 'now-coral').click();
	const first = playButton(page, 'now-ember', 'Ember Noon');
	const second = playButton(page, 'now-coral', 'Coral Night');
	const playControl = (await first.count()) === 1 && (await second.count()) === 1 ? 1 : 0;
	await resetProbe(page);
	if (playControl === 1) {
		await first.focus();
		await page.keyboard.press('Enter');
		await settle(page);
	}
	const midpoint = {
		order: await queueOrder(page),
		current: await currentId(page),
		focused: playControl === 1 ? await first.evaluate((element) => element === document.activeElement) : false
	};
	if (playControl === 1) {
		await second.focus();
		await page.keyboard.press('Space');
		await settle(page);
	}
	const probe = await readProbe(page);
	const announced = await announcement(page);
	const actual = {
		playControl,
		midpoint,
		order: await queueOrder(page),
		current: await currentId(page),
		focused: playControl === 1 ? await second.evaluate((element) => element === document.activeElement) : false,
		announceNames: announced.text.includes('Coral Night'),
		play: probe.play,
		pause: probe.pause,
		errors
	};
	await check(
		info,
		{
			playControl: 1,
			midpoint: { order: ['now-coral', 'now-ember'], current: 'now-ember', focused: true },
			order: ['now-coral', 'now-ember'],
			current: 'now-coral',
			focused: true,
			announceNames: true,
			play: 2,
			pause: 0,
			errors: []
		},
		actual,
		{ announced: announced.text, probe }
	);
});
