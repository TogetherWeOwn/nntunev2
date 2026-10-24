import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: continue with production changes, not edits to this file.
 * Contract-first browser half for per-entry history management. Existing hooks
 * are reused; NEW actions are located by accessible name INSIDE the existing
 * recent row, not by a new controller/path/test-id convention. Names must
 * communicate Add to queue versus Forget, and the row supplies track context.
 * Feedback wording is flexible: one nonempty polite region in the history
 * section, action-only (not mount/play/seek), added versus already queued.
 *
 * Unknown history IDs may be filtered (existing policy) or shown unavailable;
 * either way explain unavailable history without ever substituting a track.
 * Focus may move to any surviving enabled history button, not the document
 * body or removed node. No exact DOM layout, focus-target or message pin.
 *
 * Media loading stays pending locally. Only the native transport boundary is
 * mocked; queue/history/session persistence and the Svelte page remain real.
 * All backend/telemetry/external requests are rejected and asserted absent.
 * Synthetic media events are not evidence of real streams, audio output, OS
 * media controls, or OS IME. This suite adds no operational API or CLI.
 *
 * Breaks caught: Replay accidentally reused for Add; duplicate add; mutation
 * of order/current/time/source/history; by-index forget; stale-ID substitution;
 * storage rollback; resume deletion; focus loss; duplicate live messages;
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
function add(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /add.*queue/i });
}
function forget(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /forget/i });
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
async function feedback(page: Page, meaning: RegExp): Promise<void> {
	await expect.poll(() => liveMessages(page)).toHaveLength(1);
	const messages = await liveMessages(page);
	expect(messages[0]).toMatch(meaning);
	await expect(section(page).locator('[role="alert"], [aria-live="assertive"]')).toHaveCount(0);
}

