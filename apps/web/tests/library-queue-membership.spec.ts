import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance-owned; the paired Builder continues this branch, not this bank.
 * Only the external library response is mocked; projection, queue, selection,
 * favorites, refresh and auth teardown execute in the real /library page.
 * All metadata is synthetic. No provider, catalog, media or remote API traffic.
 *
 * UI contract: a separately labeled native Queue state/Queue membership select,
 * options All / Queued / Not queued (values all / queued / not-queued). The
 * existing Show (all/favorites) select is independent. New component-local
 * hooks: library-displayed-count (visible final/loaded counts), and
 * queue-membership-announcement (one polite atomic final-count region).
 * Empty-state reset says All queue states or Reset queue filter, and resets
 * membership alone. Wording otherwise stays flexible and loaded-scope honest.
 *
 * Mutations caught: title/current-index membership, stale queue projections,
 * filter-as-mutation, bulk using unfiltered data, selection loss, auth leaks,
 * persisted view, focus falling to body, duplicate/per-row announcements.
 * Pure real-route projections: src/routes/library/queue-membership-acceptance.test.ts.
 * Existing acceptance banks remain unchanged; ask their owner about a conflict.
 *
 * Run from repo root:
 * CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *   tests/library-queue-membership.spec.ts --workers=1
 */
const ITEMS = [
	{ id: 'member-zulu', title: 'Shared Tune', artist: 'North Harbor' },
	{ id: 'member-alpha', title: 'Alpha Song', artist: 'North Harbor' },
	{ id: 'member-twin', title: 'Shared Tune', artist: 'South Bay' },
	{ id: 'member-literal', title: 'What.*Ever', artist: 'C++ Players' },
	{ id: 'member-fallback' }
];
const ALL = ['member-zulu', 'member-alpha', 'member-twin', 'member-literal', 'member-fallback'];
type Mode = 'all' | 'queued' | 'not-queued';
type Reply = { items: unknown; status?: number };

async function openLibrary(page: Page, reply: Reply = { items: ITEMS }) {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return route.fulfill({
			status: reply.status ?? 200, contentType: 'application/json', body: JSON.stringify(reply.items)
		});
	});
	await page.goto('/library');
	await expectView(page, ALL);
	return { requests: () => requests };
}

