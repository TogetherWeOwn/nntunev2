import { expect, test, type Page, type TestInfo } from '@playwright/test';

/**
 * DEMOFIND — protected test-only handoff; test/demo-queue-filter-acceptance.
 * Protected: this spec and src/lib/player/demoQueueFilter.acceptance.test.ts.
 * Builder-owned source: src/lib/player/queueFilter.ts,
 * src/lib/player/PlayerQueue.svelte, src/routes/demo/player/+page.svelte.
 * Do not edit existing PlayerQueue/demo acceptance tests. Use the one shared
 * matchesQueueQuery export pinned by the companion units in both consumers.
 *
 * UI: native textbox/searchbox "Filter queue", button "Clear queue filter".
 * Live role=status text contains "N of M tracks"; a nonempty filtered queue
 * with zero results also announces "No matches in this queue". No new global
 * TESTIDS entries required. Keep queue-item, data-track-id, now-playing and
 * existing transport hooks. Clear restores all rows and focus within the panel.
 *
 * Offline setup: pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 * Run: CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *        tests/demo-queue-filter.acceptance.spec.ts --workers=1
 * Regression (unchanged): same command selecting tests/player-queue.spec.ts,
 * tests/shuffle-upcoming.spec.ts, tests/library-queue-filter.spec.ts and
 * tests/library-queue-filter-pins.spec.ts. Units: companion header.
 * Chromium must already be installed. Local host may require LD_LIBRARY_PATH
 * pointing at its provisioned browser libraries. Port 4173 must be free, or
 * supply a run-owned config with an unused strict port; never reuse a preview.
 * All network is loopback-only, audio held locally; no backend/prod/provider.
 *
 * Breaks caught: wrong match field/normalization; matching hidden IDs/regexes;
 * missing announcements; destructive filtering/auto-selection/autoplay/seek;
 * persistence of query; reordering by visible index; keyboard/touch overflow.
 * Main RED is a JSON-bearing assertion AFTER the real demo fixture hydrates,
 * not an import/setup error or a timed-out click. Controls remain GREEN.
 * No RED-only ready PR: Builder continues the exact branch in a GREEN PR.
 */

type Probe = { play: number; pause: number; load: number; seek: number; audio: HTMLAudioElement | null };
declare global { interface Window { __demoFind: Probe } }
const ORDER = ['demo-track', 'second-signal', 'third-light'];
const input = (page: Page) => page.getByRole('textbox', { name: 'Filter queue', exact: true })
	.or(page.getByRole('searchbox', { name: 'Filter queue', exact: true }));
const clear = (page: Page) => page.getByRole('button', { name: 'Clear queue filter', exact: true });
const order = (page: Page) => page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
const settle = (page: Page) => page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
const missing = { error: 'demo queue-only Filter queue input missing' };

test.setTimeout(20_000);
test.use({ hasTouch: true });

async function receipt(info: TestInfo, expected: unknown, observe: () => Promise<unknown>) {
	let actual: unknown = null;
	let passed = false;
	try {
		actual = await observe();
		expect(actual).toEqual(expected);
		passed = true;
	} catch (error) {
		if (actual === null) actual = { error: error instanceof Error ? error.message : String(error) };
		throw error;
	} finally {
		const line = JSON.stringify({ case_id: info.title.split(' ')[0], expected, actual,
			status: passed ? 'PASS' : 'FAIL', error_code: passed ? null : 'DEMO_QUEUE_FILTER_CONTRACT',
			next_action: passed ? 'none' : 'Implement demo queue presentation filtering without changing playback or the protected tests.' });
		console.log(line);
		await info.attach('acceptance-case', { body: line, contentType: 'application/json' });
	}
}