async function addedFeedback(page: Page): Promise<void> {
	await feedback(page, /(?:added|queued)/i);
	expect((await liveMessages(page))[0]).not.toMatch(/already|unavailable|failed/i);
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

for (const intent of ['selected', 'playing', 'paused'] as const) {
	test(`RM-B-01-${intent} enqueue missing track once without transport/history mutation`, async ({ page }) => {
		await open(page, { intent });
		const before = await transport(page);
		const history = await entries(page);
		await expect(add(page, 'second-signal'), 'separate accessible Add to queue action').toBeVisible();
		await add(page, 'second-signal').click();
		await expect.poll(() => ids(page, 'queue-item')).toEqual(['third-light', 'demo-track', 'second-signal']);
		expect(await transport(page)).toEqual(before);
		expect(await entries(page)).toEqual(history);
		await addedFeedback(page);
		// A second activation must be possible and report an honest no-op.
		await add(page, 'second-signal').click();
		await feedback(page, /already/i);
		expect(await ids(page, 'queue-item')).toEqual(['third-light', 'demo-track', 'second-signal']);
		expect(await transport(page)).toEqual(before);
		expect(await entries(page)).toEqual(history);
	});
}

test('RM-B-02 already queued current ID is an explained no-op, never Replay', async ({ page }) => {
	await open(page, { intent: 'playing' });
	const before = await transport(page);
	const history = await entries(page);
	await expect(add(page, 'demo-track')).toBeVisible();
	await add(page, 'demo-track').click();
	await feedback(page, /already/i);
	expect(await transport(page)).toEqual(before);
	expect(await entries(page)).toEqual(history);
	expect(await ids(page, 'queue-item')).toEqual(['third-light', 'demo-track']);
});

for (const target of ['second-signal', 'demo-track', 'third-light']) {
	test(`RM-B-03-${target} forget only chosen history ID, not queue/current/resume`, async ({ page }) => {
		await open(page, { intent: target === 'demo-track' ? 'playing' : 'selected' });
		const before = await transport(page);
		const history = await entries(page);
		await expect(forget(page, target), 'separate accessible Forget action').toBeVisible();
		await forget(page, target).focus();
		await page.keyboard.press('Enter');
		const expected = history.filter((entry) => entry.trackId !== target);
		await expect.poll(() => ids(page, 'recent-item')).toEqual(expected.map((entry) => entry.trackId));
		expect(await entries(page)).toEqual(expected);
		expect(await transport(page)).toEqual(before);
		expect(await ids(page, 'queue-item')).toEqual(['third-light', 'demo-track']);
		await feedback(page, /forgot|forgotten|removed/i);
		expect(await section(page).evaluate((el) => {
			const focused = document.activeElement;
			return focused instanceof HTMLButtonElement && !focused.disabled && el.contains(focused);
		}), 'focus stays on an enabled surviving history action').toBe(true);
		await page.reload();
		await expect.poll(() => ids(page, 'recent-item')).toEqual(expected.map((entry) => entry.trackId));
		expect(await entries(page)).toEqual(expected);
	});
}

test('RM-B-04 forget last row gives usable empty state and preserves Clear', async ({ page }) => {
	await open(page, { history: [ENTRIES[2]!] });
	const before = await transport(page);
	await expect(forget(page, 'demo-track')).toBeVisible();
	await forget(page, 'demo-track').focus();
	await page.keyboard.press('Space');
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
	await expect(page.getByTestId('recently-played-empty')).toContainText(/\S/);
	expect(await entries(page)).toEqual([]);
	expect(await transport(page)).toEqual(before);
	await expect(page.getByTestId('recently-played-clear')).toBeFocused();
});

test('RM-B-05 actual play re-adds a forgotten entry; Replay still explicitly plays', async ({ page }) => {
	await open(page);
	await expect(forget(page, 'demo-track')).toBeVisible();
	await forget(page, 'demo-track').click();
	await expect(row(page, 'demo-track')).toHaveCount(0);
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
	expect((await entries(page))[0]?.trackId).toBe('second-signal');
});

test('RM-B-06 Clear after one forget persists empty without clearing queue or resume', async ({ page }) => {
	await open(page);
	const before = await transport(page);
	await expect(forget(page, 'second-signal')).toBeVisible();
	await forget(page, 'second-signal').click();
	await page.getByTestId('recently-played-clear').click();
	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
	expect(await entries(page)).toEqual([]);
	expect(await transport(page)).toEqual(before);
	await page.reload();
	await expect(page.getByTestId('recent-item')).toHaveCount(0);
});

for (const blocked of ['writes', 'all'] as const) {
	test(`RM-B-07-${blocked} blocked session storage cannot undo current-visit forget or enqueue`, async ({ page }) => {
		await open(page, { intent: 'playing' });
		const before = await transport(page);
		const history = await entries(page);
		await page.evaluate((mode) => {
			(window as unknown as { __recentManagement: Probe }).__recentManagement.blocked = mode;
		}, blocked);
		await expect(forget(page, 'third-light')).toBeVisible();
		await forget(page, 'third-light').click();
		await expect.poll(() => ids(page, 'recent-item')).toEqual(history.filter((entry) => entry.trackId !== 'third-light').map((entry) => entry.trackId));
		await add(page, 'second-signal').click();
		await addedFeedback(page);
		expect(await ids(page, 'queue-item')).toEqual(['third-light', 'demo-track', 'second-signal']);
		await add(page, 'second-signal').click();
		await feedback(page, /already/i);
		expect(await transport(page)).toEqual(before);
		expect(await ids(page, 'recent-item')).toEqual(history.filter((entry) => entry.trackId !== 'third-light').map((entry) => entry.trackId));
		// Quota/read denial is real: persisted bytes remain unchanged.
		expect(await entries(page)).toEqual(history);
	});
}

test('RM-B-08 unrecognized stable ID is explained and never resolves to another track', async ({ page }) => {
	await open(page, { history: [
		{ trackId: 'second-signal-missing', playedAt: 400 }, ...ENTRIES
	] });
	const before = await transport(page);
	const history = await entries(page);
	await expect(section(page)).toContainText(/unavailable|unrecognized|unknown|no longer available|not available/i);
	const unavailable = row(page, 'second-signal-missing');
	if (await unavailable.count()) {
		const action = unavailable.getByRole('button', { name: /add.*queue/i });
		if (await action.count() && await action.isEnabled()) {
			await action.click();
			await feedback(page, /unavailable|unknown|not found|not available/i);
		}
	}
	expect(await transport(page)).toEqual(before);
	expect(await entries(page)).toEqual(history);
	expect(await ids(page, 'queue-item')).toEqual(['third-light', 'demo-track']);
});

test('RM-B-09 no action feedback on load/play/time; enqueue remains session-only on reload', async ({ page }) => {
	await open(page, { intent: 'playing' });
	expect(await liveMessages(page)).toEqual([]);
	await expect(add(page, 'second-signal')).toBeVisible();
	await add(page, 'second-signal').focus();
	await page.keyboard.press('Enter');
	await addedFeedback(page);
	const messages = await liveMessages(page);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 33;
		el.dispatchEvent(new Event('timeupdate'));
	});
	expect(await liveMessages(page)).toEqual(messages);
	const history = await entries(page);
	await page.reload();
	await expect.poll(() => ids(page, 'queue-item')).toEqual(['third-light', 'demo-track', 'second-signal']);
	expect(await entries(page)).toEqual(history);
	expect(await liveMessages(page)).toEqual([]);
	// Session state must not subscribe to another tab's storage events.
	await page.evaluate(({ key }) => window.dispatchEvent(new StorageEvent('storage', {
		key, newValue: '[]', storageArea: localStorage
	})), { key: HISTORY });
	expect(await entries(page)).toEqual(history);
});