function row(page: Page, id: string) {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function enqueue(page: Page, id: string) {
	return row(page, id).locator('button.nt-library-row');
}
function membership(page: Page) {
	return page.getByRole('combobox', { name: /queue state|queue membership/i });
}
async function choose(page: Page, mode: Mode) {
	await expect(membership(page), 'separately labeled queue-membership control is required').toBeVisible();
	await membership(page).selectOption(mode);
	await expect(membership(page)).toHaveValue(mode);
}
function find(page: Page) {
	return page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
}
async function expectView(page: Page, ids: string[]) {
	await expect.poll(() => page.getByTestId('library-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-id'))
	)).toEqual(ids);
}
async function expectQueue(page: Page, ids: string[]) {
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
	)).toEqual(ids);
}
async function expectCounts(page: Page, displayed: number, loaded = 5) {
	// Dedicated summary: queue positions, loaded total and selected counts cannot
	// accidentally satisfy the displayed-count assertion elsewhere on the page.
	const count = page.getByTestId('library-displayed-count');
	await expect(count).toBeVisible();
	await expect(count).toContainText(new RegExp(`\\b${displayed}\\b`));
	await expect(count).toContainText(new RegExp(`\\b${loaded}\\b`));
	await expect(count).toContainText(/loaded/i);
}
function reset(page: Page) {
	return page.getByRole('button', { name: /all queue states|reset queue filter/i });
}
async function expectUsableFocus(page: Page) {
	expect(await page.evaluate(() => {
		const active = document.activeElement as HTMLElement | null;
		return !!active && active !== document.body && active !== document.documentElement &&
			active.isConnected && active.getClientRects().length > 0 && !active.matches(':disabled');
	})).toBe(true);
}
async function authTransition(page: Page) {
	const tab = await page.context().newPage();
	await tab.goto('/login');
	await tab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'membership-fixture');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await tab.close();
}
async function presentationSnapshot(page: Page) {
	return page.evaluate(() => ({
		local: Object.entries(localStorage).sort(), session: Object.entries(sessionStorage).sort(),
		queue: Array.from(document.querySelectorAll('[data-testid="queue-item"]')).map((item) => ({
			id: item.getAttribute('data-track-id') ?? item.getAttribute('data-id'),
			current: item.getAttribute('aria-current')
		})),
		playing: document.querySelector('[data-testid="now-playing"]')?.textContent,
		favorites: Array.from(document.querySelectorAll('[data-testid="favorite-item"]'))
			.map((item) => item.getAttribute('data-id')),
		selected: document.querySelector('[data-testid="selection-count"]')?.textContent,
		queueAnnouncement: document.querySelector('[data-testid="queue-announcement"]')?.textContent
	}));
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('MEM-00: baseline real library and stable-ID metadata-only queue work', async ({ page }) => {
	const api = await openLibrary(page);
	await enqueue(page, 'member-zulu').click();
	await expectQueue(page, ['member-zulu']);
	await expect(row(page, 'member-zulu').getByTestId('library-queued-badge')).toBeVisible();
	await expect(row(page, 'member-twin').getByTestId('library-queued-badge')).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	expect(api.requests()).toBe(1);
});

test('MEM-01: labeled membership modes are separate from All/Favorites', async ({ page }) => {
	await openLibrary(page);
	await expect(membership(page)).toBeVisible();
	await expect(membership(page)).toHaveValue('all');
	await expect(membership(page).getByRole('option')).toHaveText(['All', 'Queued', 'Not queued']);
	await expect(page.getByRole('combobox', { name: 'Show', exact: true })).toHaveValue('all');
	await expectCounts(page, 5);
	await choose(page, 'queued');
	await expectView(page, []);
	await expectCounts(page, 0);
	await choose(page, 'not-queued');
	await expectView(page, ALL);
	await choose(page, 'all');
	await expectView(page, ALL);
});

test('MEM-02: duplicate titles, id fallback and noncurrent queue entries use stable IDs', async ({ page }) => {
	await openLibrary(page);
	await enqueue(page, 'member-zulu').click();
	await enqueue(page, 'member-fallback').click();
	await choose(page, 'queued');
	await expectView(page, ['member-zulu', 'member-fallback']);
	for (const id of ['member-zulu', 'member-fallback']) {
		await expect(row(page, id).getByTestId('library-queued-badge')).toBeVisible();
		await expect(enqueue(page, id)).toHaveAttribute('aria-pressed', 'true');
	}
	await page.getByTestId('player-next').click();
	await expectView(page, ['member-zulu', 'member-fallback']);
	await choose(page, 'not-queued');
	await expectView(page, ['member-alpha', 'member-twin', 'member-literal']);
	await expect(page.getByTestId('library-queued-badge')).toHaveCount(0);
});

test('MEM-03: membership composes with Favorites, artist find and descending order', async ({ page }) => {
	await openLibrary(page);
	for (const id of ['member-zulu', 'member-alpha', 'member-twin']) {
		await row(page, id).getByRole('button', { name: /^Star / }).click();
	}
	await enqueue(page, 'member-zulu').click();
	await page.getByRole('combobox', { name: 'Show', exact: true }).selectOption('favorites');
	await page.getByRole('combobox', { name: 'Order tracks', exact: true }).selectOption('title-desc');
	await find(page).fill(' NORTH HARBOR ');
	await choose(page, 'not-queued');
	await expectView(page, ['member-alpha']);
	await expectCounts(page, 1);
	await choose(page, 'all');
	await expectView(page, ['member-zulu', 'member-alpha']);
	await expect(find(page)).toHaveValue(' NORTH HARBOR ');
	await expect(page.getByRole('combobox', { name: 'Show', exact: true })).toHaveValue('favorites');
});

test('MEM-04: punctuation remains literal, with no remote search', async ({ page }) => {
	const api = await openLibrary(page);
	await enqueue(page, 'member-literal').click();
	await choose(page, 'queued');
	await find(page).fill('.*');
	await expectView(page, ['member-literal']);
	await choose(page, 'not-queued');
	await expectView(page, []);
	await find(page).fill('MEMBER-FALLBACK');
	await expectView(page, ['member-fallback']);
	expect(api.requests()).toBe(1);
});

test('MEM-05: changing membership never mutates queue, stars, selection, storage or media', async ({ page }) => {
	await openLibrary(page);
	await enqueue(page, 'member-zulu').click();
	await row(page, 'member-alpha').getByRole('checkbox').check();
	await row(page, 'member-alpha').getByRole('button', { name: 'Star Alpha Song', exact: true }).click();
	const before = await presentationSnapshot(page);
	const requests: string[] = [];
	page.on('request', (request) => requests.push(request.url()));
	await page.evaluate(() => {
		const calls: string[] = [];
		(window as unknown as { membershipMediaCalls: string[] }).membershipMediaCalls = calls;
		HTMLMediaElement.prototype.play = function () { calls.push('play'); return Promise.resolve(); };
		HTMLMediaElement.prototype.load = function () { calls.push('load'); };
		const writes: string[] = [];
		(window as unknown as { membershipWrites: string[] }).membershipWrites = writes;
		for (const name of ['setItem', 'removeItem', 'clear'] as const) {
			const original = Storage.prototype[name];
			Storage.prototype[name] = function (...args: string[]) {
				writes.push(`storage:${name}`);
				return Reflect.apply(original, this, args);
			};
		}
		for (const name of ['add', 'put', 'delete', 'clear'] as const) {
			const original = IDBObjectStore.prototype[name];
			IDBObjectStore.prototype[name] = function (...args: unknown[]) {
				writes.push(`indexeddb:${name}`);
				return Reflect.apply(original, this, args);
			};
		}
	});
	for (const mode of ['queued', 'not-queued', 'all'] as const) await choose(page, mode);
	expect(await presentationSnapshot(page)).toEqual(before);
	expect(requests).toEqual([]);
	expect(await page.evaluate(() => (window as unknown as { membershipMediaCalls: string[] }).membershipMediaCalls)).toEqual([]);
	expect(await page.evaluate(() => (window as unknown as { membershipWrites: string[] }).membershipWrites)).toEqual([]);
	await expect(page.locator('audio, video')).toHaveCount(0);
});

test('MEM-06: enqueue removes the unqueued row immediately and leaves usable focus', async ({ page }) => {
	await openLibrary(page);
	await choose(page, 'not-queued');
	const button = enqueue(page, 'member-alpha');
	await button.focus();
	await button.press('Enter');
	await expectView(page, ['member-zulu', 'member-twin', 'member-literal', 'member-fallback']);
	await expectCounts(page, 4);
	await expectQueue(page, ['member-alpha']);
	await expectUsableFocus(page);
	await choose(page, 'queued');
	await expectView(page, ['member-alpha']);
});

test('MEM-07: removal updates queued projection/count without stealing surviving control focus', async ({ page }) => {
	await openLibrary(page);
	await enqueue(page, 'member-zulu').click();
	await enqueue(page, 'member-alpha').click();
	await choose(page, 'queued');
	const remove = page.getByRole('button', { name: 'Remove Alpha Song from queue', exact: true });
	await remove.focus();
	await remove.press('Enter');
	await expectView(page, ['member-zulu']);
	await expectCounts(page, 1);
	await expectQueue(page, ['member-zulu']);
	await expectUsableFocus(page);
});

test('MEM-08: Clear queue empties queued view without changing the chosen mode', async ({ page }) => {
	await openLibrary(page);
	await enqueue(page, 'member-alpha').click();
	await choose(page, 'queued');
	const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
	await clear.focus();
	await clear.press('Enter');
	await expectView(page, []);
	await expectCounts(page, 0);
	await expect(membership(page)).toHaveValue('queued');
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expectUsableFocus(page);
	await choose(page, 'not-queued');
	await expectView(page, ALL);
});

test('MEM-09: hidden selected choices survive membership and live queue changes', async ({ page }) => {
	await openLibrary(page);
	await row(page, 'member-alpha').getByRole('checkbox').check();
	await row(page, 'member-twin').getByRole('checkbox').check();
	await enqueue(page, 'member-alpha').click();
	await choose(page, 'queued');
	await expect(page.getByTestId('selection-count')).toHaveText(/2 selected/);
	await expect(page.getByTestId('selection-hidden-count')).toContainText(/1/);
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await expect(page.getByTestId('selection-hidden-count')).toContainText(/2/);
	await choose(page, 'all');
	await expect(row(page, 'member-alpha').getByRole('checkbox')).toBeChecked();
	await expect(row(page, 'member-twin').getByRole('checkbox')).toBeChecked();
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
});

test('MEM-10: Add displayed consumes only the composed unqueued rows, once, in display order', async ({ page }) => {
	await openLibrary(page);
	await enqueue(page, 'member-twin').click();
	await enqueue(page, 'member-zulu').click();
	await find(page).fill('north');
	await page.getByRole('combobox', { name: 'Order tracks', exact: true }).selectOption('title-desc');
	await choose(page, 'not-queued');
	await expectView(page, ['member-alpha']);
	const add = page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true });
	await add.click();
	await expectQueue(page, ['member-twin', 'member-zulu', 'member-alpha']);
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', 'member-twin');
	await expectView(page, []);
	await expect(add).toBeDisabled();
	await expect(page.getByTestId('queue-announcement')).toHaveText('Added 1 track to the queue.');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await choose(page, 'queued');
	await expectView(page, ['member-zulu', 'member-alpha']);
	await expect(add).toBeDisabled();
});

