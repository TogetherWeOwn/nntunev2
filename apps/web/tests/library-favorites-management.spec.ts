import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned, paired with favorites-management-acceptance.test.ts.
 * Implement the feature on this handoff without editing these protected files.
 *
 * Human-facing contract (no implementation/source-shape checks):
 * - A labeled native view/filter combobox with All tracks / Favorites only.
 * - Existing favorite-item rows retain their FIRST queue-add control (the
 *   protected favorites-local suite already relies on that stable neighbor),
 *   plus a distinct button labeled Unstar {title} or Remove {title} from favorites.
 * - Clear local favorites opens an IN-PAGE confirmation with explicit Confirm
 *   and Cancel buttons. No window.confirm. Cancellation never saves; confirmation
 *   saves an empty whole favorite-ID set, including IDs absent from this library.
 * - Visible matched/total counts (e.g. "2 of 4 tracks"), a Show all tracks reset
 *   for zero matches, and concise polite management-action feedback in the shelf.
 * No new test IDs are needed: only existing hooks and accessible controls.
 *
 * Breaks caught: index/title matching, filtering before ordering, stale counts,
 * persisted filter, removal bubbling into queue-add, queue/current reset,
 * storage re-read losing pending changes, partial clear, auth leakage, focus
 * falling to body, fetch-on-filter, stale/error replacement and refresh chatter.
 * All network is intercepted at the context boundary; synthetic fixtures only.
 * Playback evidence is queue/current/transport plus native media-call/state
 * invariance, not a claim of real streaming or audible playback.
 */
const ITEMS = [
	{ id: 'manage-zulu', title: 'Zulu Song', artist: 'Beta' },
	{ id: 'manage-alpha', title: 'Alpha Song', artist: 'Zulu' },
	{ id: 'manage-mike', title: 'Mike Song', artist: 'Alpha' },
	{ id: 'manage-bravo', title: 'Bravo Song', artist: 'Delta' }
];
const ALL = ['manage-zulu', 'manage-alpha', 'manage-mike', 'manage-bravo'];
const AUTH = 'nntune.library.auth-context.v1';
const TRANSITION = 'nntune.library.auth-transition.v1';
const BASE = 'nntune.library.favorites.v1';
const ACCOUNT = 'management-account';
const KEY = `${BASE}.${ACCOUNT}`;
const SEEDED = ['manage-mike', 'manage-zulu', 'hidden-library-id'];
type Reply = { items: unknown; status: number; hold?: Promise<void> };
type Harness = { reply: Reply; requests: string[]; errors: string[] };

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function shelf(page: Page): Locator { return page.getByTestId('favorites-section'); }
function favorite(page: Page, id: string): Locator {
	return page.locator(`[data-testid="favorite-item"][data-id="${id}"]`);
}
function star(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}
function unstar(page: Page, id: string): Locator {
	return favorite(page, id).getByRole('button', { name: /unstar|remove .+ from favorites/i });
}
function view(page: Page): Locator {
	return page.getByRole('combobox', { name: /show|filter|view/i });
}
function clear(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear local favorites', exact: true });
}
function cancel(page: Page): Locator {
	return page.getByRole('button', { name: /^(cancel|keep (?:my |local )?favorites)$/i });
}
function confirm(page: Page): Locator {
	return page.getByRole('button', { name: /^(confirm(?: clear(?: local)? favorites)?|yes,? clear(?: local)? favorites|clear (?:all )?favorites now)$/i });
}
function feedback(page: Page): Locator {
	return shelf(page).locator('[role="status"], [aria-live="polite"]').filter({ hasText: /removed|unstarred|cleared|cancelled|canceled|kept/i });
}
async function ids(page: Page, hook = 'library-item'): Promise<(string | null)[]> {
	return page.getByTestId(hook).evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}
