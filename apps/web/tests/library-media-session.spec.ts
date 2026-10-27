import { expect, test as base, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-media-session/mount.js';

/**
 * LIBSESSION — library native player through system media controls, tests-only handoff.
 *
 * Protected: this spec + tests/fixtures/library-media-session/*.
 * Builder-owned implementation: src/routes/library/+page.svelte and, if needed,
 * src/lib/player/*.ts via the existing injectable adapter (applyMediaSession,
 * clearMediaSession, setMediaSessionPosition, buildMediaMetadataInit). Existing
 * suites remain unchanged. Continue the handoff branch; open a combined GREEN PR only.
 *
 * Premise: the library mounts a REAL native audio[controls] only while the
 * queue's current track resolves to a non-blank stream URL (resolveStreamUrl).
 * Real /library row selection stores streamUrl:'' (metadata-only, nonplayable);
 * a test-only Vite pre-transform adds ONE exported setter calling the page's
 * existing replaceQueue. We mount the REAL page and feed it playable queue
 * states via the real selectTrack/moveTrack/restoreQueue/clearQueue reducers
 * plus the real same-window auth-transition notifier and Svelte unmount. No
 * readout, stream derivation, markup, transport or announcement is replaced.
 * The OS Media Session boundary is faked (injectable adapter seam); no
 * physical lock-screen or OS-hardware presentation is claimed.
 *
 * Wiring contract under test (Builder):
 * - Wire the existing injectable adapter ONLY while the real-library native
 *   audio element is current. Publish safe title/artist identity plus the
 *   existing placeholder artwork fallback only; never leak track/source URLs,
 *   grants, provider/account identifiers or tokens into OS metadata. No new
 *   remote artwork request.
 * - Supported play/pause act ONLY on the current matching playable element,
 *   play ONLY on an explicit supported OS play action; rejected play promises
 *   are handled truthfully (no uncaught error, native controls intact).
 *   Next/previous use existing library selection semantics (nextTrack /
 *   previousTrack, clamped, metadata-only), never auto-play or fetch/renew a
 *   stream. Never claim metadata-only selection became audible playback.
 * - Report actual current media duration/position/rate through the canonical
 *   helper where finite and valid; invalid clears. Handle absent API,
 *   unsupported single actions and thrown metadata/position calls in isolation
 *   without losing other actions or native controls. Do NOT register seek
 *   handlers (no opt into seeking); reuse helper behavior, keep demo contracts.
 * - Clear metadata, position and handlers on source/queue teardown,
 *   sign-out/account transition and library unmount. Fence stale callbacks by
 *   element/source/auth generation. Never clear a newer mounted consumer's
 *   session, never create an app-global player store, never keep media alive
 *   across routes.
 *
 * Setup (cached packages/browser required for offline use):
 *   pnpm --dir apps/web install --offline --frozen-lockfile
 * Run (CI=1 prevents reuse of another worktree's preview):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-media-session.spec.ts --workers=1 --retries=0
 * Full gates (from apps/web): pnpm check, pnpm test (full Vitest), then the
 * scoped browser command above.
 * Regression (must stay GREEN): demo Media Session suites —
 *   pnpm --dir apps/web exec vitest run src/lib/player/mediaSession
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/media-session-artwork-errors.spec.ts tests/player-system-seek.spec.ts \
 *     --workers=1 --retries=0
 *   plus tests/library-native-now-playing.spec.ts and
 *   tests/library-player-queue.spec.ts for library player health.
 *
 * Every case emits/attaches compact JSON expected/actual/status/error_code/
 * next_action. Missing wiring is assertion-RED via check(), never a click
 * timeout/import failure: harness waits (library rows, bridge, audio presence)
 * exist on baseline; feature probes (metadata, handlers, positions) are read
 * without polling and asserted via check(). LIBSESSION-08/11/17/18 are
 * already-correct controls (baseline GREEN is correct); core wiring cases must
 * be RED on base. Never fabricate guard RED.
 * Breaks caught: metadata leak (URL/id/token), remote artwork fetch, auto-play
 * on publish/selection, play on stale/detached element, unhandled play
 * rejection, next/previous auto-play or stream fetch, stale position, one bad
 * action/metadata/position call killing transport, seek opt-in, teardown/auth/
 * unmount leaks, stale callbacks resurrecting playback, over-clearing a newer
 * consumer, global store, media alive across routes, narrow/theme breakage.
 */

const routePath = decodeURIComponent(
	new URL('../src/routes/library/+page.svelte', import.meta.url).pathname
);
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const test = base.extend<{ libraryOrigin: string }>({
	libraryOrigin: [
		async ({}, use) => {
			const server = await createServer({
				configFile: false,
				root: decodeURIComponent(
					new URL('./fixtures/library-media-session/', import.meta.url).pathname
				),
				publicDir: false,
				resolve: {
					alias: {
						'#lib': `${webRoot}src/lib`,
						'$app/navigation': decodeURIComponent(
							new URL('./fixtures/library-media-session/navigation.ts', import.meta.url)
								.pathname
						)
					}
				},
				plugins: [
					{
						name: 'acceptance-library-queue-input',
						enforce: 'pre',
						transform(code, id) {
							if (id !== routePath) return;
							if (!code.includes('</script>'))
								throw new Error('library script boundary missing');
							return code.replace(
								'</script>',
								'\nexport function acceptanceReplaceQueue(next: QueueState) { replaceQueue(next); }\n</script>'
							);
						}
					},
					svelte({ configFile: false, compilerOptions: { runes: true } })
				],
				server: { host: '127.0.0.1', port: 0, fs: { allow: [webRoot] } }
			});
			try {
				await server.listen();
				const address = server.httpServer?.address();
				if (!address || typeof address === 'string') throw new Error('no fixture TCP address');
				await use(`http://127.0.0.1:${address.port}`);
			} finally {
				await server.close();
			}
		},
		{ scope: 'worker' }
	]
});

test.setTimeout(20_000);
test.use({ viewport: { width: 1000, height: 740 } });

const TRACKS: Track[] = [
	{
		id: 'ls-coral',
		title: 'Coral Night',
		artist: 'Mira North',
		streamUrl: '/__acceptance_audio__/coral.wav'
	},
	{
		id: 'ls-pine',
		title: 'Pine Morning',
		artist: 'South Ensemble',
		streamUrl: '/__acceptance_audio__/pine.wav'
	},
	{
		id: 'ls-ember',
		title: 'Ember Noon',
		artist: 'West Quartet',
		streamUrl: '/__acceptance_audio__/ember.wav'
	}
];
const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const readout = (page: Page) => page.getByTestId('library-now-playing');
const audio = (page: Page) => player(page).locator('audio[controls]');

type Position = { duration: number; position: number; playbackRate: number };
type Details = { action: string; seekTime?: unknown; seekOffset?: unknown };
type Callback = (details?: Details) => void;
type Probe = {
	handlers: Record<string, Callback | null | undefined>;
	attempts: string[];
	positions: (Position | null)[];
	metadataInits: Record<string, unknown>[];
	playCalls: number;
	pauseCalls: number;
	playSrcs: string[];
	pauseSrcs: string[];
};
declare global {
	// eslint-disable-next-line no-unused-vars
	interface Window {
		__libSessionAcceptance: Probe;
		__libSessionNewerPlayCalled?: boolean;
	}
}
type Mode = {
	rejectAction?: string;
	positionApi?: 'missing' | 'throwing';
	noSession?: boolean;
	metadataCtor?: 'missing' | 'throwing';
	playBehavior?: 'resolve' | 'reject';
};

async function openLibrary(page: Page, origin: string, mode: Mode = {}) {
	const errors: string[] = [];
	const requestedUrls: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	const expectedOrigin = new URL(origin).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const target = new URL(request.url());
		requestedUrls.push(request.url());
		if (target.origin !== expectedOrigin || request.resourceType() === 'media')
			return route.abort();
		if (target.pathname === '/api/library' && request.method() === 'GET') {
			return route.fulfill({
				status: 200,
				json: TRACKS.map(({ id, title, artist }) => ({ id, title, artist }))
			});
		}
		if (/^\/(?:api|v1|__acceptance_audio__)(?:\/|$)/.test(target.pathname))
			return route.abort();
		return route.continue();
	});
	await page.addInitScript((config: Mode) => {
		const probe: Probe = {
			handlers: {},
			attempts: [],
			positions: [],
			metadataInits: [],
			playCalls: 0,
			pauseCalls: 0,
			playSrcs: [],
			pauseSrcs: []
		};
		(window as unknown as { __libSessionAcceptance: Probe }).__libSessionAcceptance = probe;
		const nativePause = HTMLMediaElement.prototype.pause;
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			try {
				probe.playSrcs.push((this as HTMLMediaElement).currentSrc || (this as HTMLMediaElement).src);
			} catch {
				probe.playSrcs.push('');
			}
			if (config.playBehavior === 'reject')
				return Promise.reject(new DOMException('synthetic play rejection', 'NotAllowedError'));
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauseCalls += 1;
			try {
				probe.pauseSrcs.push(
					(this as HTMLMediaElement).currentSrc || (this as HTMLMediaElement).src
				);
			} catch {
				probe.pauseSrcs.push('');
			}
			try {
				nativePause.call(this);
			} catch {
				// Aborted synthetic media may reject native pause; the call count is the point.
			}
		};
		if (config.noSession) {
			let owner: object | null = navigator;
			while (owner && !Object.prototype.hasOwnProperty.call(owner, 'mediaSession')) {
				owner = Object.getPrototypeOf(owner);
			}
			if (owner) Reflect.deleteProperty(owner, 'mediaSession');
			return;
		}
		const session = {
			metadata: null as unknown,
			setActionHandler(action: string, callback: Callback | null) {
				probe.attempts.push(action);
				if (config.rejectAction === action)
					throw new DOMException('unsupported', 'NotSupportedError');
				probe.handlers[action] = callback;
			},
			...(config.positionApi === 'missing'
				? {}
				: {
						setPositionState(state?: Position) {
							probe.positions.push(
								state && Object.keys(state).length > 0 ? { ...state } : null
							);
							if (config.positionApi === 'throwing')
								throw new DOMException('unsupported', 'NotSupportedError');
						}
					})
		};
		Object.defineProperty(navigator, 'mediaSession', { configurable: true, value: session });
		if (config.metadataCtor === 'missing') {
			try {
				Object.defineProperty(window, 'MediaMetadata', {
					configurable: true,
					value: undefined
				});
			} catch {
				// If the constructor cannot be hidden, Builder must still survive null.
			}
		} else if (config.metadataCtor === 'throwing') {
			(window as unknown as { MediaMetadata: unknown }).MediaMetadata = class {
				constructor(init: Record<string, unknown>) {
					probe.metadataInits.push({ ...(init ?? {}) });
					throw new Error('synthetic MediaMetadata failure');
				}
			};
		} else {
			(window as unknown as { MediaMetadata: unknown }).MediaMetadata = class {
				title: unknown;
				artist: unknown;
				album: unknown;
				artwork: unknown;
				constructor(init: Record<string, unknown>) {
					const copy = { ...(init ?? {}) };
					if (Array.isArray(copy['artwork']))
						copy['artwork'] = (copy['artwork'] as unknown[]).map((entry) => ({ ...(entry as object) }));
					probe.metadataInits.push(copy);
					this.title = copy['title'] ?? '';
					this.artist = copy['artist'] ?? '';
					this.album = copy['album'] ?? '';
					this.artwork = copy['artwork'] ?? [];
				}
			};
		}
	}, mode);
	await page.goto(origin);
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect
		.poll(() => page.evaluate(() => Boolean(window.libraryMediaSession)))
		.toBe(true);
	return { errors, requestedUrls };
}

