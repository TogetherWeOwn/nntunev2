import { expect, test as base, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type {} from './fixtures/library-now-playing/mount.js';

/**
 * Native library audio recovery acceptance. Tests only; no transport or grants.
 * Protected: this file. Existing library/share/queue/demo banks stay unchanged.
 *
 * Observable contract: the Player region keeps audio[controls], adds a native
 * button named "Reload audio" after a current media failure, and scopes one
 * data-testid="library-audio-status" polite status to that region. The status
 * uses generic failure wording, not MediaError diagnostics. Exact prose is free.
 * A reload request is not readiness or playback success. The action remains
 * usable without waiting for a browser event, including when load() throws.
 *
 * Boundary doubles replace only native load/play and media readiness/error
 * inputs. load() is synchronous; no Promise is invented for it. Native source
 * assignment, controls, seeking, page handlers and queue reducers remain real.
 * Pending local media avoids unrelated automatic network-error events.
 *
 * Most cases use the real /library route and existing synthetic streamUrl seam
 * from library-share-link.spec.ts. LAR-14 reuses the existing now-playing mount
 * and queue input seam to replace a source without inventing source-refresh UI.
 * No recovery handler, markup or state derivation is replaced by that seam.
 *
 * Run from repository root, with cached packages and supported Chromium libs:
 *   pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-native-audio-recovery.spec.ts --workers=1 --retries=0
 * Port 4173 must be free. CI=1 forbids reusing another worktree's preview.
 * Browser launch/import/action failures are ERROR, never behavioral RED.
 * Each case attaches compact case_id/expected/actual/status/error_code/next_action.
 */

const ITEMS = [
	{ id: 'recovery-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '/test-audio/recovery-coral?token=synthetic-media-marker' },
	{ id: 'recovery-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '/test-audio/recovery-pine' },
	{ id: 'recovery-plain', title: 'Metadata Only' }
];
const RAW_ERROR = 'SyntheticDecoder provider-detail token=synthetic-media-marker /test-audio/private?signature=synthetic-signature';
const FAILURE = /(?:fail|unavailable|unable|could not|couldn.t|can.t|problem|error)/i;
const SUCCESS = /(?:playing|playback (?:started|restored|successful)|audio (?:ready|reloaded successfully)|successfully reloaded)/i;
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const audio = (page: Page) => page.getByTestId('audio-player');
const reload = (page: Page) => player(page).getByRole('button', { name: 'Reload audio', exact: true });
const status = (page: Page) => player(page).getByTestId('library-audio-status');

interface MediaProbe {
	loads: string[];
	loadCurrent: boolean[];
	plays: string[];
	pauses: number;
	writes: string[];
	throwLoad: boolean;
	previous: HTMLAudioElement | null;
}
declare global {
	interface Window { __lar: MediaProbe }
}

// The canonical mount exports queue input, not recovery behavior. The helper is
// file-local in its original spec, so only the minimal Vite setup is repeated.
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const routePath = `${webRoot}src/routes/library/+page.svelte`;
const test = base.extend<{}, { queueOrigin: string }>({
	queueOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: `${webRoot}tests/fixtures/library-now-playing`,
			publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': `${webRoot}tests/fixtures/library-now-playing/navigation.ts`
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

test.setTimeout(25_000);
test.use({ viewport: { width: 1000, height: 740 } });

type LibraryPayload = { items: typeof ITEMS; status: number };
async function openLibrary(page: Page, options: { url?: string; origin?: string } = {}) {
	const payload: LibraryPayload = { items: ITEMS, status: 200 };
	const requests: string[] = [];
	const forbidden: string[] = [];
	const origin = options.origin ?? test.info().project.use.baseURL ?? 'http://localhost:4173';
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.context().route('**/*', (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== new URL(origin).origin) {
			forbidden.push(`${request.method()} ${target.origin}${target.pathname}`);
			return route.abort();
		}
		if (target.pathname === '/api/library' && request.method() === 'GET') {
			requests.push('GET /api/library');
			return route.fulfill({ status: payload.status, json: payload.items });
		}
		if (target.pathname.startsWith('/test-audio/') && request.method() === 'GET') {
			requests.push(`GET ${target.pathname}`);
			return; // Intentional pending fixture, never a real media/provider fetch.
		}
		if (/^\/(?:api|v1)(?:\/|$)/.test(target.pathname) || request.method() !== 'GET') {
			forbidden.push(`${request.method()} ${target.pathname}`);
			return route.abort();
		}
		return route.continue();
	});
	await page.addInitScript(() => {
		window.__lar = { loads: [], loadCurrent: [], plays: [], pauses: 0, writes: [], throwLoad: false, previous: null };
		HTMLMediaElement.prototype.load = function () {
			window.__lar.loads.push(this.getAttribute('src') ?? '');
			window.__lar.loadCurrent.push(this === document.querySelector('[data-testid="audio-player"]'));
			if (window.__lar.throwLoad) throw new DOMException('Synthetic load refusal, token=synthetic-load-marker', 'NotSupportedError');
		};
		HTMLMediaElement.prototype.play = function () {
			window.__lar.plays.push(this.getAttribute('src') ?? '');
			return Promise.resolve();
		};
		const pause = HTMLMediaElement.prototype.pause;
		HTMLMediaElement.prototype.pause = function () {
			window.__lar.pauses += 1;
			pause.call(this);
		};
		Object.defineProperty(HTMLMediaElement.prototype, 'duration', { configurable: true, get: () => 120 });
		const setItem = Storage.prototype.setItem;
		const removeItem = Storage.prototype.removeItem;
		const clear = Storage.prototype.clear;
		Storage.prototype.setItem = function (key, value) {
			window.__lar.writes.push(`set:${key}`);
			setItem.call(this, key, value);
		};
		Storage.prototype.removeItem = function (key) {
			window.__lar.writes.push(`remove:${key}`);
			removeItem.call(this, key);
		};
		Storage.prototype.clear = function () {
			window.__lar.writes.push('clear');
			clear.call(this);
		};
	});
	await page.goto(new URL(options.url ?? '/library', origin).href);
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	return { payload, requests, forbidden };
}

async function select(page: Page, id = 'recovery-coral') {
	await page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`).click();
	await settle(page);
}
async function start(page: Page, options: { url?: string; origin?: string } = {}) {
	const harness = await openLibrary(page, options);
	if (!options.url?.includes('?')) await select(page);
	await expect(audio(page)).toHaveCount(1);
	await expect(player(page).locator('audio[controls]')).toHaveCount(1);
	await settle(page);
	return harness;
}
async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
async function emit(page: Page, event: string, options: { previous?: boolean; code?: number } = {}) {
	await page.evaluate(({ event, previous, code, raw }) => {
		const element = previous ? window.__lar.previous : document.querySelector<HTMLAudioElement>('[data-testid="audio-player"]');
		if (!element) throw new Error('native fixture audio missing before event');
		if (event === 'error') {
			Object.defineProperty(element, 'error', { configurable: true, value: { code, message: raw } });
		} else if (event === 'loadeddata' || event === 'canplay') {
			Object.defineProperty(element, 'error', { configurable: true, value: null });
			Object.defineProperty(element, 'readyState', { configurable: true, value: 3 });
		}
		element.dispatchEvent(new Event(event));
	}, { event, previous: options.previous ?? false, code: options.code ?? 4, raw: RAW_ERROR });
	await settle(page);
}
async function rememberAudio(page: Page) {
	await audio(page).evaluate((element: HTMLAudioElement) => { window.__lar.previous = element; });
}
async function counts(page: Page) {
	return page.evaluate(() => ({ loads: [...window.__lar.loads], loadCurrent: [...window.__lar.loadCurrent], plays: [...window.__lar.plays], pauses: window.__lar.pauses, writes: [...window.__lar.writes] }));
}
async function queue(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((elements) => elements.map((element) => ({
		id: element.getAttribute('data-track-id'), current: element.getAttribute('aria-current') === 'true'
	})));
}
async function storage(page: Page) {
	return page.evaluate(() => {
		const dump = (value: Storage) => {
			const entries: Record<string, string | null> = {};
			for (let index = 0; index < value.length; index += 1) {
				const key = value.key(index);
				if (key !== null) entries[key] = value.getItem(key);
			}
			return entries;
		};
		return { local: dump(localStorage), session: dump(sessionStorage) };
	});
}
async function check(info: TestInfo, expected: unknown, actual: unknown) {
	const record = { expected, actual };
	info.annotations.push({ type: 'native-audio-check', description: JSON.stringify(record) });
	expect(actual, `${info.title.split(' ')[0]} LAR_CONTRACT`).toEqual(expected);
}
async function recovery(page: Page, info: TestInfo) {
	await settle(page);
	await check(info, { reload: 1, status: 1, visible: true }, {
		reload: await reload(page).count(), status: await status(page).count(),
		visible: await reload(page).isVisible()
	});
	await expect(reload(page), 'LAR_CONTRACT: native reload action').toHaveJSProperty('tagName', 'BUTTON');
	await expect(reload(page), 'LAR_CONTRACT: no event-dependent lockout').toBeEnabled();
	await expect(status(page), 'LAR_CONTRACT: generic failure').toContainText(FAILURE);
	await check(info, true, await status(page).evaluate((element) =>
		element.getAttribute('role') === 'status' || element.getAttribute('aria-live') === 'polite'));
	await check(info, 0, await page.locator('main [role="alert"], main [aria-live="assertive"]').count());
	const text = `${await page.locator('main').innerText()} ${await status(page).evaluate((element) => element.outerHTML)}`;
	await check(info, false, /SyntheticDecoder|provider-detail|synthetic-media-marker|synthetic-signature|signature=|test-audio|expired|renew.*grant|decode error|network error/i.test(text));
	await check(info, true, await player(page).locator('audio[controls]').isVisible());
}
async function noFailure(page: Page, info: TestInfo) {
	await settle(page);
	const text = await status(page).count() ? await status(page).innerText() : '';
	await check(info, { reload: 0, failure: false }, { reload: await reload(page).count(), failure: FAILURE.test(text) });
}

test.afterEach(async ({}, info) => {
	const failed = info.status !== info.expectedStatus;
	const behavioral = info.errors.some((error) => error.message?.includes('LAR_CONTRACT'));
	const checks = info.annotations.filter((entry) => entry.type === 'native-audio-check').map((entry) => JSON.parse(entry.description!));
	const receipt = {
		case_id: info.title.split(' ')[0],
		expected: checks.length ? checks.map((entry) => entry.expected) : 'reach behavioral assertions',
		actual: failed ? info.errors.map((error) => error.message?.match(/error while loading shared libraries: [^\n]+/)?.[0] ?? error.message?.slice(0, 1200)) : checks.map((entry) => entry.actual),
		status: failed ? (behavioral ? 'RED' : 'ERROR') : 'GREEN',
		error_code: failed ? (behavioral ? 'NATIVE_AUDIO_CONTRACT' : 'HARNESS_OR_ACTION_FAILURE') : null,
		next_action: failed ? (behavioral ? 'implement recovery without editing protected tests' : 'repair execution; not behavioral RED') : 'preserve acceptance'
	};
	const body = JSON.stringify(receipt);
	console.log(body);
	await info.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

for (const code of [1, 2, 3, 4]) {
	test(`LAR-0${code} current MediaError code ${code} gets generic polite recovery, not a diagnosis`, async ({ page }, info) => {
		await start(page);
		await emit(page, 'error', { code });
		await recovery(page, info);
	});
}

test('LAR-05 reload loads only the still-current element once without other effects', async ({ page }, info) => {
	const harness = await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	const before = { counts: await counts(page), queue: await queue(page), storage: await storage(page), requests: [...harness.requests] };
	await reload(page).click();
	await settle(page);
	const after = await counts(page);
	await check(info, [ITEMS[0].streamUrl], after.loads.slice(before.counts.loads.length));
	await check(info, [true], after.loadCurrent.slice(before.counts.loadCurrent.length));
	await check(info, { plays: before.counts.plays, pauses: before.counts.pauses, writes: before.counts.writes }, { plays: after.plays, pauses: after.pauses, writes: after.writes });
	await check(info, before.queue, await queue(page));
	await check(info, before.storage, await storage(page));
	await check(info, before.requests, harness.requests);
	await check(info, [], harness.forbidden);
	await expect(audio(page), 'LAR_CONTRACT: active source is unchanged').toHaveAttribute('src', ITEMS[0].streamUrl!);
	await expect(page.getByTestId('library-now-playing'), 'LAR_CONTRACT: identity is unchanged').toContainText('Coral Night');
});

test('LAR-06 each explicit click stays usable without a readiness event', async ({ page }, info) => {
	await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	const before = await counts(page);
	for (let activation = 1; activation <= 3; activation += 1) {
		await expect(reload(page), 'LAR_CONTRACT: repeated explicit reload is available').toBeEnabled();
		await reload(page).click();
		await settle(page);
		await check(info, activation, (await counts(page)).loads.length - before.loads.length);
	}
	await check(info, before.plays, (await counts(page)).plays);
});

test('LAR-07 synchronous load refusal is generic and leaves another explicit attempt usable', async ({ page }, info) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	const before = await counts(page);
	await page.evaluate(() => { window.__lar.throwLoad = true; });
	await reload(page).click();
	await settle(page);
	await recovery(page, info);
	await check(info, false, /synthetic-load-marker|NotSupportedError|Synthetic load refusal/.test(await page.locator('main').innerText()));
	await check(info, [], errors);
	await page.evaluate(() => { window.__lar.throwLoad = false; });
	await reload(page).click();
	await settle(page);
	await check(info, 2, (await counts(page)).loads.length - before.loads.length);
	await check(info, before.plays, (await counts(page)).plays);
});

for (const [id, event] of [['08', 'loadeddata'], ['09', 'canplay']]) {
	test(`LAR-${id} current ${event} clears failure without playing`, async ({ page }, info) => {
		await start(page);
		await emit(page, 'error');
		await recovery(page, info);
		const before = await counts(page);
		await emit(page, event);
		await noFailure(page, info);
		await check(info, before, await counts(page));
	});
}

test('LAR-10 a reload request alone never claims recovered playback', async ({ page }, info) => {
	await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	await reload(page).click();
	await settle(page);
	const text = await status(page).count() ? await status(page).innerText() : '';
	await check(info, false, SUCCESS.test(text));
	await expect(reload(page), 'LAR_CONTRACT: reload remains available until readiness').toBeEnabled();
	await check(info, [], (await counts(page)).plays);
});

test('LAR-11 error and stalled media events never start automatic retries', async ({ page }, info) => {
	await page.clock.install();
	await page.clock.resume(); // rAF-based settling continues until explicit runFor.
	await start(page);
	const before = await counts(page);
	await emit(page, 'error');
	await recovery(page, info);
	// Install before page code can schedule a retry, then advance its timers.
	await page.clock.runFor(30_000);
	for (const event of ['stalled', 'waiting', 'suspend', 'error', 'abort', 'emptied']) {
		await emit(page, event);
	}
	await check(info, before, await counts(page));
});

test('LAR-12 changing track clears old failure and detached errors cannot revive it', async ({ page }, info) => {
	await start(page);
	await select(page, 'recovery-pine');
	await emit(page, 'error');
	await recovery(page, info);
	await rememberAudio(page);
	await page.getByTestId('player-next').click();
	await expect(audio(page)).toHaveAttribute('src', ITEMS[1].streamUrl!);
	await noFailure(page, info);
	await check(info, false, await page.evaluate(() => window.__lar.previous!.isConnected));
	const before = await counts(page);
	await emit(page, 'error', { previous: true });
	await noFailure(page, info);
	await check(info, before, await counts(page));
	await expect(page.getByTestId('library-now-playing'), 'LAR_CONTRACT: next track identity').toContainText('Pine Morning');
});

test('LAR-13 old readiness cannot clear the new track failure', async ({ page }, info) => {
	await start(page);
	await select(page, 'recovery-pine');
	await rememberAudio(page);
	await page.getByTestId('player-next').click();
	await emit(page, 'error');
	await recovery(page, info);
	await emit(page, 'loadeddata', { previous: true });
	await emit(page, 'canplay', { previous: true });
	await recovery(page, info);
	const before = await counts(page);
	await reload(page).click();
	await settle(page);
	await check(info, [ITEMS[1].streamUrl], (await counts(page)).loads.slice(before.loads.length));
});

test('LAR-14 same-track source replacement ignores the previous source in both directions', async ({ page, queueOrigin }, info) => {
	await start(page, { origin: queueOrigin, url: '/' });
	await expect.poll(() => page.evaluate(() => Boolean(window.libraryNowPlaying))).toBe(true);
	await emit(page, 'error');
	await recovery(page, info);
	await rememberAudio(page);
	await page.evaluate((track) => window.libraryNowPlaying.replace({
		tracks: [{ ...track, artist: track.artist ?? '', streamUrl: '/test-audio/replacement-coral' }], currentIndex: 0
	}), ITEMS[0]);
	await expect(audio(page)).toHaveAttribute('src', '/test-audio/replacement-coral');
	await noFailure(page, info);
	await emit(page, 'error', { previous: true });
	await noFailure(page, info);
	await emit(page, 'error');
	await recovery(page, info);
	await emit(page, 'canplay', { previous: true });
	await recovery(page, info);
	const before = await counts(page);
	await reload(page).click();
	await settle(page);
	await check(info, ['/test-audio/replacement-coral'], (await counts(page)).loads.slice(before.loads.length));
});

test('LAR-15 clearing queue removes recovery and ignores all detached completions', async ({ page }, info) => {
	await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	await rememberAudio(page);
	await reload(page).click();
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await expect(audio(page)).toHaveCount(0);
	await check(info, 0, await player(page).count());
	const before = await counts(page);
	for (const event of ['error', 'loadeddata', 'canplay']) await emit(page, event, { previous: true });
	await check(info, { player: 0, queue: [], counts: before }, { player: await player(page).count(), queue: await queue(page), counts: await counts(page) });
});

test('LAR-16 auth generation change purges recovery even when the next account reuses a track id', async ({ page }, info) => {
	const harness = await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	await rememberAudio(page);
	await reload(page).click();
	const otherTab = await page.context().newPage();
	try {
		await otherTab.goto(new URL('/login', page.url()).href);
		await otherTab.evaluate(() => {
			localStorage.setItem('nntune.library.auth-transition.v1', 'synthetic-auth-boundary');
			localStorage.removeItem('nntune.library.auth-transition.v1');
		});
	} finally { await otherTab.close(); }
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(audio(page)).toHaveCount(0);
	await check(info, [], await queue(page));
	// Deliberately reuse id AND source so identity comparisons alone cannot pass.
	harness.payload.items = ITEMS.map((item) => ({ ...item, title: `${item.title} Next Account` }));
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await select(page);
	await noFailure(page, info);
	await emit(page, 'error', { previous: true });
	await noFailure(page, info);
	await emit(page, 'error');
	await recovery(page, info);
	await emit(page, 'canplay', { previous: true });
	await recovery(page, info);
	await expect(page.getByTestId('library-now-playing'), 'LAR_CONTRACT: new account identity').toContainText('Next Account');
});

test('LAR-17 an unauthorized refresh removes recovery and cannot retain a prior session player', async ({ page }, info) => {
	const harness = await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	await rememberAudio(page);
	harness.payload.status = 401;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toBeVisible();
	await expect(audio(page)).toHaveCount(0);
	await emit(page, 'error', { previous: true });
	await emit(page, 'canplay', { previous: true });
	await check(info, { player: 0, queue: [] }, { player: await player(page).count(), queue: await queue(page) });
});

test('LAR-18 metadata-only current items never show audio or recovery', async ({ page }, info) => {
	await openLibrary(page);
	await check(info, 0, await player(page).count());
	await select(page, 'recovery-plain');
	await check(info, { audio: 0, player: 0, reload: 0, status: 0, loads: [], plays: [] }, {
		audio: await audio(page).count(), player: await player(page).count(),
		reload: await page.getByRole('button', { name: 'Reload audio', exact: true }).count(),
		status: await page.getByTestId('library-audio-status').count(), loads: (await counts(page)).loads, plays: (await counts(page)).plays
	});
});

test('LAR-19 healthy native audio has controls but no recovery action', async ({ page }, info) => {
	await start(page);
	await noFailure(page, info);
	const observed = await counts(page);
	await check(info, [], observed.plays);
	await check(info, true, observed.loads.length <= 1 && observed.loadCurrent.every(Boolean));
});

for (const [id, key] of [['20', 'Enter'], ['21', 'Space']]) {
	test(`LAR-${id} ${key} reaches a visibly focused reload and activates it once`, async ({ page }, info) => {
		await start(page);
		await emit(page, 'error');
		await recovery(page, info);
		for (let tab = 0; tab < 80 && !await reload(page).evaluate((element) => element === document.activeElement); tab += 1) {
			await page.keyboard.press('Tab');
		}
		await expect(reload(page), 'LAR_CONTRACT: reload is reachable by Tab, not programmatic focus').toBeFocused();
		await check(info, true, await reload(page).evaluate((element) => {
			const css = getComputedStyle(element);
			return element.matches(':focus-visible') && (
				(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0 && css.outlineColor !== 'rgba(0, 0, 0, 0)') || css.boxShadow !== 'none'
			);
		}));
		const before = await counts(page);
		await reload(page).press(key);
		await settle(page);
		await check(info, 1, (await counts(page)).loads.length - before.loads.length);
		await expect(reload(page), 'LAR_CONTRACT: reload retains focus while waiting').toBeFocused();
		await check(info, before.plays, (await counts(page)).plays);
	});
}

for (const colorScheme of ['light', 'dark'] as const) {
	test.describe(`narrow ${colorScheme}`, () => {
		test.use({ viewport: { width: 360, height: 740 }, hasTouch: true, colorScheme });
		test(`LAR-${colorScheme === 'light' ? '22' : '23'} touch recovery remains visible without horizontal overflow`, async ({ page }, info) => {
			await start(page);
			await emit(page, 'error');
			await recovery(page, info);
			await reload(page).scrollIntoViewIfNeeded();
			const box = await reload(page).boundingBox();
			await check(info, true, Boolean(box && box.x >= 0 && box.x + box.width <= 361 && box.height >= 24 && box.width >= 24));
			await check(info, true, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
			const before = await counts(page);
			await reload(page).tap();
			await settle(page);
			await check(info, 1, (await counts(page)).loads.length - before.loads.length);
			await check(info, before.plays, (await counts(page)).plays);
		});
	});
}

test('LAR-24 timestamp-share seeking and queue identity survive error and readiness', async ({ page }, info) => {
	await start(page, { url: '/library?track=recovery-coral&t=35' });
	await expect.poll(() => audio(page).evaluate((element: HTMLAudioElement) => element.currentTime)).toBe(35);
	await select(page, 'recovery-pine');
	await emit(page, 'error');
	await recovery(page, info);
	const before = { queue: await queue(page), storage: await storage(page), plays: (await counts(page)).plays };
	await check(info, 35, await audio(page).evaluate((element: HTMLAudioElement) => element.currentTime));
	await reload(page).click();
	// Model the native load position reset at the boundary; readiness must not
	// replay an already-consumed inbound timestamp or claim playback success.
	await audio(page).evaluate((element: HTMLAudioElement) => { element.currentTime = 0; });
	await emit(page, 'loadeddata');
	await noFailure(page, info);
	await check(info, 0, await audio(page).evaluate((element: HTMLAudioElement) => element.currentTime));
	// User seeking still wins over any late share-link metadata/clamp handling.
	await audio(page).evaluate((element: HTMLAudioElement) => {
		element.currentTime = 12;
		element.dispatchEvent(new Event('seeking'));
		element.dispatchEvent(new Event('loadedmetadata'));
		element.dispatchEvent(new Event('durationchange'));
	});
	await settle(page);
	await check(info, 12, await audio(page).evaluate((element: HTMLAudioElement) => element.currentTime));
	await check(info, before, { queue: await queue(page), storage: await storage(page), plays: (await counts(page)).plays });
	await expect(page.getByTestId('library-now-playing'), 'LAR_CONTRACT: identity survives readiness').toContainText('Coral Night');
});

test('LAR-25 removing the failed current item preserves queue behavior and clears recovery', async ({ page }, info) => {
	await start(page);
	await select(page, 'recovery-pine');
	await emit(page, 'error');
	await recovery(page, info);
	await rememberAudio(page);
	await page.locator('[data-testid="queue-item"][data-track-id="recovery-coral"]')
		.getByRole('button', { name: /remove/i }).click();
	await expect(audio(page)).toHaveAttribute('src', ITEMS[1].streamUrl!);
	await noFailure(page, info);
	await emit(page, 'error', { previous: true });
	await noFailure(page, info);
	await check(info, [{ id: 'recovery-pine', current: true }], await queue(page));
});

test('LAR-26 a detached reload action cannot reload the next current source', async ({ page }, info) => {
	await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	const oldAction = await reload(page).elementHandle();
	if (!oldAction) throw new Error('recovery action missing after its assertion');
	await select(page, 'recovery-pine');
	await page.getByTestId('player-next').click();
	await noFailure(page, info);
	const before = await counts(page);
	// A queued activation of the removed action is not a new user request to
	// reload a different source. Dispatch on the retained real button, not a mock.
	await oldAction.evaluate((element) => (element as HTMLButtonElement).click());
	await settle(page);
	await check(info, before, await counts(page));
	await expect(audio(page), 'LAR_CONTRACT: stale action leaves the current source alone').toHaveAttribute('src', ITEMS[1].streamUrl!);
});

test('LAR-27 route teardown ignores old media completions and remount starts clean', async ({ page }, info) => {
	await start(page);
	await emit(page, 'error');
	await recovery(page, info);
	await rememberAudio(page);
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.id = 'recovery-leave-library';
		link.href = '/login';
		link.textContent = 'Leave library';
		document.body.append(link);
	});
	await page.locator('#recovery-leave-library').click();
	await expect(page).toHaveURL(/\/login$/);
	await expect(audio(page)).toHaveCount(0);
	const before = await counts(page);
	for (const event of ['error', 'loadeddata', 'canplay']) await emit(page, event, { previous: true });
	await check(info, before, await counts(page));
	await check(info, 0, await page.getByTestId('library-audio-status').count());
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await check(info, { player: 0, queue: [] }, { player: await player(page).count(), queue: await queue(page) });
	await select(page);
	await noFailure(page, info);
});