async function expectRows(page: Page, want: string[]) {
	await expect.poll(() => ids(page)).toEqual(want);
}
async function expectShelf(page: Page, want: string[]) {
	await expect.poll(async () => (await ids(page, 'favorite-item')).sort()).toEqual([...want].sort());
}
async function stored(page: Page): Promise<string[]> {
	return page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? '[]'), KEY);
}
async function count(page: Page, matched: number, total = ITEMS.length) {
	await expect(page.getByText(new RegExp(`\\b${matched}\\s*(?:of|/)\\s*${total}\\s+(?:loaded\\s+)?tracks?\\b`, 'i'))).toBeVisible();
}
async function mode(page: Page, label: 'All tracks' | 'Favorites only') {
	await expect(view(page), 'transient All tracks/Favorites only selector is missing').toBeVisible();
	await view(page).selectOption({ label });
	await expect(view(page).locator('option:checked')).toHaveText(label);
}

async function setup(page: Page, initial = SEEDED, options: { initialReadFailure?: boolean } = {}): Promise<Harness> {
	const requests: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (['fetch', 'xhr', 'media'].includes(request.resourceType()) ||
			url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/') || request.method() !== 'GET') {
			requests.push(`${request.method()} ${url.pathname}${url.search}`);
		}
	});
	await page.addInitScript(({ auth, key, initial, account, initialReadFailure }) => {
		// Seed once per browser context, not on reload (otherwise reload can
		// falsely pass after a broken save or clear).
		if (sessionStorage.getItem('test.management.seeded') !== 'yes') {
			sessionStorage.setItem(auth, account);
			localStorage.setItem(key, JSON.stringify(initial));
			localStorage.setItem('nntune.library.favorites.v1.other-account', '["other-private-id"]');
			localStorage.setItem('test.unrelated.preference', 'unchanged');
			sessionStorage.setItem('test.management.seeded', 'yes');
		}
		const calls = { play: 0, pause: 0 };
		Object.assign(window, { __managementMedia: calls });
		for (const method of ['play', 'pause'] as const) {
			const original = HTMLMediaElement.prototype[method];
			Object.defineProperty(HTMLMediaElement.prototype, method, { configurable: true, value: function (this: HTMLMediaElement) {
				calls[method] += 1;
				return original.call(this);
			} });
		}
		if (initialReadFailure) {
			const original = Storage.prototype.getItem;
			Storage.prototype.getItem = function (key) {
				if (this === localStorage && key.startsWith('nntune.library.favorites.v1.')) {
					throw new DOMException('Synthetic favorites read refusal', 'SecurityError');
				}
				return original.call(this, key);
			};
		}
	}, { auth: AUTH, key: KEY, initial, account: ACCOUNT, initialReadFailure: options.initialReadFailure ?? false });
	const reply: Reply = { items: ITEMS, status: 200 };
	await page.route('**/api/library', async (route) => {
		if (reply.hold) await reply.hold;
		await route.fulfill({ status: reply.status, json: reply.items });
	});
	return { reply, requests, errors };
}

async function open(page: Page) {
	await page.goto('/library');
	await expectRows(page, ALL);
}