test('MEM-11: same-account refresh preserves mode and uses refreshed loaded metadata', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	const api = await openLibrary(page, reply);
	await enqueue(page, 'member-alpha').click();
	await choose(page, 'queued');
	reply.items = [
		{ id: 'member-alpha', title: 'Updated Song', artist: 'Fresh Band' },
		{ id: 'member-fresh', title: 'Fresh Tune' }
	];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, ['member-alpha']);
	await expect(row(page, 'member-alpha')).toContainText('Updated Song');
	await expectCounts(page, 1, 2);
	await expect(membership(page)).toHaveValue('queued');
	await choose(page, 'not-queued');
	await expectView(page, ['member-fresh']);
	expect(api.requests()).toBe(2);
});

test('MEM-12: failed refresh keeps mode, stale projection and selection; Retry reconciles', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	await openLibrary(page, reply);
	await row(page, 'member-alpha').getByRole('checkbox').check();
	await enqueue(page, 'member-alpha').click();
	await choose(page, 'queued');
	reply.status = 503;
	reply.items = { error: 'temporary' };
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toBeVisible();
	await expectView(page, ['member-alpha']);
	await expect(membership(page)).toHaveValue('queued');
	await expect(row(page, 'member-alpha').getByRole('checkbox')).toBeChecked();
	reply.status = 200;
	reply.items = ITEMS;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['member-alpha']);
	await expect(membership(page)).toHaveValue('queued');
});