async function settle(page: Page) {
	await page.evaluate(
		() =>
			new Promise<void>((resolve) =>
				requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
			)
	);
}

async function replace(page: Page, next: QueueState = state()) {
	await page.evaluate((value) => window.libraryMediaSession.replace(value), next);
	await settle(page);
}

async function probe(page: Page): Promise<Omit<Probe, 'handlers'>> {
	return page.evaluate(() => {
		const found = (window as unknown as { __libSessionAcceptance?: Probe })
			.__libSessionAcceptance;
		if (!found)
			return {
				attempts: [],
				positions: [],
				metadataInits: [],
				playCalls: 0,
				pauseCalls: 0,
				playSrcs: [],
				pauseSrcs: []
			};
		// Handlers are functions and do not survive evaluate serialization;
		// read their types via handlerType() instead.
		return {
			attempts: [...found.attempts],
			positions: found.positions.map((entry) => (entry ? { ...entry } : null)),
			metadataInits: found.metadataInits.map((entry) => ({ ...entry })),
			playCalls: found.playCalls,
			pauseCalls: found.pauseCalls,
			playSrcs: [...found.playSrcs],
			pauseSrcs: [...found.pauseSrcs]
		};
	});
}

async function handlerType(page: Page, action: string): Promise<string> {
	return page.evaluate((name) => {
		const found = (window as unknown as { __libSessionAcceptance?: Probe })
			.__libSessionAcceptance;
		const handler = found?.handlers[name];
		if (handler === undefined) return 'missing';
		if (handler === null) return 'null';
		return typeof handler;
	}, action);
}