async function queueState(page: Page) {
	return page.evaluate(() => {
		const rows = [...document.querySelectorAll('[data-testid="queue-item"]')];
		const current = rows.findIndex((item) => item.getAttribute('aria-current') === 'true');
		return {
			ids: rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id')),
			currentIndex: current,
			currentId: current < 0 ? null : rows[current]?.getAttribute('data-track-id') ?? rows[current]?.getAttribute('data-id'),
			now: document.querySelector('[data-testid="now-playing"]')?.textContent,
			transports: ['player-next', 'player-prev'].map((hook) => document.querySelector(`[data-testid="${hook}"]`)?.hasAttribute('disabled') ?? null),
			media: [...document.querySelectorAll('audio, video')].map((element) => {
				const media = element as HTMLMediaElement;
				return { src: media.currentSrc, paused: media.paused, ended: media.ended };
			}),
			calls: Reflect.get(window, '__managementMedia')
		};
	});
}
async function seedQueue(page: Page) {
	for (const id of ['manage-mike', 'manage-zulu', 'manage-alpha']) await row(page, id).click();
	await page.getByRole('button', { name: 'Move Alpha Song up', exact: true }).click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	const state = await queueState(page);
	expect(state.ids).toEqual(['manage-mike', 'manage-alpha', 'manage-zulu']);
	expect(state.currentId).toBe('manage-alpha');
	expect(state.currentIndex).toBe(1);
}
async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
async function localAction(page: Page, harness: Harness, action: () => Promise<unknown>) {
	const before = await queueState(page);
	const requests = [...harness.requests];
	await action();
	await settle(page);
	expect(await queueState(page), 'favorite management must preserve exact queue/order/current/index/transport/media intent').toEqual(before);
	expect(harness.requests, 'favorite management must issue no request, including GET refreshes').toEqual(requests);
	expect(harness.errors).toEqual([]);
}
async function clearPrompt(page: Page, h: Harness) {
	await expect(clear(page), 'Clear local favorites trigger is missing').toBeVisible();
	await localAction(page, h, () => clear(page).click());
	await expect(cancel(page), 'clear requires explicit in-page cancellation').toBeVisible();
	await expect(confirm(page), 'clear requires an explicit in-page confirmation').toBeVisible();
}
async function expectStableFocus(page: Page, targets: Locator[]) {
	let focused = false;
	for (const target of targets) {
		for (const element of await target.all()) {
			if (await element.evaluate((node) => node === document.activeElement)) focused = true;
		}
	}
	expect(focused, 'focus must land on a surviving management control, shelf heading or empty state, not body/a queue-add button').toBe(true);
}
async function expectActionFeedback(page: Page, action: RegExp) {
	await expect(feedback(page)).toHaveCount(1);
	await expect(feedback(page)).toContainText(action);
	await expect(feedback(page)).not.toHaveAttribute('aria-live', 'assertive');
	expect((await feedback(page).innerText()).length, 'action feedback should be concise').toBeLessThanOrEqual(180);
}
async function fault(page: Page, operation: 'getItem' | 'setItem') {
	await page.evaluate(({ operation, base }) => {
		const original = Storage.prototype[operation];
		Reflect.set(window, '__managementRestoreStorage', () => { Storage.prototype[operation] = original; });
		if (operation === 'getItem') {
			const get = Storage.prototype.getItem;
			Storage.prototype.getItem = function (key) {
				if (this === localStorage && key.startsWith(`${base}.`)) throw new DOMException('Synthetic read refusal', 'SecurityError');
				return get.call(this, key);
			};
		} else {
			const set = Storage.prototype.setItem;
			Storage.prototype.setItem = function (key, value) {
				if (this === localStorage && key.startsWith(`${base}.`)) throw new DOMException('Synthetic write refusal', 'QuotaExceededError');
				set.call(this, key, value);
			};
		}
	}, { operation, base: BASE });
}
async function restore(page: Page) {
	await page.evaluate(() => Reflect.get(window, '__managementRestoreStorage')());
}
async function tabTo(page: Page, target: Locator) {
	await expect(target).toBeVisible();
	for (let i = 0; i < 80; i += 1) {
		if (await target.evaluate((node) => node === document.activeElement)) break;
		await page.keyboard.press('Tab');
	}
	await expect(target, 'control must be reachable by Tab').toBeFocused();
	const visibleFocus = await target.evaluate((node) => {
		const css = getComputedStyle(node);
		return node.matches(':focus-visible') && (
			(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) || css.boxShadow !== 'none'
		);
	});
	expect(visibleFocus, 'keyboard focus needs a visible outline or focus ring').toBe(true);
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) {
			return route.fulfill({ status: 404, json: { error: 'unmocked endpoint' } });
		}
		return route.continue();
	});
});

