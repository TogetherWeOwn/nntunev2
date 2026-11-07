import { writeFile } from 'node:fs/promises';
import { expect, test, type Browser, type Locator, type Page } from '@playwright/test';
import { openQueue } from './helpers/queue-host';
import { tabTo as boundedTabTo } from './support/tab-to';

/**
 * Keyboard-traversal diagnostics.
 *
 * D1 replays the desktop FM-B12 flow (library-favorites-management.spec.ts,
 * the desktop branch through the Unstar tab step) against the unchanged
 * fixture, storage seed and queue, once with the legacy bounded-80 tabTo
 * helper (copied verbatim below, observational only) and once with the shared
 * bounded helper (hard). Each lane runs in its own fresh browser context. Both
 * lanes record the actual Tab sequence by sampling the deep active element
 * after every real keyboard press; nothing here focuses or clicks the target.
 * D2-D4 are synthetic pages proving the shared helper's failure diagnostics.
 *
 * The setup/seed/queue helpers below are verbatim copies of the originals in
 * the protected spec (which cannot be imported without registering its
 * tests), minus only the mobile-observer branches that desktop never takes.
 */
const ITEMS = [
	{ id: 'manage-zulu', title: 'Zulu Song', artist: 'Beta' },
	{ id: 'manage-alpha', title: 'Alpha Song', artist: 'Zulu' },
	{ id: 'manage-mike', title: 'Mike Song', artist: 'Alpha' },
	{ id: 'manage-bravo', title: 'Bravo Song', artist: 'Delta' }
];
const ALL = ['manage-zulu', 'manage-alpha', 'manage-mike', 'manage-bravo'];
const AUTH = 'nntune.library.auth-context.v1';
const BASE = 'nntune.library.favorites.v1';
const ACCOUNT = 'management-account';
const KEY = `${BASE}.${ACCOUNT}`;
const SEEDED = ['manage-mike', 'manage-zulu', 'hidden-library-id'];
type Reply = { items: unknown; status: number; hold?: Promise<void> };
type Harness = { reply: Reply; requests: string[]; errors: string[]; mobileObserver?: boolean };

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function favorite(page: Page, id: string): Locator {
	return page.locator(`[data-testid="favorite-item"][data-id="${id}"]`);
}
function unstar(page: Page, id: string): Locator {
	return favorite(page, id).getByRole('button', { name: /unstar|remove .+ from favorites/i });
}
function view(page: Page): Locator {
	return page.getByRole('combobox', { name: /show|filter|view/i });
}
async function ids(page: Page, hook = 'library-item'): Promise<(string | null)[]> {
	return page.getByTestId(hook).evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}
async function expectRows(page: Page, want: string[]) {
	await expect.poll(() => ids(page)).toEqual(want);
}
async function count(page: Page, matched: number, total = ITEMS.length) {
	await expect(page.getByText(new RegExp(`\\b${matched}\\s*(?:of|/)\\s*${total}\\s+(?:loaded\\s+)?tracks?\\b`, 'i'))).toBeVisible();
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

async function open(page: Page, libraryUrl = '/library') {
	await page.goto(libraryUrl);
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
	await openQueue(page);
	await page.getByRole('button', { name: 'Move Alpha Song up', exact: true }).click();
	await expect(page.getByTestId('player-next')).toHaveCount(1);
	await expect(page.getByTestId('player-next')).toBeVisible();
	await expect(page.getByTestId('player-next')).toBeEnabled();
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

// ---- diagnostic recording (test side only; no product DOM changes) ----

type Identity = Record<string, string | number | null> | null;
type Step = { n: number; key: string; active: Identity };
type Stage = {
	stage: string; helper: 'legacy' | 'amended'; ok: boolean; error: string | null;
	presses: number; initial: Identity; final: Identity; sequence: Step[];
};

// Deep active element (through open shadow roots) with stable, content-based identity.
async function activeIdentity(page: Page): Promise<Identity> {
	return page.evaluate(() => {
		let el: Element | null = document.activeElement;
		while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
		if (!el) return null;
		const parent = el.parentElement;
		const owner = el.closest('[data-id]');
		return {
			tag: el.tagName.toLowerCase(),
			role: el.getAttribute('role'),
			name: (el.getAttribute('aria-label') ?? el.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 60),
			testid: el.getAttribute('data-testid'),
			id: el.id || null,
			ownerTestid: owner?.getAttribute('data-testid') ?? null,
			ownerId: owner?.getAttribute('data-id') ?? null,
			index: parent ? [...parent.children].indexOf(el) : -1
		};
	});
}

async function observe(page: Page, stage: string, helper: Stage['helper'], run: () => Promise<void>): Promise<Stage> {
	const keyboard = page.keyboard;
	const hadOwn = Object.prototype.hasOwnProperty.call(keyboard, 'press');
	const original = keyboard.press;
	const sequence: Step[] = [];
	const initial = await activeIdentity(page);
	keyboard.press = async (key, options) => {
		await original.call(keyboard, key, options);
		sequence.push({ n: sequence.length + 1, key, active: await activeIdentity(page) });
	};
	let error: string | null = null;
	try {
		await run();
	} catch (caught) {
		error = caught instanceof Error ? caught.message : String(caught);
		if (helper === 'amended') {
			const final = await activeIdentity(page).catch(() => null);
			throw Object.assign(caught instanceof Error ? caught : new Error(error), { diagnostic: { stage, final, sequence } });
		}
	} finally {
		if (hadOwn) keyboard.press = original; else delete (keyboard as { press?: unknown }).press;
	}
	return { stage, helper, ok: error === null, error, presses: sequence.length, initial, final: await activeIdentity(page), sequence };
}

async function runLane(browser: Browser, baseURL: string | undefined, colorScheme: 'light' | 'dark', helper: Stage['helper']) {
	const context = await browser.newContext({ baseURL, viewport: { width: 1280, height: 844 }, colorScheme });
	const stages: Stage[] = [];
	const useHelper = (page: Page, target: Locator) => helper === 'legacy' ? tabTo(page, target) : boundedTabTo(page, target);
	try {
		await context.route('**/*', (route) => {
			const url = new URL(route.request().url());
			if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) return route.abort('blockedbyclient');
			if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) {
				return route.fulfill({ status: 404, json: { error: 'unmocked endpoint' } });
			}
			return route.continue();
		});
		const page = await context.newPage();
		await page.emulateMedia({ colorScheme });
		const h = await setup(page);
		await open(page, '/library');
		await seedQueue(page);
		const stage1 = await observe(page, 'tab-to-view', helper, () => useHelper(page, view(page)));
		stages.push(stage1);
		if (!stage1.ok) return stages; // legacy only: cannot continue without a direct focus substitute
		await localAction(page, h, async () => {
			await page.keyboard.press('ArrowDown');
			await page.keyboard.press('Enter');
		});
		await expect(view(page).locator('option:checked')).toHaveText('Favorites only');
		await count(page, 2);
		stages.push(await observe(page, 'tab-to-unstar-manage-zulu', helper, () => useHelper(page, unstar(page, 'manage-zulu'))));
		return stages;
	} catch (caught) {
		const diagnostic = (caught as { diagnostic?: Stage }).diagnostic;
		if (diagnostic) stages.push({ ...diagnostic, helper, ok: false, error: String(caught), presses: diagnostic.sequence.length, initial: null });
		throw caught;
	} finally {
		const path = test.info().outputPath(`tab-traversal-${helper}.json`);
		await writeFile(path, JSON.stringify({ helper, colorScheme, stages }, null, 2));
		await test.info().attach(`tab-traversal-${helper}`, { path, contentType: 'application/json' });
		await context.close();
	}
}

