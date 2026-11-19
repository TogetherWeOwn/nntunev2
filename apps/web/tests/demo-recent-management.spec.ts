import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: only acceptance-owner-authorized amendments may change this bank.
 * Contract-first browser coverage for the surviving recent-history actions:
 * Replay and Clear, plus real play recording. The history-row Forget and Add
 * controls are retired; their absence is checked in the companion browser suite
 * and exact-ID forgetting remains covered by the shared store tests, not here.
 * Clear removes all entries; it is not claimed to delete one stable ID.
 *
 * Unknown history IDs may be filtered (existing policy) or shown unavailable;
 * either way explain unavailable history without ever substituting a track.
 * Keyboard (Enter and Space) Clear keeps focus on the surviving enabled Clear
 * button, not the document body or a removed node. No exact DOM layout, focus
 * message or copy pin. Mount, play and seek emit no management announcement,
 * and reload never replays stale feedback.
 *
 * Media loading stays pending locally. Only the native transport boundary is
 * mocked; queue/history/session persistence and the Svelte page remain real.
 * All backend/telemetry/external requests are rejected and asserted absent,
 * including for the second tab and the mobile contexts.
 * Synthetic media events are not evidence of real streams, audio output, OS
 * media controls, or OS IME. This suite adds no operational API or CLI.
 *
 * Breaks caught: mutation of queue/current/time/source/resume during Clear;
 * storage rollback or stale-byte resurrection under write/read denial;
 * focus loss; spurious live messages; recording or Replay regressions;
 * inaccessible/overlapping narrow-screen actions; cross-tab propagation.
 */
const HISTORY = 'nntune:demo-recently-played:v1';
const QUEUE = 'nntune:demo-queue:v1';
const RESUME = 'nntune:resume:v1';
type Entry = { trackId: string; playedAt: number };
const ENTRIES: Entry[] = [
	{ trackId: 'third-light', playedAt: 300 },
	{ trackId: 'second-signal', playedAt: 200 },
	{ trackId: 'demo-track', playedAt: 100 }
];
type Probe = {
	blocked: 'none' | 'writes' | 'all';
	plays: number;
	pauses: number;
	loads: number;
	seeks: number;
	sourceChanges: number;
	read: (key: string) => string | null;
	identity: (node: Element) => number;
};

function row(page: Page, id: string): Locator {
	return page.getByTestId('recent-item').filter({ has: page.locator(`[data-testid="recent-played-at-${id}"]`) });
}
function section(page: Page): Locator { return page.getByTestId('recently-played'); }

async function entries(page: Page): Promise<Entry[]> {
	return page.evaluate((key) => {
		const probe = (window as unknown as { __recentManagement: Probe }).__recentManagement;
		return JSON.parse(probe.read(key) ?? '[]') as Entry[];
	}, HISTORY);
}
async function ids(page: Page, hook: 'recent-item' | 'queue-item'): Promise<string[]> {
	return page.getByTestId(hook).evaluateAll((items) => items.map((item) => item.getAttribute('data-track-id')!));
}
async function liveMessages(page: Page): Promise<string[]> {
	return section(page).locator('[role="status"], [aria-live="polite"]').evaluateAll((regions) =>
		regions.map((node) => node.textContent?.trim() ?? '').filter(Boolean)
	);
}