/** Dispatch an OS handler; false when missing/null (assertion data, never a throw). */
async function dispatch(page: Page, action: string, details: Details = { action }) {
	return page.evaluate((input) => {
		const found = (window as unknown as { __libSessionAcceptance?: Probe })
			.__libSessionAcceptance;
		const handler = found?.handlers[input.action];
		if (typeof handler !== 'function') return false;
		handler(input);
		return true;
	}, details);
}

async function sessionMetadataTitle(page: Page): Promise<string | null> {
	return page.evaluate(() => {
		const session = (navigator as unknown as { mediaSession?: { metadata?: { title?: unknown } | null } | null })
			.mediaSession;
		if (!session) return null;
		const title = session.metadata?.title;
		return typeof title === 'string' ? title : title == null ? null : String(title);
	});
}

async function nowPlaying(page: Page): Promise<string> {
	return ((await page.getByTestId('now-playing').innerText()) ?? '').replace(/\s+/g, ' ').trim();
}

async function currentId(page: Page): Promise<string | null> {
	return page
		.locator('[data-testid="queue-item"][aria-current="true"]')
		.getAttribute('data-track-id');
}

async function check(testInfo: TestInfo, expected: unknown, actual: unknown) {
	let passed = false;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} finally {
		const receipt = {
			case_id: testInfo.title.split(' ')[0],
			expected,
			actual,
			status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : 'LIBRARY_MEDIA_SESSION_CONTRACT',
			next_action: passed
				? 'preserve acceptance'
				: 'implement library media-session wiring; do not edit protected tests'
		};
		const body = JSON.stringify(receipt);
		console.log(body);
		await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
	}
}