test('RM-B-11 forget stays in this tab, never writes account/local history or syncs another tab', async ({ page }) => {
	await open(page);
	const other = await page.context().newPage();
	try {
		await open(other, { history: [ENTRIES[0]!], order: ['third-light'], currentId: 'third-light' });
		await expect(forget(page, 'third-light')).toBeVisible();
		await forget(page, 'third-light').click();
		await expect(row(page, 'third-light')).toHaveCount(0);
		// Let a mistakenly broadcast storage/channel event reach the second tab.
		await other.waitForTimeout(100);
		expect(await entries(other)).toEqual([ENTRIES[0]]);
		expect(await ids(other, 'recent-item')).toEqual(['third-light']);
		expect(await page.evaluate((key) => localStorage.getItem(key), HISTORY)).toBeNull();
		expect(backendCalls.get(other)).toEqual([]);
	} finally { await other.close(); }
});

for (const colorScheme of ['light', 'dark'] as const) {
	test(`RM-B-10-${colorScheme} narrow touch actions are distinct, reachable and do not transport`, async ({ browser, baseURL }, testInfo) => {
		const context = await browser.newContext({ baseURL, viewport: { width: 375, height: 812 }, hasTouch: true, isMobile: true, colorScheme });
		const page = await context.newPage();
		try {
			await open(page);
			const before = await transport(page);
			await expect(add(page, 'second-signal')).toBeVisible();
			await expect(forget(page, 'second-signal')).toBeVisible();
			await expect(page.getByTestId('recent-replay-second-signal')).toBeVisible();
			for (const control of [add(page, 'second-signal'), forget(page, 'second-signal')]) {
				await control.scrollIntoViewIfNeeded();
				const box = await control.boundingBox();
				expect(box).not.toBeNull();
				expect(box!.x).toBeGreaterThanOrEqual(0);
				expect(box!.x + box!.width).toBeLessThanOrEqual(375);
				expect(box!.width).toBeGreaterThanOrEqual(24);
				expect(box!.height).toBeGreaterThanOrEqual(24);
			}
			await add(page, 'second-signal').tap();
			await addedFeedback(page);
			await forget(page, 'second-signal').tap();
			await expect(row(page, 'second-signal')).toHaveCount(0);
			expect(await transport(page)).toEqual(before);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			await testInfo.attach(`recent-management-${colorScheme}`, { body: await section(page).screenshot(), contentType: 'image/png' });
			expect(backendCalls.get(page)).toEqual([]);
		} finally { await context.close(); }
	});
}