test.afterEach(async ({ page }, info) => {
	if (!page.isClosed()) {
		await info.attach('favorites-management-runtime', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
	}
});

test('[FM-B00] control: scoped favorites and distinct shelf queue-add still dedupe and preserve arranged current', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	await expectShelf(page, ['manage-mike', 'manage-zulu']);
	await seedQueue(page);
	const before = await queueState(page);
	await favorite(page, 'manage-mike').locator('button, [role="button"], [role="option"]').first().click();
	expect(await queueState(page)).toEqual(before);
	await star(page, 'manage-bravo').click();
	await favorite(page, 'manage-bravo').locator('button, [role="button"], [role="option"]').first().click();
	expect((await queueState(page)).ids).toEqual(['manage-mike', 'manage-alpha', 'manage-zulu', 'manage-bravo']);
	expect((await queueState(page)).currentId).toBe('manage-alpha');
	expect(h.requests).toEqual(['GET /api/library']);
});

test('[FM-B01] favorites-only preserves each chosen ordering and updates main-row star counts without touching queue/cache', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	await seedQueue(page);
	const snapshot = await page.evaluate((auth) => sessionStorage.getItem(`nntune.library.snapshot.v1.${sessionStorage.getItem(auth)}`), AUTH);
	expect(snapshot).not.toBeNull();
	await expect(view(page)).toBeVisible();
	await expect(view(page).locator('option:checked')).toHaveText('All tracks');
	for (const [label, want] of [
		['Original order', ['manage-zulu', 'manage-mike']],
		['Title A–Z', ['manage-mike', 'manage-zulu']],
		['Artist A–Z', ['manage-mike', 'manage-zulu']]
	] as const) {
		await localAction(page, h, () => page.getByRole('combobox', { name: /order|sort/i }).selectOption({ label }));
		await localAction(page, h, () => mode(page, 'Favorites only'));
		await expectRows(page, [...want]);
		await count(page, 2);
		await localAction(page, h, () => mode(page, 'All tracks'));
		await count(page, 4);
	}
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await localAction(page, h, () => star(page, 'manage-mike').click());
	await expectRows(page, ['manage-zulu']);
	await count(page, 1);
	await localAction(page, h, () => mode(page, 'All tracks'));
	await localAction(page, h, () => star(page, 'manage-alpha').click());
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await expectRows(page, ['manage-zulu', 'manage-alpha']);
	await count(page, 2);
	expect(await page.evaluate((auth) => sessionStorage.getItem(`nntune.library.snapshot.v1.${sessionStorage.getItem(auth)}`), AUTH)).toBe(snapshot);
	expect((await stored(page)).sort()).toEqual(['hidden-library-id', 'manage-alpha', 'manage-zulu']);
});

test('[FM-B02] zero loaded matches show truthful counts and a useful reset, even with hidden favorites', async ({ page }) => {
	const h = await setup(page, ['hidden-library-id']);
	await open(page);
	await seedQueue(page);
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await expectRows(page, []);
	await count(page, 0);
	const reset = page.getByRole('button', { name: /show all tracks|reset (?:filter|view)/i });
	await expect(reset).toBeVisible();
	await localAction(page, h, () => reset.click());
	await expectRows(page, ALL);
	await expect(view(page).locator('option:checked')).toHaveText('All tracks');
	expect(await stored(page)).toEqual(['hidden-library-id']);
	await clearPrompt(page, h);
	await localAction(page, h, () => confirm(page).click());
	expect(await stored(page), 'hidden-only favorites must remain clearable without a visible shelf row').toEqual([]);
});