async function open(page: Page, options: {
	history?: Entry[];
	order?: string[];
	currentId?: string | null;
	intent?: 'selected' | 'playing' | 'paused';
} = {}): Promise<void> {
	const calls: string[] = [];
	backendCalls.set(page, calls);
	let localOrigin: string | null = null;
	await page.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		localOrigin ??= url.origin; // First request is the configured local page navigation.
		if (url.origin !== localOrigin) {
			calls.push(`external ${request.method()} ${url.pathname}`);
			await route.abort();
		} else if (request.method() !== 'GET' || ['xhr', 'fetch'].includes(request.resourceType())) {
			calls.push(`${request.method()} ${url.pathname}`);
			await route.abort();
		} else if (url.pathname === '/api/demo/audio') {
			// Pending native media request: no remote provider and no garbage bytes.
		} else {
			await route.fallback();
		}
	});
	await page.clock.setFixedTime(new Date('2026-01-01T12:00:00Z'));
	await page.addInitScript(({ historyKey, queueKey, resumeKey, history, order, currentId }) => {
		const session = sessionStorage;
		const nativeGet = Storage.prototype.getItem;
		const nativeSet = Storage.prototype.setItem;
		// Seed once so reload exercises the actual saved result, not the fixture.
		if (nativeGet.call(session, 'recent-management-fixture') === null) {
			nativeSet.call(session, historyKey, JSON.stringify(history));
			nativeSet.call(session, queueKey, JSON.stringify({ orderIds: order, currentId }));
			nativeSet.call(session, 'recent-management-fixture', 'seeded');
			localStorage.setItem(resumeKey, JSON.stringify({ 'demo-track': 31.25, 'second-signal': 12.5 }));
		}
		const nodes = new WeakMap<Element, number>();
		let serial = 0;
		const probe: Probe = {
			blocked: 'none', plays: 0, pauses: 0, loads: 0, seeks: 0, sourceChanges: 0,
			read: (key) => nativeGet.call(session, key),
			identity: (node) => {
				if (!nodes.has(node)) nodes.set(node, ++serial);
				return nodes.get(node)!;
			}
		};
		Object.assign(window, { __recentManagement: probe });
		new MutationObserver((changes) => {
			probe.sourceChanges += changes.filter((change) => change.target instanceof HTMLMediaElement).length;
		}).observe(document, { subtree: true, attributes: true, attributeFilter: ['src'] });
		Storage.prototype.getItem = function (key) {
			if (this === session && probe.blocked === 'all') throw new Error('synthetic storage denial');
			return nativeGet.call(this, key);
		};
		Storage.prototype.setItem = function (key, value) {
			if (this === session && probe.blocked !== 'none') throw new Error('synthetic storage denial');
			return nativeSet.call(this, key, value);
		};
		const time = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime')!;
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			...time,
			set(value: number) { probe.seeks += 1; time.set!.call(this, value); }
		});
		HTMLMediaElement.prototype.play = function () { probe.plays += 1; return Promise.resolve(); };
		HTMLMediaElement.prototype.pause = function () { probe.pauses += 1; };
		HTMLMediaElement.prototype.load = function () { probe.loads += 1; };
	}, {
		historyKey: HISTORY, queueKey: QUEUE, resumeKey: RESUME,
		history: options.history ?? ENTRIES,
		order: options.order ?? ['third-light', 'demo-track'], currentId: options.currentId === undefined ? 'demo-track' : options.currentId
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(options.order?.length ?? 2);
	await expect(section(page)).toContainText(/demo/i);
	await expect(page.getByTestId('recently-played-clear')).toBeVisible();
	if (options.intent && options.intent !== 'selected') {
		await page.getByTestId('queue-select-demo-track').click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await page.getByTestId('demo-audio').evaluate((el) => {
			Object.defineProperty(el, 'paused', { value: false, configurable: true });
			el.dispatchEvent(new Event('play'));
			el.dispatchEvent(new Event('playing'));
		});
		if (options.intent === 'paused') {
			await page.getByTestId('demo-audio').evaluate((el) => {
				Object.defineProperty(el, 'paused', { value: true, configurable: true });
				el.dispatchEvent(new Event('pause'));
			});
			await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		}
	}
	if ((options.order ?? ['third-light', 'demo-track']).length > 0) {
		await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
			el.currentTime = 31.25;
			el.dispatchEvent(new Event('timeupdate'));
		});
	}
}
const backendCalls = new WeakMap<Page, string[]>();
test.afterEach(async ({ page }) => {
	expect(backendCalls.get(page) ?? [], 'no backend, telemetry or external requests').toEqual([]);
});

async function transport(page: Page) {
	return page.evaluate((resumeKey) => {
		const probe = (window as unknown as { __recentManagement: Probe }).__recentManagement;
		const audio = document.querySelector<HTMLAudioElement>('[data-testid="demo-audio"]');
		const buttons = [...document.querySelectorAll('[data-testid="queue-item"] button[aria-current]')];
		const current = buttons.find((button) => button.getAttribute('aria-current') === 'true');
		const rows = [...document.querySelectorAll('[data-testid="queue-item"]')];
		return {
			currentId: current?.closest('li')?.getAttribute('data-track-id') ?? null,
			index: rows.findIndex((item) => item.contains(current ?? null)),
			nowPlaying: document.querySelector('[data-testid="now-playing"]')?.textContent,
			status: document.querySelector('[data-testid="player-status"]')?.textContent,
			node: audio ? probe.identity(audio) : null,
			source: audio?.getAttribute('src') ?? null, time: audio?.currentTime ?? null,
			paused: audio?.paused ?? null, plays: probe.plays, pauses: probe.pauses,
			loads: probe.loads, seeks: probe.seeks, sourceChanges: probe.sourceChanges,
			resume: localStorage.getItem(resumeKey)
		};
	}, RESUME);
}