test.afterEach(async ({}, testInfo) => {
	const path = testInfo.outputPath('diagnostic-result.json');
	await writeFile(path, JSON.stringify({
		title: testInfo.title, status: testInfo.status, expectedStatus: testInfo.expectedStatus,
		errors: testInfo.errors.map((error) => error.message ?? error.value),
		attachments: testInfo.attachments.map(({ name, contentType }) => ({ name, contentType }))
	}, null, 2));
	await testInfo.attach('diagnostic-result', { path, contentType: 'application/json' });
});

for (const colorScheme of ['light', 'dark'] as const) {
	test(`[KT-D1a:${colorScheme}] legacy 80-press tabTo on the desktop favorites flow (observational)`, async ({ browser, baseURL }) => {
		const stages = await runLane(browser, baseURL, colorScheme, 'legacy');
		// Legacy outcome is evidence only: a smaller queue may make it pass.
		expect(stages.length).toBeGreaterThan(0);
	});
	test(`[KT-D1b:${colorScheme}] shared bounded tabTo on the desktop favorites flow`, async ({ browser, baseURL }) => {
		const stages = await runLane(browser, baseURL, colorScheme, 'amended');
		expect(stages.every((stage) => stage.ok)).toBe(true);
		expect(stages).toHaveLength(2);
	});
}

// ---- synthetic failure diagnostics ----

const STYLE = '<style>button{outline:2px solid #06c;outline-offset:2px}button:focus{outline-color:#c60}</style>';

test('[KT-D2] a script that cycles focus among two controls reports a focus trap', async ({ page }) => {
	await page.setContent(`${STYLE}
		<button id="a">A</button><button id="b">B</button><button id="c">C</button><button id="target">Target</button>
		<script>
			const cycle = [document.getElementById('a'), document.getElementById('b')];
			let at = -1;
			document.addEventListener('keydown', (event) => {
				if (event.key !== 'Tab') return;
				event.preventDefault();
				at = (at + 1) % cycle.length;
				cycle[at].focus();
			});
		</script>`);
	await expect(boundedTabTo(page, page.locator('#target'))).rejects.toThrow(/focus trap/);
});

test('[KT-D3] a tabindex=-1 target in a naturally traversable document is not in the tab cycle', async ({ page }) => {
	await page.setContent(`${STYLE}
		<button id="a">A</button><button id="b">B</button><button id="c">C</button>
		<button id="target" tabindex="-1">Target</button>`);
	await expect(boundedTabTo(page, page.locator('#target'))).rejects.toThrow(/target not in tab cycle/);
});

test('[KT-D4] a document that cancels Tab leaves focus stuck', async ({ page }) => {
	await page.setContent(`${STYLE}
		<button id="a">A</button><button id="b">B</button><button id="target">Target</button>
		<script>
			document.addEventListener('keydown', (event) => { if (event.key === 'Tab') event.preventDefault(); });
		</script>`);
	await expect(boundedTabTo(page, page.locator('#target'))).rejects.toThrow(/focus stuck/);
});