test('[FM-B03] shelf unstar while filtered preserves hidden IDs and queue; focus survives last visible removal', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	await seedQueue(page);
	await localAction(page, h, () => mode(page, 'Favorites only'));
	const shelfIds = await ids(page, 'favorite-item') as string[];
	const first = shelfIds[0] as string;
	const next = shelfIds[1] as string;
	await tabTo(page, unstar(page, first));
	await localAction(page, h, () => page.keyboard.press('Enter'));
	await expectShelf(page, [next]);
	await expectRows(page, [next]);
	await count(page, 1);
	await expectStableFocus(page, [unstar(page, next), shelf(page).getByRole('heading')]);
	await expectActionFeedback(page, /removed|unstarred/i);
	expect((await stored(page)).sort()).toEqual(['hidden-library-id', next].sort());
	await tabTo(page, unstar(page, next));
	await localAction(page, h, () => page.keyboard.press('Space'));
	await expectShelf(page, []);
	await expectRows(page, []);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expectStableFocus(page, [shelf(page).getByRole('heading'), page.getByTestId('favorites-empty')]);
	expect(await stored(page)).toEqual(['hidden-library-id']);
});

test('[FM-B04] Clear requires in-page confirmation; Cancel is a no-save and Confirm clears hidden IDs with stable focus', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	await seedQueue(page);
	let dialogs = 0;
	page.on('dialog', async (dialog) => { dialogs += 1; await dialog.dismiss(); });
	const raw = await page.evaluate((key) => localStorage.getItem(key), KEY);
	await page.evaluate((key) => {
		const original = Storage.prototype.setItem;
		const writes: string[] = [];
		Reflect.set(window, '__managementFavoriteWrites', writes);
		Storage.prototype.setItem = function (name, value) {
			if (this === localStorage && name === key) writes.push(value);
			original.call(this, name, value);
		};
	}, KEY);
	await clearPrompt(page, h);
	await expectShelf(page, ['manage-mike', 'manage-zulu']);
	expect(await page.evaluate((key) => localStorage.getItem(key), KEY)).toBe(raw);
	await localAction(page, h, () => cancel(page).click());
	await expect(cancel(page)).toBeHidden();
	await expect(confirm(page)).toBeHidden();
	await expect(clear(page)).toBeFocused();
	expect(await page.evaluate(() => Reflect.get(window, '__managementFavoriteWrites'))).toEqual([]);
	await expectShelf(page, ['manage-mike', 'manage-zulu']);
	await clearPrompt(page, h);
	await localAction(page, h, () => confirm(page).click());
	await expectShelf(page, []);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	expect(await stored(page)).toEqual([]);
	expect(await page.evaluate((auth) => sessionStorage.getItem(auth), AUTH)).toBe(ACCOUNT);
	expect(await page.evaluate((base) => localStorage.getItem(`${base}.other-account`), BASE)).toBe('["other-private-id"]');
	expect(await page.evaluate(() => localStorage.getItem('test.unrelated.preference'))).toBe('unchanged');
	await expectStableFocus(page, [clear(page), view(page), shelf(page).getByRole('heading'), page.getByTestId('favorites-empty')]);
	await expectActionFeedback(page, /cleared/i);
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await count(page, 0);
	await expectRows(page, []);
	expect(dialogs, 'native dialogs are not an in-page confirmation').toBe(0);
});

for (const operation of ['getItem', 'setItem'] as const) {
	test(`[FM-B05:${operation}] unstar uses visit state during storage failure; a recovered save retains every pending and hidden ID`, async ({ page }) => {
		const h = await setup(page);
		await open(page);
		await seedQueue(page);
		await fault(page, operation);
		await expect(unstar(page, 'manage-zulu'), 'shelf needs an explicit unstar button').toBeVisible();
		await localAction(page, h, () => unstar(page, 'manage-zulu').click());
		await expectShelf(page, ['manage-mike']);
		await localAction(page, h, () => star(page, 'manage-alpha').click());
		await expectShelf(page, ['manage-mike', 'manage-alpha']);
		await localAction(page, h, () => mode(page, 'Favorites only'));
		await expectRows(page, ['manage-alpha', 'manage-mike']);
		await restore(page);
		await localAction(page, h, () => mode(page, 'All tracks'));
		await localAction(page, h, () => star(page, 'manage-bravo').click());
		expect((await stored(page)).sort()).toEqual(['hidden-library-id', 'manage-alpha', 'manage-bravo', 'manage-mike']);
		await page.reload();
		await expectRows(page, ALL);
		await expectShelf(page, ['manage-alpha', 'manage-bravo', 'manage-mike']);
	});
}