async function open(page: Page, baseURL: string) {
	const origin = new URL(baseURL);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
	await page.route('**/*', (route) => {
		const target = new URL(route.request().url());
		if (target.origin !== origin.origin) return route.abort();
		// Keep browser media pending locally; deterministic media doubles below
		// isolate the external decoder, not the real playback state machine.
		if (target.pathname === '/api/demo/audio') return;
		if (target.pathname.startsWith('/api/')) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		const probe: Probe = { play: 0, pause: 0, load: 0, seek: 0, audio: null };
		window.__demoFind = probe;
		const times = new WeakMap<HTMLMediaElement, number>();
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get() { return times.get(this) ?? 0; },
			set(value: number) { probe.seek += 1; times.set(this, value); }
		});
		HTMLMediaElement.prototype.play = function () {
			probe.play += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pause += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		HTMLMediaElement.prototype.load = function () { probe.load += 1; };
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// SSR rows alone are insufficient: this real mutation proves hydration.
	await page.getByTestId('queue-move-up-third-light').click();
	await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	await page.getByTestId('queue-move-down-third-light').click();
	await expect.poll(() => order(page)).toEqual(ORDER);
	await settle(page);
}

async function live(page: Page) {
	return (await page.getByRole('status').allTextContents()).join(' ').replace(/\s+/g, ' ');
}

async function snapshot(page: Page) {
	return page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => {
		const probe = window.__demoFind;
		const metadata = navigator.mediaSession?.metadata;
		return {
			sameAudio: probe.audio === audio, src: audio.getAttribute('src'), time: audio.currentTime,
			paused: audio.paused, rate: audio.playbackRate,
			calls: { play: probe.play, pause: probe.pause, load: probe.load, seek: probe.seek },
			metadata: metadata ? { title: metadata.title, artist: metadata.artist } : null,
			local: { ...localStorage }, session: { ...sessionStorage },
			now: document.querySelector('[data-testid="now-playing"]')?.textContent,
			status: document.querySelector('[data-testid="player-status"]')?.textContent
		};
	});
}

for (const vector of [
	{ id: 'DEMOFIND-B01', query: '  sIgNaL  ', rows: ['second-signal'] },
	{ id: 'DEMOFIND-B02', query: '  fIxTuRe BaNd  ', rows: ORDER },
	{ id: 'DEMOFIND-B03', query: ' \t ', rows: ORDER },
	{ id: 'DEMOFIND-B04', query: 'no such track', rows: [] },
	{ id: 'DEMOFIND-B05', query: 'second-signal', rows: [] },
	{ id: 'DEMOFIND-B06', query: '.*', rows: [] }
]) {
	test(`${vector.id} demo title/artist substring ${JSON.stringify(vector.query)}`, async ({ page, baseURL }, info) => {
		await open(page, baseURL!);
		await receipt(info, { rows: vector.rows, countAnnounced: true, noMatchAnnounced: vector.rows.length === 0 }, async () => {
			if (await input(page).count() !== 1) return missing;
			await input(page).fill(vector.query);
			await settle(page);
			const status = await live(page);
			return { rows: await order(page), countAnnounced: status.includes(`${vector.rows.length} of 3 tracks`),
				noMatchAnnounced: status.includes('No matches in this queue') };
		});
	});
}

for (const intent of ['playing', 'paused'] as const) {
	test(`DEMOFIND-B0${intent === 'playing' ? '7' : '8'} ${intent} query/clear leaves order, current selection and playback untouched`, async ({ page, baseURL }, info) => {
		await open(page, baseURL!);
		await page.getByTestId('queue-move-up-third-light').click();
		await page.getByTestId('queue-move-up-third-light').click();
		await page.getByTestId('queue-select-second-signal').click();
		await page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement, paused) => {
			audio.dispatchEvent(new Event('playing'));
			if (paused) { audio.pause(); audio.dispatchEvent(new Event('pause')); }
			audio.currentTime = 42.5;
			audio.dispatchEvent(new Event('timeupdate'));
			window.__demoFind.audio = audio;
		}, intent === 'paused');
		await expect(page.getByTestId('player-status')).toHaveText(`Status: ${intent}`);
		await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
		await settle(page);
		const before = await snapshot(page);
		expect(before.time).toBe(42.5);
		expect(before.sameAudio).toBe(true);
		const requests: string[] = [];
		page.on('request', (request) => requests.push(`${request.method()} ${new URL(request.url()).pathname}`));
		await receipt(info, { filtered: ['third-light'], restored: ['third-light', 'demo-track', 'second-signal'],
			unchanged: true, current: 'second-signal', requests: [] }, async () => {
			if (await input(page).count() !== 1) return missing;
			await input(page).fill('third');
			await settle(page);
			const filtered = await order(page);
			const during = await snapshot(page);
			// Editing shortcuts must stay text input, never transport hotkeys.
			await input(page).fill('');
			await input(page).pressSequentially('k j m r ');
			await input(page).press('ArrowLeft');
			await input(page).press('Enter');
			await clear(page).click();
			await settle(page);
			return { filtered, restored: await order(page),
				unchanged: JSON.stringify(during) === JSON.stringify(before) && JSON.stringify(await snapshot(page)) === JSON.stringify(before),
				current: await page.evaluate(() => JSON.parse(sessionStorage.getItem('nntune:demo-queue:v1') ?? 'null')?.currentId), requests };
		});
	});
}