test('MEM-13: honest zero-match copy and reset clear only queue membership', async ({ page }) => {
	await openLibrary(page);
	await row(page, 'member-alpha').getByRole('checkbox').check();
	await row(page, 'member-alpha').getByRole('button', { name: 'Star Alpha Song', exact: true }).click();
	await page.getByRole('combobox', { name: 'Show', exact: true }).selectOption('favorites');
	await find(page).fill('alpha');
	await choose(page, 'queued');
	await expectView(page, []);
	await expect(page.locator('main').getByText(/no .*queued|no .*match/i).first()).toBeVisible();
	await expect(page.locator('main')).not.toContainText(/your library is empty/i);
	await expect(reset(page)).toBeVisible();
	await reset(page).focus();
	await reset(page).press('Enter');
	await expect(membership(page)).toHaveValue('all');
	await expectView(page, ['member-alpha']);
	await expect(find(page)).toHaveValue('alpha');
	await expect(page.getByRole('combobox', { name: 'Show', exact: true })).toHaveValue('favorites');
	await expect(row(page, 'member-alpha').getByRole('checkbox')).toBeChecked();
	await expectUsableFocus(page);
});

test('MEM-14: keyboard operates the select and never enqueues or loses focus', async ({ page }) => {
	await openLibrary(page);
	await expect(membership(page)).toBeVisible();
	await membership(page).focus();
	await membership(page).press('Home');
	await membership(page).press('ArrowDown');
	await membership(page).press('Enter');
	await expect(membership(page)).toHaveValue('queued');
	await expect(membership(page)).toBeFocused();
	await expectView(page, []);
	await expectQueue(page, []);
	await membership(page).press('End');
	await membership(page).press('Enter');
	await expect(membership(page)).toHaveValue('not-queued');
	await expectView(page, ALL);
});

test.describe('narrow touch themes', () => {
	test.use({ viewport: { width: 320, height: 740 }, hasTouch: true });
	for (const theme of ['light', 'dark'] as const) {
		test(`MEM-15-${theme}: touch membership, counts and reset fit the viewport`, async ({ page }) => {
			await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
			await openLibrary(page);
			await expect(membership(page)).toBeVisible();
			await membership(page).tap();
			await choose(page, 'queued');
			await expectCounts(page, 0);
			await expect(reset(page)).toBeInViewport();
			await reset(page).tap();
			await expectView(page, ALL);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			const box = await membership(page).boundingBox();
			expect(box).not.toBeNull();
			expect(box!.x).toBeGreaterThanOrEqual(0);
			expect(box!.x + box!.width).toBeLessThanOrEqual(320);
		});
	}
});

