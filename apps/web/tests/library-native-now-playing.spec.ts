import { expect, test as base, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-now-playing/mount.js';

/**
 * LIBNOWPLAY — native library player identity acceptance, tests-only handoff.
 *
 * Protected: this spec + tests/fixtures/library-now-playing/*.
 * Builder-owned implementation: src/routes/library/+page.svelte and, if needed,
 * src/lib/player/PlayerQueue.svelte / other src/lib/player/*.ts. Existing suites
 * remain unchanged. Continue the handoff branch; open a combined GREEN PR only.
 *
 * Presentation seam: data-testid="library-now-playing" is the ONE visible
 * title/artist readout IN the Player region containing native audio[controls].
 * It is not the existing queue's data-testid="now-playing" live announcement.
 * Wording/separators are free; identity must be literal text, not HTML. Missing
 * artists produce no undefined/null placeholder; blank titles use stable ids.
 *
 * PREMISE: current /library selection intentionally stores streamUrl:''; it has
 * no persistence or top/bottom selection UI. Do NOT implement streaming or new
 * controls/storage just to seed these tests. A test-only Vite pre-transform adds
 * ONE exported setter calling the page's existing replaceQueue. We mount the
 * REAL page and feed it playable queue states / existing selectTrack, moveTrack
 * and restoreQueue reducers. No readout, stream derivation, markup, transport or
 * announcement is replaced. Actual queue buttons still exercise page handlers.
 * This tests restored-state rendering, not nonexistent library persistence.
 *
 * Setup (cached packages/browser required for offline use):
 *   pnpm --dir apps/web install --offline --frozen-lockfile
 * Run (4173 must be free; CI=1 prevents reuse of another worktree's preview):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-native-now-playing.spec.ts --workers=1 --retries=0
 * Regression: same command with tests/library-player-queue.spec.ts,
 * tests/library-player-queue-pins.spec.ts, tests/library-queue-manage.spec.ts.
 * The runner owns preview + ephemeral component server lifetimes. HTTP fixtures
 * are local; media, other API and external requests are blocked. No live backend.
 *
 * Every case emits/attaches compact JSON expected/actual/status/error_code/
 * next_action. Missing identity is assertion-RED, never a click timeout/import
 * failure. LN10/LN11 are already-existing no-stream controls (baseline GREEN is
 * correct); new readout cases must be RED on base. Never fabricate guard RED.
 * Breaks caught: stale/index-based identity, empty fallback, HTML interpretation,
 * wrong restored id, readout outside player, stale hidden text, duplicate live
 * path, progress chatter and narrow-screen overflow/low contrast.
 */

const routePath = decodeURIComponent(new URL('../src/routes/library/+page.svelte', import.meta.url).pathname);
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const test = base.extend<{ libraryPage: Page }, { libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/library-now-playing/', import.meta.url).pathname),
			publicDir: false,
			resolve: { alias: { '#lib': `${webRoot}src/lib` } },
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
		await localOnly(page, libraryOrigin);
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryNowPlaying))).toBe(true);
		await use(page);
	}
});

test.setTimeout(20_000);
test.use({ viewport: { width: 1000, height: 740 } });

const TRACKS: Track[] = [
	{ id: 'ln-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '/__acceptance_audio__/coral.wav' },
	{ id: 'ln-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '/__acceptance_audio__/pine.wav' },
	{ id: 'ln-ember', title: 'Ember Noon', artist: 'West Quartet', streamUrl: '/__acceptance_audio__/ember.wav' }
];
const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const readout = (page: Page) => page.getByTestId('library-now-playing');
const row = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);

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
	await page.evaluate((value) => window.libraryNowPlaying.replace(value), next);
	await settle(page);
}

async function identity(page: Page, title: string, artist: string, stale = '') {
	await settle(page);
	const count = await readout(page).count();
	const text = count === 1 ? (await readout(page).innerText()).trim() : '';
	return {
		count,
		visible: count === 1 && await readout(page).isVisible(),
		inPlayer: await player(page).getByTestId('library-now-playing').count() === 1,
		title: text.includes(title),
		artist: artist ? text.includes(artist) : !/undefined|null/.test(text),
		noStale: !stale || !text.includes(stale),
		native: await player(page).locator('audio[controls]').count() === 1,
		src: await player(page).locator('audio').getAttribute('src')
	};
}

const named = { count: 1, visible: true, inPlayer: true, title: true, artist: true, noStale: true, native: true };
const expectedCoral = { ...named, src: '/__acceptance_audio__/coral.wav' };
const expectedPine = { ...named, src: '/__acceptance_audio__/pine.wav' };
const expectedEmber = { ...named, src: '/__acceptance_audio__/ember.wav' };

