import { expect, test, type Page } from '@playwright/test';

const HISTORY = 'nntune:demo-recently-played:v1';
const QUEUE = 'nntune:demo-queue:v1';
const RESUME = 'nntune:resume:v1';
const baseline = [{ trackId: 'third-light', playedAt: 300 }, { trackId: 'demo-track', playedAt: 100 }];
type Fixture = { id: string; clear(): Promise<void>; entries(): { trackId: string; playedAt: number }[] };
type Probe = { blocked: 'none' | 'writes' | 'all'; read(key: string): string | null; plays: number; pauses: number; loads: number; seeks: number; sources: number; identity(node: Element): number };
type Instrumented = Window & { __nntuneHistoryFixture: Fixture; __historyProbe: Probe; __oldHistoryFixture?: Fixture };
const calls = new WeakMap<Page, string[]>();

async function open(page: Page) {
	const rejected: string[] = [];
	calls.set(page, rejected);
	let origin = '';
	await page.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		origin ||= url.origin;
		if (url.origin !== origin || request.method() !== 'GET' || ['fetch', 'xhr'].includes(request.resourceType())) {
			rejected.push(`${request.method()} ${url.origin}${url.pathname}`);
			await route.abort();
		} else if (url.pathname !== '/api/demo/audio') await route.fallback();
	});
	await page.clock.setFixedTime(new Date('2026-01-01T12:00:00Z'));
	await page.addInitScript(({ history, queue, resume, seed }) => {
		const session = sessionStorage;
		const get = Storage.prototype.getItem;
		const set = Storage.prototype.setItem;
		if (get.call(session, 'history-fixture-seeded') === null) {
			set.call(session, history, JSON.stringify(seed));
			set.call(session, queue, JSON.stringify({ orderIds: ['third-light', 'demo-track'], currentId: 'demo-track' }));
			set.call(session, 'history-fixture-seeded', 'yes');
			localStorage.setItem(resume, JSON.stringify({ 'demo-track': 31.25 }));
		}
		const nodes = new WeakMap<Element, number>();
		let serial = 0;
		const probe: Probe = { blocked: 'none', read: (key) => get.call(session, key), plays: 0, pauses: 0, loads: 0, seeks: 0, sources: 0,
			identity: (node) => { if (!nodes.has(node)) nodes.set(node, ++serial); return nodes.get(node)!; } };
		(window as Instrumented).__historyProbe = probe;
		new MutationObserver((changes) => { probe.sources += changes.filter((change) => change.target instanceof HTMLMediaElement).length; })
			.observe(document, { subtree: true, attributes: true, attributeFilter: ['src'] });
		Storage.prototype.getItem = function (key) { if (this === session && probe.blocked === 'all') throw Error('denied'); return get.call(this, key); };
		Storage.prototype.setItem = function (key, value) { if (this === session && probe.blocked !== 'none') throw Error('denied'); return set.call(this, key, value); };
		const time = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime')!;
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', { ...time, set(value: number) { probe.seeks++; time.set!.call(this, value); } });
		HTMLMediaElement.prototype.play = function () { probe.plays++; return Promise.resolve(); };
		HTMLMediaElement.prototype.pause = function () { probe.pauses++; };
		HTMLMediaElement.prototype.load = function () { probe.loads++; };
	}, { history: HISTORY, queue: QUEUE, resume: RESUME, seed: baseline });
	await page.goto('/demo/player');
	await expect(page.getByTestId('recent-item')).toHaveCount(2);
	await expect.poll(() => page.evaluate(() => !!(window as Instrumented).__nntuneHistoryFixture)).toBe(true);
}
async function clear(page: Page) { await page.evaluate(() => (window as Instrumented).__nntuneHistoryFixture.clear()); }
async function raw(page: Page) { return page.evaluate((key) => (window as Instrumented).__historyProbe.read(key), HISTORY); }
async function snapshot(page: Page) {
	return page.evaluate((key) => {
		const p = (window as Instrumented).__historyProbe;
		const audio = document.querySelector<HTMLAudioElement>('[data-testid="demo-audio"]')!;
		return { id: (window as Instrumented).__nntuneHistoryFixture.id, node: p.identity(audio), source: audio.getAttribute('src'), time: audio.currentTime, paused: audio.paused,
			plays: p.plays, pauses: p.pauses, loads: p.loads, seeks: p.seeks, sources: p.sources,
			queue: [...document.querySelectorAll('[data-testid="queue-item"]')].map((x) => x.outerHTML),
			status: document.querySelector('[data-testid="player-status"]')?.textContent,
			now: document.querySelector('[data-testid="now-playing"]')?.textContent, resume: localStorage.getItem(key) };
	}, RESUME);
}
async function empty(page: Page) {
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	await expect(page.getByTestId('recently-played-empty')).toContainText(/\S/);
}
test.afterEach(async ({ page }) => { expect(calls.get(page) ?? []).toEqual([]); });