const clear = (page: Page): Locator => page.getByTestId('recently-played-clear');
async function expectUsableClearFocus(page: Page): Promise<void> {
	await expect(clear(page), 'surviving enabled Clear keeps focus').toBeFocused();
	await expect(clear(page)).toBeEnabled();
}
async function expectEmptyHistory(page: Page): Promise<void> {
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
	await expect(page.getByTestId('recently-played-empty')).toContainText(/\S/);
}

test('RM-B-04 Clear on a one-row history by Space gives usable empty state and focus', async ({ page }) => {
	await open(page, { history: [ENTRIES[2]!] });
	const before = await transport(page);
	await expect(clear(page)).toBeEnabled();
	await clear(page).focus();
	await page.keyboard.press('Space');
	await expectEmptyHistory(page);
	expect(await entries(page)).toEqual([]);
	expect(await transport(page)).toEqual(before);
	expect(await ids(page, 'queue-item')).toEqual(['third-light', 'demo-track']);
	await expectUsableClearFocus(page);
});

test('RM-B-05 actual play records a history entry; Replay still explicitly plays', async ({ page }) => {
	await open(page, { history: [ENTRIES[0]!, ENTRIES[1]!] });
	await expect(row(page, 'demo-track')).toHaveCount(0);
	await expect(row(page, 'third-light')).toHaveCount(1);
	await expect(row(page, 'second-signal')).toHaveCount(1);
	await page.clock.setFixedTime(new Date('2026-01-01T12:01:00Z'));
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect.poll(() => entries(page)).toEqual([
		{ trackId: 'demo-track', playedAt: new Date('2026-01-01T12:01:00Z').getTime() },
		ENTRIES[0], ENTRIES[1]
	]);
	const before = await transport(page);
	await page.getByTestId('recent-replay-second-signal').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect((await transport(page)).plays).toBeGreaterThan(before.plays);
	const after = await entries(page);
	expect(after[0]?.trackId).toBe('second-signal');
	expect(after.filter((entry) => entry.trackId === 'second-signal')).toHaveLength(1);
});

test('RM-B-06 Clear persists empty without clearing queue or resume', async ({ page }) => {
	await open(page);
	const before = await transport(page);
	const queue = await ids(page, 'queue-item');
	await expect(clear(page)).toBeVisible();
	await clear(page).click();
	await expectEmptyHistory(page);
	expect(await entries(page)).toEqual([]);
	expect(await page.evaluate((key) => (window as unknown as { __recentManagement: Probe }).__recentManagement.read(key), HISTORY)).toBe('[]');
	expect(await transport(page)).toEqual(before);
	expect(await ids(page, 'queue-item')).toEqual(queue);
	await page.reload();
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	expect(await entries(page)).toEqual([]);
	expect(await ids(page, 'queue-item')).toEqual(queue);
});

for (const blocked of ['writes', 'all'] as const) {
	test(`RM-B-07-${blocked} blocked session storage cannot undo current-visit Clear`, async ({ page }) => {
		await open(page, { intent: 'playing' });
		const before = await transport(page);
		const queue = await ids(page, 'queue-item');
		const history = await entries(page);
		expect(history.length).toBeGreaterThan(0);
		const persisted = await page.evaluate((key) => (window as unknown as { __recentManagement: Probe }).__recentManagement.read(key), HISTORY);
		await page.evaluate((mode) => {
			(window as unknown as { __recentManagement: Probe }).__recentManagement.blocked = mode;
		}, blocked);
		await expect(clear(page)).toBeVisible();
		await clear(page).focus();
		await page.keyboard.press('Enter');
		await expectEmptyHistory(page);
		expect(await transport(page)).toEqual(before);
		expect(await ids(page, 'queue-item')).toEqual(queue);
		// Quota/read denial is real: persisted bytes remain unchanged.
		expect(await entries(page)).toEqual(history);
		expect(await page.evaluate((key) => (window as unknown as { __recentManagement: Probe }).__recentManagement.read(key), HISTORY)).toBe(persisted);
		// Another Clear invocation refreshes state; stale bytes must not resurrect rows.
		await clear(page).click();
		await expectEmptyHistory(page);
		expect(await transport(page)).toEqual(before);
		expect(await ids(page, 'queue-item')).toEqual(queue);
		expect(await entries(page)).toEqual(history);
		expect(await page.evaluate((key) => (window as unknown as { __recentManagement: Probe }).__recentManagement.read(key), HISTORY)).toBe(persisted);
	});
}

test('RM-B-08 unrecognized stable ID is explained and never resolves to another track', async ({ page }) => {
	await open(page, { history: [
		{ trackId: 'second-signal-missing', playedAt: 400 }, ...ENTRIES
	] });
	const before = await transport(page);
	const history = await entries(page);
	await expect(section(page)).toContainText(/unavailable|unrecognized|unknown|no longer available|not available/i);
	expect(await transport(page)).toEqual(before);
	expect(await entries(page)).toEqual(history);
	expect(await ids(page, 'queue-item')).toEqual(['third-light', 'demo-track']);
});