test('[FM-B06] Clear remains effective for this visit when writes fail; recovery never resurrects hidden or visible IDs', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	await seedQueue(page);
	await fault(page, 'setItem');
	await clearPrompt(page, h);
	await localAction(page, h, () => confirm(page).click());
	await expectShelf(page, []);
	expect(await stored(page), 'a failed write must not pretend it persisted').toEqual(SEEDED);
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await expectRows(page, []);
	await count(page, 0);
	await restore(page);
	await localAction(page, h, () => mode(page, 'All tracks'));
	await localAction(page, h, () => star(page, 'manage-alpha').click());
	expect(await stored(page)).toEqual(['manage-alpha']);
	await page.reload();
	await expectRows(page, ALL);
	await expectShelf(page, ['manage-alpha']);
});

test('[FM-B07] unavailable initial favorite reads leave a usable empty view and current-visit starring/filter/clear', async ({ page }) => {
	const h = await setup(page, SEEDED, { initialReadFailure: true });
	await open(page);
	await expectShelf(page, []);
	await localAction(page, h, () => star(page, 'manage-alpha').click());
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await expectRows(page, ['manage-alpha']);
	await count(page, 1);
	await clearPrompt(page, h);
	await localAction(page, h, () => confirm(page).click());
	await expectRows(page, []);
	await expectShelf(page, []);
});

test('[FM-B08] reload resets to All tracks without persisting a new schema or losing scoped favorites', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	const keys = await page.evaluate(() => ({ local: Object.keys(localStorage).sort(), session: Object.keys(sessionStorage).sort() }));
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await expectRows(page, ['manage-zulu', 'manage-mike']);
	expect(await page.evaluate(() => ({ local: Object.keys(localStorage).sort(), session: Object.keys(sessionStorage).sort() }))).toEqual(keys);
	await page.reload();
	await expectRows(page, ALL);
	await expect(view(page).locator('option:checked')).toHaveText('All tracks');
	await count(page, 4);
	expect(await stored(page)).toEqual(SEEDED);
});

test('[FM-B09] refresh retains filter/focus/queue and does not announce management actions; failed refresh keeps stale subset usable', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	await seedQueue(page);
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await expect(unstar(page, 'manage-zulu')).toBeVisible();
	await localAction(page, h, () => unstar(page, 'manage-zulu').click());
	await expectActionFeedback(page, /removed|unstarred/i);
	const announcement = await feedback(page).innerText();
	const before = await queueState(page);
	await view(page).focus();
	h.reply.items = ITEMS.map((item) => item.id === 'manage-mike' ? { ...item, title: 'Mike Song refreshed' } : item);
	let release!: () => void;
	h.reply.hold = new Promise<void>((resolve) => { release = resolve; });
	await page.getByRole('button', { name: 'Refresh', exact: true }).evaluate((node: HTMLButtonElement) => node.click());
	await expectRows(page, ['manage-mike']);
	await expect(view(page)).toBeFocused();
	release();
	await expect(row(page, 'manage-mike')).toContainText('Mike Song refreshed');
	await expect(view(page)).toBeFocused();
	await expect(view(page).locator('option:checked')).toHaveText('Favorites only');
	await count(page, 1);
	await expect(feedback(page)).toHaveText(announcement);
	expect(await queueState(page)).toEqual(before);
	h.reply.hold = undefined;
	h.reply.status = 503;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expectRows(page, ['manage-mike']);
	await localAction(page, h, () => unstar(page, 'manage-mike').click());
	await expectRows(page, []);
	await count(page, 0);
	expect(h.requests).toEqual(['GET /api/library', 'GET /api/library', 'GET /api/library']);
});