async function hidden(page: Page) {
	await settle(page);
	return { sections: await player(page).count(), readouts: await readout(page).count(), audio: await page.locator('audio').count() };
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
			error_code: passed ? null : 'LIBRARY_NOW_PLAYING_CONTRACT',
			next_action: passed ? 'preserve acceptance' : 'implement library player identity; do not edit protected tests'
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

test('LIBNOWPLAY-LN01 title and artist appear beside native controls', async ({ libraryPage: page }, info) => {
	await replace(page);
	await check(info, expectedCoral, await identity(page, 'Coral Night', 'Mira North'));
});

test('LIBNOWPLAY-LN02 whitespace title falls back to stable id', async ({ libraryPage: page }, info) => {
	await replace(page, state(0, [{ ...TRACKS[0], title: ' \t ' }]));
	await check(info, expectedCoral, await identity(page, 'ln-coral', 'Mira North'));
});

test('LIBNOWPLAY-LN03 absent artist never prints undefined or null', async ({ libraryPage: page }, info) => {
	const { artist: _, ...withoutArtist } = TRACKS[0];
	await replace(page, state(0, [withoutArtist as Track]));
	await check(info, expectedCoral, await identity(page, 'Coral Night', ''));
});

test('LIBNOWPLAY-LN04 title and artist stay literal text', async ({ libraryPage: page }, info) => {
	await replace(page, state(0, [{ ...TRACKS[0], title: '<b>Coral & Night</b>', artist: '<i>Mira</i>' }]));
	await check(info, { identity: expectedCoral, interpreted: 0 }, {
		identity: await identity(page, '<b>Coral & Night</b>', '<i>Mira</i>'),
		interpreted: await readout(page).locator('b, i').count()
	});
});

test('LIBNOWPLAY-LN05 direct selection and bottom/top jumps follow identity, not stale index', async ({ libraryPage: page }, info) => {
	await replace(page);
	await page.evaluate(() => window.libraryNowPlaying.select(1));
	const direct = await identity(page, 'Pine Morning', 'South Ensemble', 'Coral Night');
	await page.evaluate(() => window.libraryNowPlaying.select(2));
	const bottom = await identity(page, 'Ember Noon', 'West Quartet', 'Pine Morning');
	await page.evaluate(() => window.libraryNowPlaying.select(0));
	await check(info, { direct: expectedPine, bottom: expectedEmber, top: expectedCoral }, {
		direct, bottom, top: await identity(page, 'Coral Night', 'Mira North', 'Ember Noon')
	});
});

test('LIBNOWPLAY-LN06 moving current to another slot preserves its readout and audio URL', async ({ libraryPage: page }, info) => {
	await replace(page, state(1));
	await page.evaluate(() => window.libraryNowPlaying.move(1, 2));
	await check(info, { identity: expectedPine, current: 'ln-pine', order: ['ln-coral', 'ln-ember', 'ln-pine'] }, {
		identity: await identity(page, 'Pine Morning', 'South Ensemble', 'Coral Night'),
		current: await page.locator('[data-testid="queue-item"][aria-current="true"]').getAttribute('data-track-id'),
		order: await page.getByTestId('queue-item').evaluateAll((rows) => rows.map((item) => item.getAttribute('data-track-id')))
	});
});

test('LIBNOWPLAY-LN07 real play-next then Next updates both readout and stream', async ({ libraryPage: page }, info) => {
	await replace(page);
	await row(page, 'ln-ember').getByRole('button', { name: 'Play Ember Noon next', exact: true }).click();
	const beforeNext = await identity(page, 'Coral Night', 'Mira North', 'Ember Noon');
	await page.getByTestId('player-next').click();
	await check(info, { beforeNext: expectedCoral, afterNext: expectedEmber }, {
		beforeNext, afterNext: await identity(page, 'Ember Noon', 'West Quartet', 'Coral Night')
	});
});

test('LIBNOWPLAY-LN08 restored nondefault order/current id renders the restored identity', async ({ libraryPage: page }, info) => {
	await replace(page);
	await page.evaluate((catalogue) => window.libraryNowPlaying.restore(
		'{"orderIds":["ln-ember","ln-coral","ln-pine"],"currentId":"ln-pine"}', catalogue
	), TRACKS);
	await check(info, expectedPine, await identity(page, 'Pine Morning', 'South Ensemble', 'Coral Night'));
});

test('LIBNOWPLAY-LN09 unknown restored current id uses the resolved fallback track', async ({ libraryPage: page }, info) => {
	await replace(page);
	await page.evaluate((catalogue) => window.libraryNowPlaying.restore(
		'{"orderIds":["ln-ember","ln-coral"],"currentId":"unknown-track"}', catalogue
	), TRACKS);
	await check(info, expectedEmber, await identity(page, 'Ember Noon', 'West Quartet', 'unknown-track'));
});

test('LIBNOWPLAY-LN10 empty/invalid current and blank streams hide the WHOLE Player section', async ({ libraryPage: page }, info) => {
	const actual = [];
	for (const value of [state(-1, []), state(99), state(0, [{ ...TRACKS[0], streamUrl: '' }]), state(0, [{ ...TRACKS[0], streamUrl: ' \t ' }])]) {
		await replace(page, value);
		actual.push(await hidden(page));
	}
	await check(info, Array(4).fill({ sections: 0, readouts: 0, audio: 0 }), actual);
});

test('LIBNOWPLAY-LN11 real library metadata selection stays nonplayable', async ({ page, baseURL }, info) => {
	await localOnly(page, baseURL!);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await page.locator('[data-testid="library-item"][data-id="ln-coral"] button.nt-library-row').click();
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await check(info, { sections: 0, readouts: 0, audio: 0 }, await hidden(page));
});

test('LIBNOWPLAY-LN12 stream loss and clear remove previously visible identity, not just audio', async ({ libraryPage: page }, info) => {
	await replace(page, state(0, [TRACKS[0], { ...TRACKS[1], streamUrl: '' }]));
	const before = await identity(page, 'Coral Night', 'Mira North');
	await page.getByTestId('player-next').click();
	const noStream = await hidden(page);
	await replace(page);
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await check(info, { before: expectedCoral, noStream: { sections: 0, readouts: 0, audio: 0 }, cleared: { sections: 0, readouts: 0, audio: 0 } }, {
		before, noStream, cleared: await hidden(page)
	});
});

test('LIBNOWPLAY-LN13 exactly one existing announcement path; no progress/reselect chatter', async ({ libraryPage: page }, info) => {
	await replace(page);
	await page.getByTestId('now-playing').evaluate((region) => {
		window.libraryNowPlayingAnnouncements = [];
		new MutationObserver(() => window.libraryNowPlayingAnnouncements.push((region.textContent ?? '').replace(/\s+/g, ' ').trim()))
			.observe(region, { childList: true, characterData: true, subtree: true });
	});
	await page.getByTestId('player-next').click();
	await settle(page);
	const changes = await page.evaluate(() => [...window.libraryNowPlayingAnnouncements]);
	const livePaths = await page.locator('[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]').evaluateAll((nodes) =>
		nodes.filter((node) => node.getAttribute('aria-live') !== 'off' && node.getClientRects().length && (node.textContent ?? '').includes('Pine Morning')).length);
	const quietReadout = await readout(page).count() === 1 && await readout(page).evaluate((node) =>
		!node.closest('[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]'));
	await page.locator('[data-testid="library-item"][data-id="ln-pine"] button.nt-library-row').click();
	await page.locator('audio').evaluate((audio: HTMLAudioElement) => {
		for (const seconds of [1, 2, 3, 10, 60]) {
			audio.currentTime = seconds;
			audio.dispatchEvent(new Event('timeupdate'));
		}
	});
	await settle(page);
	await check(info, {
		identity: expectedPine, changes: 1, changedTitle: true, changedArtist: true,
		afterProgress: 1, livePaths: 1, quietReadout: true
	}, {
		identity: await identity(page, 'Pine Morning', 'South Ensemble', 'Coral Night'),
		changes: changes.length,
		changedTitle: changes.every((text) => text.includes('Pine Morning')),
		changedArtist: changes.every((text) => text.includes('South Ensemble')),
		afterProgress: await page.evaluate(() => window.libraryNowPlayingAnnouncements.length), livePaths, quietReadout
	});
});

for (const [index, theme] of ['light', 'dark'].entries()) {
	test(`LIBNOWPLAY-LN${14 + index} ${theme} narrow native player identity stays readable and contained`, async ({ libraryPage: page }, info) => {
		await page.setViewportSize({ width: 360, height: 740 });
		await page.emulateMedia({ colorScheme: theme as 'light' | 'dark' });
		await page.evaluate((theme) => document.documentElement.setAttribute('data-appearance', theme), theme);
		await replace(page, state(0, [{ ...TRACKS[0], title: 'Coral Night '.repeat(12).trim(), artist: 'Mira North '.repeat(8).trim() }]));
		const layout = await readout(page).count() === 1 ? await readout(page).evaluate((node) => {
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
			return {
				contained: box.x >= 0 && box.right <= innerWidth + 1 && document.documentElement.scrollWidth <= innerWidth,
				readable: background.length >= 3 && (values[0] + 0.05) / (values[1] + 0.05) >= 4.5
			};
		}) : { contained: false, readable: false };
		await check(info, { identity: expectedCoral, layout: { contained: true, readable: true } }, {
			identity: await identity(page, 'Coral Night', 'Mira North'), layout
		});
	});
}