test('RM-B-09 no feedback on load/play/time; keyboard Clear remains session-only on reload', async ({ page }) => {
	await open(page, { intent: 'playing' });
	expect(await liveMessages(page)).toEqual([]);
	const before = await transport(page);
	const queue = await ids(page, 'queue-item');
	await expect(clear(page)).toBeVisible();
	await clear(page).focus();
	await page.keyboard.press('Enter');
	await expectEmptyHistory(page);
	expect(await entries(page)).toEqual([]);
	expect(await transport(page)).toEqual(before);
	expect(await ids(page, 'queue-item')).toEqual(queue);
	await expectUsableClearFocus(page);
	const messages = await liveMessages(page);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 33;
		el.dispatchEvent(new Event('timeupdate'));
	});
	expect(await liveMessages(page)).toEqual(messages);
	expect(await entries(page)).toEqual([]);
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	await page.reload();
	await expect.poll(() => ids(page, 'queue-item')).toEqual(['third-light', 'demo-track']);
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	expect(await entries(page)).toEqual([]);
	expect(await liveMessages(page)).toEqual([]);
	// Session state must not subscribe to another tab's storage events. The payload
	// is a valid, nonempty history that conflicts with the empty cleared state.
	await page.evaluate(({ key, value }) => window.dispatchEvent(new StorageEvent('storage', {
		key, newValue: value, storageArea: localStorage
	})), { key: HISTORY, value: JSON.stringify([{ trackId: 'third-light', playedAt: 999 }]) });
	expect(await entries(page)).toEqual([]);
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	expect(await ids(page, 'recent-item')).toEqual([]);
});

test('RM-B-11 Clear stays in this tab, never writes account/local history or syncs another tab', async ({ page }) => {
	await open(page);
	const before = await transport(page);
	const queue = await ids(page, 'queue-item');
	const other = await page.context().newPage();
	try {
		await open(other, { history: [ENTRIES[0]!, ENTRIES[1]!], order: ['third-light'], currentId: 'third-light' });
		await expect(clear(page)).toBeVisible();
		await clear(page).click();
		await expectEmptyHistory(page);
		expect(await entries(page)).toEqual([]);
		expect(await transport(page)).toEqual(before);
		expect(await ids(page, 'queue-item')).toEqual(queue);
		// Let a mistakenly broadcast storage/channel event reach the second tab.
		await other.waitForTimeout(100);
		expect(await entries(other)).toEqual([ENTRIES[0], ENTRIES[1]]);
		expect(await ids(other, 'recent-item')).toEqual(['third-light', 'second-signal']);
		expect(await page.evaluate((key) => localStorage.getItem(key), HISTORY)).toBeNull();
		expect(backendCalls.get(other)).toEqual([]);
	} finally { await other.close(); }
});

for (const colorScheme of ['light', 'dark'] as const) {
	test(`RM-B-10-${colorScheme} narrow touch actions are reachable and Clear does not transport`, async ({ browser, baseURL }, testInfo) => {
		const context = await browser.newContext({ baseURL, viewport: { width: 375, height: 812 }, hasTouch: true, isMobile: true, colorScheme });
		const page = await context.newPage();
		try {
			await open(page);
			const before = await transport(page);
			const queue = await ids(page, 'queue-item');
			const replay = page.getByTestId('recent-replay-second-signal');
			await expect(replay).toBeVisible();
			await expect(clear(page)).toBeVisible();
			for (const control of [replay, clear(page)]) {
				await expect(control).toBeEnabled();
				await control.scrollIntoViewIfNeeded();
				const box = await control.boundingBox();
				expect(box).not.toBeNull();
				expect(box!.x).toBeGreaterThanOrEqual(0);
				expect(box!.x + box!.width).toBeLessThanOrEqual(375);
				expect(box!.width).toBeGreaterThanOrEqual(24);
				expect(box!.height).toBeGreaterThanOrEqual(24);
			}
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			// Populated-history evidence, with Replay and Clear both present.
			await testInfo.attach(`recent-management-${colorScheme}`, { body: await section(page).screenshot(), contentType: 'image/png' });
			await clear(page).tap();
			await expectEmptyHistory(page);
			expect(await entries(page)).toEqual([]);
			expect(await transport(page)).toEqual(before);
			expect(await ids(page, 'queue-item')).toEqual(queue);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			expect(backendCalls.get(page)).toEqual([]);
		} finally { await context.close(); }
	});
}