test('[FM-B10] loading/error/retry and a genuinely empty library keep their existing semantics', async ({ page }) => {
	const h = await setup(page);
	let release!: () => void;
	h.reply.hold = new Promise<void>((resolve) => { release = resolve; });
	h.reply.status = 503;
	await page.goto('/library');
	await expect(page.getByRole('status').filter({ hasText: /loading/i })).toBeVisible();
	await expectRows(page, []);
	release();
	await expect(page.getByRole('alert')).toBeVisible();
	h.reply.hold = undefined;
	h.reply.status = 200;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectRows(page, ALL);
	await localAction(page, h, () => mode(page, 'Favorites only'));
	h.reply.items = [];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('status').filter({ hasText: /library is empty/i })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Import an NZB' })).toHaveAttribute('href', '/library/import');
	await expectRows(page, []);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[FM-B11] auth transition purges pending management state/old IDs and resets the next account to All tracks', async ({ page }) => {
	const h = await setup(page);
	await open(page);
	await seedQueue(page);
	await fault(page, 'setItem');
	await localAction(page, h, () => mode(page, 'Favorites only'));
	await expect(unstar(page, 'manage-zulu')).toBeVisible();
	await localAction(page, h, () => unstar(page, 'manage-zulu').click());
	await expectShelf(page, ['manage-mike']);
	await page.evaluate((base) => localStorage.setItem(base, '["legacy-id"]'), BASE);
	const other = await page.context().newPage();
	await other.goto('/login');
	await other.evaluate((key) => { localStorage.setItem(key, 'management-auth-transition'); localStorage.removeItem(key); }, TRANSITION);
	await other.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expectRows(page, []);
	await expectShelf(page, []);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await page.evaluate(({ key, base }) => [localStorage.getItem(key), localStorage.getItem(base)], { key: KEY, base: BASE })).toEqual([null, null]);
	h.reply.items = [{ id: 'next-account', title: 'Next Account Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectRows(page, ['next-account']);
	await expect(view(page).locator('option:checked')).toHaveText('All tracks');
	await expectShelf(page, []);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await restore(page);
	expect(await page.evaluate((auth) => sessionStorage.getItem(auth), AUTH)).not.toBe(ACCOUNT);
});

for (const colorScheme of ['light', 'dark'] as const) {
	for (const width of [390, 1280]) {
		test(`[FM-B12:${colorScheme}:${width}] keyboard filter/unstar/confirmation remains visible and queue-safe`, async ({ page }) => {
			await page.setViewportSize({ width, height: 844 });
			await page.emulateMedia({ colorScheme });
			const h = await setup(page);
			await open(page);
			await seedQueue(page);
			await tabTo(page, view(page));
			await localAction(page, h, async () => {
				await page.keyboard.press('ArrowDown');
				await page.keyboard.press('Enter');
			});
			await expect(view(page).locator('option:checked')).toHaveText('Favorites only');
			await count(page, 2);
			await tabTo(page, unstar(page, 'manage-zulu'));
			await test.info().attach('filtered-keyboard-focus', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
			await localAction(page, h, () => page.keyboard.press('Space'));
			await expectRows(page, ['manage-mike']);
			await tabTo(page, clear(page));
			await localAction(page, h, () => page.keyboard.press('Enter'));
			await tabTo(page, cancel(page));
			await localAction(page, h, () => page.keyboard.press('Space'));
			await expect(clear(page)).toBeFocused();
			await localAction(page, h, () => page.keyboard.press('Enter'));
			await tabTo(page, confirm(page));
			await test.info().attach('in-page-confirmation-keyboard-focus', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
			await localAction(page, h, () => page.keyboard.press('Enter'));
			await expectShelf(page, []);
			await count(page, 0);
			await expectActionFeedback(page, /cleared/i);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'management must not create horizontal overflow').toBe(true);
		});
	}
}