test.afterEach(async ({}, testInfo) => {
	if (testInfo.attachments.some((attachment) => attachment.name === 'acceptance-case.json'))
		return;
	const body = JSON.stringify({
		case_id: testInfo.title.split(' ')[0],
		expected: 'reach behavioral assertion',
		actual: testInfo.errors.map((error) => error.message),
		status: 'ERROR',
		error_code: 'HARNESS_OR_ACTION_FAILURE',
		next_action: 'repair harness/action; not verified feature RED'
	});
	console.log(body);
	await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

const FORBIDDEN_SUBSTRINGS = [
	'/__acceptance_audio__',
	'ls-coral',
	'ls-pine',
	'ls-ember',
	'token',
	'Bearer',
	'grant',
	'provider',
	'account'
];

function leaksIn(init: Record<string, unknown> | undefined): string[] {
	if (!init) return ['missing-init'];
	const text = JSON.stringify(init).toLowerCase();
	return FORBIDDEN_SUBSTRINGS.filter((needle) => text.includes(needle.toLowerCase()));
}

function remoteRequests(requestedUrls: string[], origin: string): string[] {
	const expected = new URL(origin).origin;
	return requestedUrls.filter((url) => {
		try {
			return new URL(url).origin !== expected;
		} catch {
			return true;
		}
	});
}

test('LIBSESSION-01 playable track publishes safe title, artist and placeholder artwork only', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors, requestedUrls } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/coral.wav');
	await expect(readout(page)).toContainText('Coral Night');
	const found = await probe(page);
	const last = found.metadataInits.at(-1);
	const artwork = Array.isArray(last?.['artwork'])
		? (last?.['artwork'] as Record<string, unknown>[])
		: [];
	const actual = {
		title: last?.['title'] ?? null,
		artist: last?.['artist'] ?? null,
		hasAlbum: Object.prototype.hasOwnProperty.call(last ?? {}, 'album'),
		artworkCount: artwork.length,
		artworkSrc: String(artwork[0]?.['src'] ?? ''),
		artworkLocalPlaceholder:
			String(artwork[0]?.['src'] ?? '').startsWith(new URL(libraryOrigin).origin) &&
			String(artwork[0]?.['src'] ?? '').endsWith('/placeholder-artwork.svg'),
		leaks: leaksIn(last),
		remoteRequests: remoteRequests(requestedUrls, libraryOrigin),
		pageErrors: errors
	};
	await check(
		info,
		{
			title: 'Coral Night',
			artist: 'Mira North',
			hasAlbum: false,
			artworkCount: 1,
			artworkSrc: `${new URL(libraryOrigin).origin}/placeholder-artwork.svg`,
			artworkLocalPlaceholder: true,
			leaks: [],
			remoteRequests: [],
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-02 OS play acts only on the current element after an explicit action', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/coral.wav');
	const before = await probe(page);
	const dispatched = await dispatch(page, 'play');
	await settle(page);
	const after = await probe(page);
	const actual = {
		autoPlayCalls: before.playCalls,
		dispatched,
		playCalls: after.playCalls,
		playedSrc: after.playSrcs.at(-1) ?? null,
		stillCurrent: await currentId(page),
		stillTitle: await sessionMetadataTitle(page),
		pageErrors: errors
	};
	await check(
		info,
		{
			autoPlayCalls: 0,
			dispatched: true,
			playCalls: 1,
			playedSrc: expect.stringContaining('/__acceptance_audio__/coral.wav'),
			stillCurrent: 'ls-coral',
			stillTitle: 'Coral Night',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-03 OS pause acts only on the current matching element', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/coral.wav');
	const dispatched = await dispatch(page, 'pause');
	await settle(page);
	const found = await probe(page);
	const actual = {
		dispatched,
		pauseCalls: found.pauseCalls,
		pausedSrc: found.pauseSrcs.at(-1) ?? null,
		playCalls: found.playCalls,
		stillCurrent: await currentId(page),
		pageErrors: errors
	};
	await check(
		info,
		{
			dispatched: true,
			pauseCalls: 1,
			pausedSrc: expect.stringContaining('/__acceptance_audio__/coral.wav'),
			playCalls: 0,
			stillCurrent: 'ls-coral',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-04 rejected OS play is handled truthfully with native controls intact', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin, { playBehavior: 'reject' });
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const dispatched = await dispatch(page, 'play');
	await settle(page);
	// A rejected play must settle before the next OS action; pause still works.
	const pauseDispatched = await dispatch(page, 'pause');
	await settle(page);
	const found = await probe(page);
	const actual = {
		dispatched,
		playCalls: found.playCalls,
		pauseDispatched,
		queueCount: await page.getByTestId('queue-item').count(),
		stillCurrent: await currentId(page),
		audioControls: await audio(page).count(),
		pageErrors: errors
	};
	await check(
		info,
		{
			dispatched: true,
			playCalls: 1,
			pauseDispatched: true,
			queueCount: 3,
			stillCurrent: 'ls-coral',
			audioControls: 1,
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-05 OS next follows library selection without auto-play or stream fetch', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors, requestedUrls } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/coral.wav');
	const apiBefore = requestedUrls.filter((url) => new URL(url).pathname.startsWith('/api/')).length;
	const dispatched = await dispatch(page, 'nexttrack');
	await settle(page);
	const found = await probe(page);
	const apiAfter = requestedUrls.filter((url) => new URL(url).pathname.startsWith('/api/')).length;
	const actual = {
		dispatched,
		nowPlaying: await nowPlaying(page),
		stillCurrent: await currentId(page),
		audioSrc: await player(page).locator('audio').getAttribute('src'),
		playCalls: found.playCalls,
		apiFetches: apiAfter - apiBefore,
		metadataTitle: await sessionMetadataTitle(page),
		pageErrors: errors
	};
	await check(
		info,
		{
			dispatched: true,
			nowPlaying: expect.stringContaining('Pine Morning'),
			stillCurrent: 'ls-pine',
			audioSrc: '/__acceptance_audio__/pine.wav',
			playCalls: 0,
			apiFetches: 0,
			metadataTitle: 'Pine Morning',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-06 OS previous follows library selection without auto-play or stream fetch', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors, requestedUrls } = await openLibrary(page, libraryOrigin);
	await replace(page, state(1));
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/pine.wav');
	const apiBefore = requestedUrls.filter((url) => new URL(url).pathname.startsWith('/api/')).length;
	const dispatched = await dispatch(page, 'previoustrack');
	await settle(page);
	const midCurrent = await currentId(page);
	const midTitle = await sessionMetadataTitle(page);
	const clamped = await dispatch(page, 'previoustrack');
	await settle(page);
	const found = await probe(page);
	const apiAfter = requestedUrls.filter((url) => new URL(url).pathname.startsWith('/api/')).length;
	const actual = {
		dispatched,
		movedTo: midCurrent,
		movedTitle: midTitle,
		clampedDispatched: clamped,
		clampedCurrent: await currentId(page),
		playCalls: found.playCalls,
		apiFetches: apiAfter - apiBefore,
		pageErrors: errors
	};
	await check(
		info,
		{
			dispatched: true,
			movedTo: 'ls-coral',
			movedTitle: 'Coral Night',
			clampedDispatched: true,
			clampedCurrent: 'ls-coral',
			playCalls: 0,
			apiFetches: 0,
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-07 position reports actual finite media state and clears when invalid', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	await page
		.locator('audio')
		.evaluate((el: HTMLAudioElement) => {
			Object.defineProperty(el, 'duration', { value: 120, configurable: true });
			el.currentTime = 10;
			el.playbackRate = 1.5;
			el.dispatchEvent(new Event('loadedmetadata'));
			el.dispatchEvent(new Event('timeupdate'));
			el.dispatchEvent(new Event('ratechange'));
		});
	await settle(page);
	const valid = await probe(page);
	await page.locator('audio').evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { value: Number.NaN, configurable: true });
		el.dispatchEvent(new Event('durationchange'));
	});
	await settle(page);
	const cleared = await probe(page);
	const actual = {
		published: valid.positions.at(-1),
		cleared: cleared.positions.at(-1),
		pageErrors: errors
	};
	await check(
		info,
		{
			published: { duration: 120, position: 10, playbackRate: 1.5 },
			cleared: null,
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-08 absent Media Session API preserves native controls and queue', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin, { noSession: true });
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/coral.wav');
	await page.getByTestId('player-next').click();
	await settle(page);
	const actual = {
		audio: await player(page).locator('audio[controls]').count(),
		nowPlaying: await nowPlaying(page),
		current: await currentId(page),
		pageErrors: errors
	};
	await check(
		info,
		{
			audio: 1,
			nowPlaying: expect.stringContaining('Pine Morning'),
			current: 'ls-pine',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-09 one unsupported action does not break the remaining transport', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin, { rejectAction: 'pause' });
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const play = await dispatch(page, 'play');
	const next = await dispatch(page, 'nexttrack');
	await settle(page);
	const previous = await dispatch(page, 'previoustrack');
	await settle(page);
	const found = await probe(page);
	const actual = {
		pauseType: await handlerType(page, 'pause'),
		playDispatched: play,
		nextDispatched: next,
		previousDispatched: previous,
		current: await currentId(page),
		playCalls: found.playCalls,
		metadataTitle: await sessionMetadataTitle(page),
		pageErrors: errors
	};
	await check(
		info,
		{
			pauseType: 'missing',
			playDispatched: true,
			nextDispatched: true,
			previousDispatched: true,
			current: 'ls-coral',
			playCalls: 1,
			metadataTitle: 'Coral Night',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-10 thrown metadata and position calls leave transport and queue intact', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin, {
		metadataCtor: 'throwing',
		positionApi: 'throwing'
	});
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const play = await dispatch(page, 'play');
	const next = await dispatch(page, 'nexttrack');
	await settle(page);
	const pause = await dispatch(page, 'pause');
	await settle(page);
	const found = await probe(page);
	const actual = {
		playDispatched: play,
		nextDispatched: next,
		pauseDispatched: pause,
		current: await currentId(page),
		playCalls: found.playCalls,
		pauseCalls: found.pauseCalls,
		queueCount: await page.getByTestId('queue-item').count(),
		pageErrors: errors
	};
	await check(
		info,
		{
			playDispatched: true,
			nextDispatched: true,
			pauseDispatched: true,
			current: 'ls-pine',
			playCalls: 1,
			pauseCalls: 1,
			queueCount: 3,
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-11 library does not register system seek handlers', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const found = await probe(page);
	const seekto = await handlerType(page, 'seekto');
	const seekforward = await handlerType(page, 'seekforward');
	const seekbackward = await handlerType(page, 'seekbackward');
	const actual = {
		seekWired: [seekto, seekforward, seekbackward].some((kind) => kind === 'function'),
		seekAttempts: found.attempts.filter((action) =>
			['seekto', 'seekforward', 'seekbackward'].includes(action)
		),
		playWired: await handlerType(page, 'play'),
		pageErrors: errors
	};
	await check(
		info,
		{
			seekWired: false,
			seekAttempts: [],
			playWired: 'function',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-12 queue clear and stream loss release metadata, position and handlers', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const wiredTitle = await sessionMetadataTitle(page);
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await expect(page.locator('audio')).toHaveCount(0);
	await settle(page);
	const afterClearTitle = await sessionMetadataTitle(page);
	const afterClear = await probe(page);
	const afterClearPlay = await handlerType(page, 'play');
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	await replace(page, state(0, [{ ...TRACKS[0]!, streamUrl: '   ' }]));
	await expect(page.locator('audio')).toHaveCount(0);
	await settle(page);
	const afterLossTitle = await sessionMetadataTitle(page);
	const afterLoss = await probe(page);
	const actual = {
		wiredTitle,
		afterClearTitle,
		afterClearPosition: afterClear.positions.at(-1) ?? null,
		afterClearPlay,
		afterLossTitle,
		afterLossPosition: afterLoss.positions.at(-1) ?? null,
		afterLossPlay: await handlerType(page, 'play'),
		pageErrors: errors
	};
	await check(
		info,
		{
			wiredTitle: 'Coral Night',
			afterClearTitle: null,
			afterClearPosition: null,
			afterClearPlay: 'null',
			afterLossTitle: null,
			afterLossPosition: null,
			afterLossPlay: 'null',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-13 auth transition clears the session and retires old callbacks', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const wiredTitle = await sessionMetadataTitle(page);
	const before = await probe(page);
	await page.evaluate(() => window.libraryMediaSession.authTransition());
	await expect(page.locator('audio')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await settle(page);
	const stalePlay = await dispatch(page, 'play');
	const staleNext = await dispatch(page, 'nexttrack');
	await settle(page);
	const after = await probe(page);
	const actual = {
		wiredTitle,
		clearedTitle: await sessionMetadataTitle(page),
		clearedPosition: after.positions.at(-1) ?? null,
		playHandler: await handlerType(page, 'play'),
		stalePlay,
		staleNext,
		playCallsDelta: after.playCalls - before.playCalls,
		pageErrors: errors
	};
	await check(
		info,
		{
			wiredTitle: 'Coral Night',
			clearedTitle: null,
			clearedPosition: null,
			playHandler: 'null',
			stalePlay: false,
			staleNext: false,
			playCallsDelta: 0,
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-14 library unmount releases the session without a global store', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const wiredTitle = await sessionMetadataTitle(page);
	const storageBefore = await page.evaluate(() => ({
		local: Object.keys(localStorage),
		session: Object.keys(sessionStorage)
	}));
	const before = await probe(page);
	await page.evaluate(() => window.libraryMediaSession.unmountLibrary());
	await settle(page);
	const storageAfter = await page.evaluate(() => ({
		local: (() => {
			try {
				return Object.keys(localStorage);
			} catch {
				return [];
			}
		})(),
		session: (() => {
			try {
				return Object.keys(sessionStorage);
			} catch {
				return [];
			}
		})()
	}));
	const after = await probe(page);
	const newKeys = [
		...storageAfter.local.filter((key) => !storageBefore.local.includes(key)),
		...storageAfter.session.filter((key) => !storageBefore.session.includes(key))
	].filter((key) => /player|queue|media|session/i.test(key));
	const actual = {
		wiredTitle,
		clearedTitle: await sessionMetadataTitle(page),
		clearedPosition: after.positions.at(-1) ?? null,
		playHandler: await handlerType(page, 'play'),
		audio: await page.locator('audio').count(),
		playCallsDelta: after.playCalls - before.playCalls,
		newPlayerKeys: newKeys,
		pageErrors: errors
	};
	await check(
		info,
		{
			wiredTitle: 'Coral Night',
			clearedTitle: null,
			clearedPosition: null,
			playHandler: 'null',
			audio: 0,
			playCallsDelta: 0,
			newPlayerKeys: [],
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-15 stale element and source callbacks cannot drive the new track', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/coral.wav');
	const wiredTitle = await sessionMetadataTitle(page);
	// Retain the Coral generation's callbacks, then swap to Pine. Late
	// invocations of the retained closures must be inert: no play on the
	// detached Coral element and no selection change away from Pine.
	await page.evaluate(() => {
		const found = (window as unknown as { __libSessionAcceptance: Probe }).__libSessionAcceptance;
		(window as unknown as { __libSessionStale?: object }).__libSessionStale = {
			play: found.handlers['play'],
			next: found.handlers['nexttrack']
		};
	});
	const stale = {
		play: await handlerType(page, 'play'),
		next: await handlerType(page, 'nexttrack')
	};
	await replace(page, state(1));
	await expect(audio(page)).toHaveAttribute('src', '/__acceptance_audio__/pine.wav');
	const currentTitle = await sessionMetadataTitle(page);
	const before = await probe(page);
	await page.evaluate(() => {
		const stale = (window as unknown as { __libSessionStale?: { play?: Callback; next?: Callback } })
			.__libSessionStale;
		try {
			stale?.play?.({ action: 'play' });
		} catch {
			// A fenced callback must never throw out of a stale invocation.
		}
		try {
			stale?.next?.({ action: 'nexttrack' });
		} catch {
			// Same for stale transport.
		}
	});
	await settle(page);
	const afterStale = await probe(page);
	const staleCurrent = await currentId(page);
	const dispatchedPlay = await dispatch(page, 'play');
	await settle(page);
	const afterPlay = await probe(page);
	const actual = {
		wiredTitle,
		stale,
		currentTitle,
		stalePlayDelta: afterStale.playCalls - before.playCalls,
		staleCurrent,
		dispatchedPlay,
		playedSrc: afterPlay.playSrcs.at(-1) ?? null,
		playCallsDelta: afterPlay.playCalls - afterStale.playCalls,
		stillCurrent: await currentId(page),
		pageErrors: errors
	};
	await check(
		info,
		{
			wiredTitle: 'Coral Night',
			stale: { play: 'function', next: 'function' },
			currentTitle: 'Pine Morning',
			stalePlayDelta: 0,
			staleCurrent: 'ls-pine',
			dispatchedPlay: true,
			playedSrc: expect.stringContaining('/__acceptance_audio__/pine.wav'),
			playCallsDelta: 1,
			stillCurrent: 'ls-pine',
			pageErrors: []
		},
		actual
	);
});

test('LIBSESSION-16 teardown never clears a newer mounted consumer session', async ({
	page,
	libraryOrigin
}, info) => {
	const { errors } = await openLibrary(page, libraryOrigin);
	await replace(page, state(0));
	await expect(audio(page)).toHaveCount(1);
	const wiredTitle = await sessionMetadataTitle(page);
	await page.evaluate((origin) => {
		const session = (navigator as unknown as { mediaSession: { metadata: unknown; setActionHandler(action: string, callback: (() => void) | null): void } })
			.mediaSession;
		const Ctor = (window as unknown as { MediaMetadata: new (init: object) => unknown })
			.MediaMetadata;
		session.metadata = new Ctor({
			title: 'Newer Track',
			artist: 'Newer Artist',
			artwork: [{ src: `${origin}/newer.svg`, sizes: '512x512' }]
		});
		session.setActionHandler('play', () => {
			window.__libSessionNewerPlayCalled = true;
		});
		session.setActionHandler('pause', () => {});
		session.setActionHandler('nexttrack', () => {});
		session.setActionHandler('previoustrack', () => {});
	}, new URL(libraryOrigin).origin);
	await settle(page);
	const newerTitle = await sessionMetadataTitle(page);
	await page.evaluate(() => window.libraryMediaSession.clear());
	await expect(page.locator('audio')).toHaveCount(0);
	await settle(page);
	const newerPlayWired = await page.evaluate(
		() =>
			typeof (window as unknown as { __libSessionAcceptance: Probe }).__libSessionAcceptance
				.handlers['play']
	);
	const actual = {
		wiredTitle,
		newerTitle,
		preservedTitle: await sessionMetadataTitle(page),
		newerPlayWired,
		pageErrors: errors
	};
	await check(
		info,
		{
			wiredTitle: 'Coral Night',
			newerTitle: 'Newer Track',
			preservedTitle: 'Newer Track',
			newerPlayWired: 'function',
			pageErrors: []
		},
		actual
	);
});

for (const [index, theme] of ['light', 'dark'].entries()) {
	test(`LIBSESSION-${17 + index} ${theme} narrow player stays readable with no stray status`, async ({
		page,
		libraryOrigin
	}, info) => {
		const { errors } = await openLibrary(page, libraryOrigin);
		await page.setViewportSize({ width: 360, height: 740 });
		await page.emulateMedia({ colorScheme: theme as 'light' | 'dark' });
		await page.evaluate(
			(value) => document.documentElement.setAttribute('data-appearance', value),
			theme
		);
		await replace(
			page,
			state(0, [
				{
					...TRACKS[0]!,
					title: 'Coral Night '.repeat(12).trim(),
					artist: 'Mira North '.repeat(8).trim()
				}
			])
		);
		await expect(audio(page)).toHaveCount(1);
		const layout = await readout(page).count() === 1
			? await readout(page).evaluate((node) => {
					const box = node.getBoundingClientRect();
					const rgb = (value: string) => value.match(/[\d.]+/g)?.map(Number) ?? [];
					const luminance = (color: number[]) =>
						color
							.slice(0, 3)
							.map((n) => {
								const c = n / 255;
								return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
							})
							.reduce((sum, value, i) => sum + value * [0.2126, 0.7152, 0.0722][i]!, 0);
					let background: number[] = [];
					for (
						let element: Element | null = node;
						element;
						element = element.parentElement
					) {
						const value = rgb(getComputedStyle(element).backgroundColor);
						if (value.length === 3 || value[3] === 1) {
							background = value;
							break;
						}
					}
					const foreground = rgb(getComputedStyle(node).color);
					const values = [luminance(foreground), luminance(background)].sort(
						(a, b) => b - a
					);
					return {
						contained:
							box.x >= 0 &&
							box.right <= innerWidth + 1 &&
							document.documentElement.scrollWidth <= innerWidth,
						readable:
							background.length >= 3 && (values[0]! + 0.05) / (values[1]! + 0.05) >= 4.5
					};
				})
			: { contained: false, readable: false };
		const stray = await player(page)
			.locator('[data-testid*="media"], [data-testid*="session"], [data-testid*="system"]')
			.count();
		const actual = {
			identity: (await readout(page).innerText()).includes('Coral Night'),
			layout,
			stray,
			pageErrors: errors
		};
		await check(
			info,
			{ identity: true, layout: { contained: true, readable: true }, stray: 0, pageErrors: [] },
			actual
		);
	});
}