test('MEM-16: account transition clears mode, rows, queue and pending selection', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	await openLibrary(page, reply);
	await row(page, 'member-alpha').getByRole('checkbox').check();
	await enqueue(page, 'member-alpha').click();
	await choose(page, 'queued');
	await authTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expectView(page, []);
	await expectQueue(page, []);
	reply.items = [{ id: 'member-next-account', title: 'Next Account Tune' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(membership(page)).toHaveValue('all');
	await expectView(page, ['member-next-account']);
	await expect(page.getByTestId('selection-count')).toHaveText(/0 selected/);
});

for (const status of [401, 403]) {
	test(`MEM-17-${status}: auth failure resets membership before next session Retry`, async ({ page }) => {
		const reply: Reply = { items: ITEMS };
		await openLibrary(page, reply);
		await choose(page, 'not-queued');
		reply.status = status;
		reply.items = { error: 'unauthorized' };
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByRole('alert')).toContainText(/signed out|access/i);
		await expectView(page, []);
		reply.status = 200;
		reply.items = [{ id: 'member-next-account', title: 'Next Account Tune' }];
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(membership(page)).toHaveValue('all');
		await expectView(page, ['member-next-account']);
	});
}

test('MEM-18: a reload resets transient membership and does not persist it', async ({ page }) => {
	await openLibrary(page);
	const storage = await page.evaluate(() => ({ local: Object.entries(localStorage), session: Object.entries(sessionStorage) }));
	await choose(page, 'queued');
	expect(await page.evaluate(() => ({ local: Object.entries(localStorage), session: Object.entries(sessionStorage) }))).toEqual(storage);
	await page.reload();
	await expect(membership(page)).toHaveValue('all');
	await expectView(page, ALL);
});

test('MEM-19: one atomic polite final count, no queue-announcement or per-row chatter', async ({ page }) => {
	await openLibrary(page);
	await enqueue(page, 'member-alpha').click();
	const queueAnnouncement = await page.getByTestId('queue-announcement').textContent();
	const announcement = page.getByTestId('queue-membership-announcement');
	await expect(announcement).toHaveAttribute('aria-live', 'polite');
	await expect(announcement).toHaveAttribute('aria-atomic', 'true');
	await announcement.evaluate((element) => {
		const messages: string[] = [];
		(window as unknown as { membershipMessages: string[] }).membershipMessages = messages;
		new MutationObserver(() => {
			const message = element.textContent?.trim();
			if (message) messages.push(message);
		}).observe(element, { childList: true, subtree: true, characterData: true });
	});
	await choose(page, 'queued');
	await expect(announcement).toContainText(/\b1\b.*\b5\b|\b1\b.*loaded/i);
	// Two animation frames flush rendering/observer delivery, not a sleep/poll.
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
	const messages = await page.evaluate(() => (window as unknown as { membershipMessages: string[] }).membershipMessages);
	expect(messages).toHaveLength(1);
	await expect(page.getByTestId('queue-announcement')).toHaveText(queueAnnouncement ?? '');
	await expect(page.getByTestId('library-item').locator('[aria-live], [role="status"], [role="alert"]')).toHaveCount(0);
	// Scope to main: the framework's #svelte-announcer is intentionally assertive.
	await expect(page.locator('main [aria-live="assertive"]')).toHaveCount(0);
});

test('MEM-20: an in-flight old-account refresh cannot restore mode or old rows after teardown', async ({ page }) => {
	await openLibrary(page);
	await choose(page, 'not-queued');
	let release!: () => void;
	let started!: () => void;
	let settled!: () => void;
	const requested = new Promise<void>((resolve) => { started = resolve; });
	const pending = new Promise<void>((resolve) => { release = resolve; });
	const responseSettled = new Promise<void>((resolve) => { settled = resolve; });
	await page.route('**/api/library', async (route) => {
		started();
		await pending;
		try {
			await route.fulfill({ contentType: 'application/json', body: JSON.stringify(ITEMS) });
		} finally {
			settled();
		}
	});
	try {
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await requested;
		await authTransition(page);
		await expect(page.getByRole('alert')).toContainText(/session changed/i);
	} finally {
		release();
	}
	await responseSettled;
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
	await expectView(page, []);
	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) => route.fulfill({
		contentType: 'application/json', body: JSON.stringify([{ id: 'member-new', title: 'New Account Tune' }])
	}));
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['member-new']);
	await expect(membership(page)).toHaveValue('all');
	await expectQueue(page, []);
});

test('MEM-21: membership does not carry to another tab or restore after route teardown', async ({ page, context }) => {
	await openLibrary(page);
	await choose(page, 'queued');
	const tab = await context.newPage();
	await openLibrary(tab);
	await expect(membership(tab)).toHaveValue('all');
	await tab.close();
	await page.goto('/login');
	await page.goto('/library');
	await expect(membership(page)).toHaveValue('all');
	await expectView(page, ALL);
});