test('real mounted store clears without remount and records again', async ({ page }) => {
	await open(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect.poll(async () => JSON.parse((await raw(page))!)[0]).toEqual({ trackId: 'demo-track', playedAt: new Date('2026-01-01T12:00:00Z').getTime() });
	const before = await snapshot(page);
	await clear(page);
	await empty(page);
	expect(await raw(page)).toBe('[]');
	expect(await snapshot(page)).toEqual(before);
	await page.getByTestId('queue-select-third-light').click();
	await expect.poll(async () => JSON.parse((await raw(page))!)[0]?.trackId).toBe('third-light');
	const after = await snapshot(page);
	expect(after.id).toBe(before.id);
	// Switching tracks intentionally replaces the keyed audio element; clear did not.
	expect(after.node).not.toBe(before.node);
});

for (const mode of ['writes', 'all'] as const) {
	test(`loaded store remains empty with ${mode} denied and stale native bytes`, async ({ page }) => {
		await open(page);
		await page.getByTestId('queue-select-demo-track').click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		const before = await snapshot(page);
		const bytes = await raw(page);
		expect(JSON.parse(bytes!).length).toBeGreaterThan(0);
		await page.evaluate((value) => { (window as Instrumented).__historyProbe.blocked = value; }, mode);
		await clear(page);
		await clear(page);
		await empty(page);
		expect(await page.evaluate(() => (window as Instrumented).__nntuneHistoryFixture.entries())).toEqual([]);
		expect(await raw(page)).toBe(bytes);
		expect(await snapshot(page)).toEqual(before);
	});
}

test('clear persists through reload and replaces mount', async ({ page }) => {
	await open(page);
	const before = await snapshot(page);
	await clear(page);
	expect(await raw(page)).toBe('[]');
	await page.reload();
	await empty(page);
	await expect.poll(() => page.evaluate(() => (window as Instrumented).__nntuneHistoryFixture?.id)).toBeTruthy();
	expect((await snapshot(page)).id).not.toBe(before.id);
	expect(await raw(page)).toBe('[]');
});

test('a retained operation rejects after the route unmounts', async ({ page }) => {
	await open(page);
	await page.evaluate(() => {
		(window as Instrumented).__oldHistoryFixture = (window as Instrumented).__nntuneHistoryFixture;
		const link = document.createElement('a');
		link.id = 'fixture-navigation';
		link.href = '/';
		link.textContent = 'Navigate';
		document.body.append(link);
	});
	await page.locator('#fixture-navigation').click();
	await expect(page).toHaveURL(/\/$/);
	await expect.poll(() => page.evaluate(() => '__nntuneHistoryFixture' in window)).toBe(false);
	expect(await page.evaluate(async () => {
		try { await (window as Instrumented).__oldHistoryFixture!.clear(); return false; }
		catch { return true; }
	})).toBe(true);
});

test('real mutation stays in the originating tab', async ({ page }) => {
	await open(page);
	const other = await page.context().newPage();
	try {
		await open(other);
		const before = await snapshot(page);
		const otherBytes = await raw(other);
		await clear(page);
		await empty(page);
		await other.waitForTimeout(100);
		expect(await raw(other)).toBe(otherBytes);
		await expect(other.getByTestId('recent-item')).toHaveCount(2);
		expect(await snapshot(page)).toEqual(before);
		expect(await page.evaluate((key) => localStorage.getItem(key), HISTORY)).toBeNull();
		expect(calls.get(other)).toEqual([]);
	} finally { await other.close(); }
});

for (const colorScheme of ['light', 'dark'] as const) {
	test(`mobile ${colorScheme} real store mutation preserves transport`, async ({ browser, baseURL }, info) => {
		const context = await browser.newContext({ baseURL, viewport: { width: 375, height: 812 }, hasTouch: true, isMobile: true, colorScheme });
		const page = await context.newPage();
		try {
			await open(page);
			const replay = page.getByTestId('recent-replay-third-light');
			await expect(replay).toBeVisible();
			const box = await replay.boundingBox();
			expect(box).not.toBeNull();
			expect(box!.width).toBeGreaterThanOrEqual(24);
			expect(box!.height).toBeGreaterThanOrEqual(24);
			expect(box!.x).toBeGreaterThanOrEqual(0);
			expect(box!.x + box!.width).toBeLessThanOrEqual(375);
			await info.attach(`history-fixture-${colorScheme}`, { body: await page.getByTestId('recently-played').screenshot(), contentType: 'image/png' });
			const before = await snapshot(page);
			await clear(page);
			await empty(page);
			expect(await raw(page)).toBe('[]');
			expect(await snapshot(page)).toEqual(before);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			expect(calls.get(page)).toEqual([]);
		} finally { await context.close(); }
	});
}