test('DEMOFIND-B09 keyboard clear restores full queue, count and in-panel focus on a narrow touch screen', async ({ page, baseURL }, info) => {
	await page.setViewportSize({ width: 360, height: 740 });
	await open(page, baseURL!);
	await receipt(info, { rows: ORDER, value: '', countAnnounced: true, focusInside: true, fits: true, touchTargets: true }, async () => {
		if (await input(page).count() !== 1) return missing;
		await input(page).tap();
		await input(page).pressSequentially('missing');
		await clear(page).focus();
		const boxes = await Promise.all([input(page).boundingBox(), clear(page).boundingBox()]);
		await page.keyboard.press('Space');
		await settle(page);
		const focusInside = await input(page).evaluate((element) => {
			for (let panel = element.parentElement; panel && panel !== document.body; panel = panel.parentElement) {
				if (panel.querySelector('[data-testid="queue-item"]')) return panel.contains(document.activeElement);
			}
			return false;
		});
		return { rows: await order(page), value: await input(page).inputValue(), countAnnounced: (await live(page)).includes('3 of 3 tracks'),
			focusInside, fits: await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
			touchTargets: boxes.every((box) => box !== null && box.height >= 24 && box.width >= 24 && box.x >= 0 && box.x + box.width <= 361) };
	});
});

test('DEMOFIND-B10 next uses the arranged queue, not the filtered visible row', async ({ page, baseURL }, info) => {
	await open(page, baseURL!);
	await receipt(info, { visible: ['third-light'], playingSecond: true, restored: ORDER, current: 'second-signal' }, async () => {
		if (await input(page).count() !== 1) return missing;
		await input(page).fill('third');
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
		const visible = await order(page);
		await clear(page).click();
		return { visible, playingSecond: (await page.getByTestId('now-playing').textContent())?.includes('Second Signal'), restored: await order(page),
			current: await page.evaluate(() => JSON.parse(sessionStorage.getItem('nntune:demo-queue:v1') ?? 'null')?.currentId) };
	});
});

test('DEMOFIND-B11 filter is transient across reload, while saved queue order stays intact', async ({ page, baseURL }, info) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-move-up-third-light').click();
	await receipt(info, { rows: ['demo-track', 'third-light', 'second-signal'], value: '', countAnnounced: true }, async () => {
		if (await input(page).count() !== 1) return missing;
		await input(page).fill('third');
		await page.reload();
		await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
		return { rows: await order(page), value: await input(page).inputValue(), countAnnounced: (await live(page)).includes('3 of 3 tracks') };
	});
});

test('DEMOFIND-B12 empty queue is not a no-match queue', async ({ page, baseURL }, info) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await receipt(info, { countAnnounced: true, noMatchAnnounced: false, queueEmpty: true }, async () => {
		if (await input(page).count() !== 1) return missing;
		await input(page).fill('missing');
		await settle(page);
		const status = await live(page);
		return { countAnnounced: status.includes('0 of 0 tracks'), noMatchAnnounced: status.includes('No matches in this queue'),
			queueEmpty: (await page.getByTestId('now-playing').textContent())?.includes('Queue is empty') };
	});
});

test('DEMOFIND-C00 control: real demo hydration, queue persistence and media probe work without a filter', async ({ page, baseURL }, info) => {
	await open(page, baseURL!);
	await receipt(info, { rows: ORDER, stored: { orderIds: ORDER, currentId: 'demo-track' }, plays: 0 }, async () => ({
		rows: await order(page), stored: await page.evaluate(() => JSON.parse(sessionStorage.getItem('nntune:demo-queue:v1') ?? 'null')),
		plays: await page.evaluate(() => window.__demoFind.play)
	}));
});
