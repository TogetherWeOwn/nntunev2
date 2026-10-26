import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Protected acceptance bank. The paired Builder implements behavior and ships
 * the combined PR without editing this file or the pure companion bank.
 *
 * Local/offline real page and queue; only API/media boundaries are synthetic.
 * Not evidence of decoding, provider playback, staging or production health.
 * Missing actions fail explicit assertions AFTER fixture preconditions pass.
 * Run: pnpm --dir apps/web exec playwright test tests/library-upcoming-sort.spec.ts --workers=1
 * Use CI=1 to forbid another checkout's preview; the runner owns its webServer.
 * JSON reporters retain the stable US-B/US-C case ids for handoff evidence.
 *
 * Breaks caught: full-queue/visible-only sort, unstable ties, media restart,
 * metadata mutation, focus loss, silent/duplicate outcome, persistent sort mode,
 * and accidental coupling to favorites, browsing order, find or the network.
 */

const ITEMS = [
	{ id: 'played', title: 'Zulu played', artist: 'Zulu played' },
	{ id: 'current', title: 'Middle current', artist: 'Middle current' },
	{ id: 'future-z', title: 'Zulu future', artist: 'Beta' },
	{ id: 'future-echo-z', title: 'Echo future', artist: ' alpha ' },
	{ id: 'future-a', title: 'Alpha future', artist: '' },
	{ id: 'future-echo-a', title: 'eCHO future', artist: 'ALPHA' },
	{ id: 'future-missing', title: '', artist: ' \t ' },
	{ id: 'late-added', title: 'Aardvark late', artist: 'Aardvark' }
].map((item) => ({ ...item, streamUrl: `/api/demo/audio?track=${item.id}` }));
const INITIAL = [
	'played', 'current', 'future-z', 'future-echo-z', 'future-a', 'future-echo-a', 'future-missing'
];
const TITLE = [
	'played', 'current', 'future-a', 'future-echo-z', 'future-echo-a', 'future-missing', 'future-z'
];
const ARTIST = [
	'played', 'current', 'future-echo-z', 'future-echo-a', 'future-z', 'future-a', 'future-missing'
];
type Mode = 'title' | 'artist';
type Probe = {
	play: number;
	pause: number;
	load: number;
	seek: number;
	audio: HTMLAudioElement | null;
	currentRow: Element | null;
	messages: string[];
};

declare global {
	interface Window {
		__upcomingSortProbe: Probe;
	}
}

test.use({ hasTouch: true });

test.beforeEach(async ({ page, baseURL }) => {
	const origin = new URL(baseURL!).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (url.origin !== origin) return route.abort('blockedbyclient');
		if (url.pathname === '/api/library') return route.fulfill({ json: ITEMS });
		// Keep native decoding/network outside the synthetic media-boundary test.
		if (url.pathname === '/api/demo/audio') return;
		if (url.pathname.startsWith('/api/')) return route.fulfill({ status: 404, json: {} });
		return route.continue();
	});
	await page.addInitScript(() => {
		const probe: Probe = {
			play: 0, pause: 0, load: 0, seek: 0,
			audio: null, currentRow: null, messages: []
		};
		window.__upcomingSortProbe = probe;
		const times = new WeakMap<HTMLMediaElement, number>();
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get() { return times.get(this) ?? 0; },
			set(value: number) { probe.seek++; times.set(this, value); }
		});
		HTMLMediaElement.prototype.play = function () {
			probe.play++;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pause++;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		HTMLMediaElement.prototype.load = function () { probe.load++; };
	});
});

function libraryRow(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function enqueueButton(page: Page, id: string): Locator {
	const item = ITEMS.find((entry) => entry.id === id)!;
	const label = item.title || item.id;
	// Clicking a row's centre can hit its checkbox/copy control at 320px.
	return libraryRow(page, id).getByRole('button', { name: new RegExp(`^${label}(?:\\s|$)`) });
}

function queueRow(page: Page, id: string): Locator {
	return page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
}

function action(page: Page, mode: Mode): Locator {
	return page.getByRole('button', { name: new RegExp(`^sort (?:all )?upcoming (?:tracks )?by ${mode}\\b`, 'i') });
}

async function requireAction(page: Page, mode: Mode): Promise<Locator> {
	const button = action(page, mode);
	await expect(button, `library must expose the one-shot Sort upcoming by ${mode} action`).toHaveCount(1);
	await expect(button).toBeVisible();
	return button;
}

async function queueOrder(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) =>
		rows.map((row) => row.getAttribute('data-track-id'))
	);
}

async function openLibrary(page: Page, enqueue = true) {
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	if (!enqueue) return;
	for (const id of INITIAL) await enqueueButton(page, id).click();
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
	await queueRow(page, 'current').getByRole('button', { name: 'Select Middle current', exact: true }).click();
	await expect(queueRow(page, 'current')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('queue-position')).toContainText('2 of 7');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
}

async function prepareMedia(page: Page, paused: boolean) {
	await page.getByTestId('audio-player').evaluate(async (el: HTMLAudioElement, shouldPause) => {
		await el.play();
		el.dispatchEvent(new Event('playing'));
		if (shouldPause) { el.pause(); el.dispatchEvent(new Event('pause')); }
		el.currentTime = 42.5;
		el.dispatchEvent(new Event('timeupdate'));
		const probe = window.__upcomingSortProbe;
		probe.audio = el;
		probe.currentRow = document.querySelector('[data-testid="queue-item"][aria-current="true"]');
	}, paused);
}

async function media(page: Page) {
	return page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => {
		const p = window.__upcomingSortProbe;
		return {
			sameNode: el === p.audio,
			src: el.getAttribute('src'), time: el.currentTime, paused: el.paused,
			play: p.play, pause: p.pause, load: p.load, seek: p.seek
		};
	});
}

async function storage(page: Page) {
	return page.evaluate(() => ({
		local: Object.fromEntries(Object.entries(localStorage)),
		session: Object.fromEntries(Object.entries(sessionStorage))
	}));
}

async function observeOutcome(page: Page) {
	await page.getByTestId('queue-announcement').evaluate((region) => {
		window.__upcomingSortProbe.messages = [];
		new MutationObserver(() => {
			const message = region.textContent?.trim();
			if (message) window.__upcomingSortProbe.messages.push(message);
		}).observe(region, { subtree: true, childList: true, characterData: true });
	});
}

async function expectOutcome(page: Page) {
	const completion = page.locator('main').getByRole('status').filter({ hasText: /sort.*upcoming|upcoming.*sort/i });
	await expect(completion).toHaveCount(1);
	await expect(completion).toHaveAttribute('aria-live', 'polite');
	expect((await completion.innerText()).length).toBeLessThan(180);
	await expect.poll(() => page.evaluate(() => window.__upcomingSortProbe.messages.length)).toBe(1);
	await expect(page.locator('main [aria-live="assertive"]')).toHaveCount(0);
}

test('[US-C00] control: synthetic library, middle selection and media probe work before new controls', async ({ page }) => {
	await openLibrary(page);
	await prepareMedia(page, false);
	const snapshot = await media(page);
	expect(snapshot).toMatchObject({ sameNode: true, time: 42.5, paused: false, play: 1 });
	expect(new URL(snapshot.src!, page.url()).searchParams.get('track')).toBe('current');
	await prepareMedia(page, true);
	expect(await media(page)).toMatchObject({ sameNode: true, time: 42.5, paused: true, pause: 1 });
});

for (const paused of [false, true]) {
	test(`[US-B01-${paused ? 'paused' : 'playing'}] title sorts hidden upcoming, not current/prefix or library`, async ({ page }) => {
		await openLibrary(page);
		await libraryRow(page, 'future-a').getByRole('button', { name: 'Star Alpha future', exact: true }).click();
		const order = page.getByRole('combobox', { name: 'Order tracks', exact: true });
		await order.selectOption({ label: 'Artist A–Z' });
		const libraryFind = page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
		await libraryFind.fill('future');
		const browseBefore = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await prepareMedia(page, paused);
		const beforeMedia = await media(page);
		const beforeStorage = await storage(page);
		const filter = page.getByRole('searchbox', { name: 'Filter queue', exact: true });
		await filter.fill('Zulu future');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		const button = await requireAction(page, 'title');
		await expect(button).toBeEnabled();
		// Hint must explain the full suffix even while the filter hides most rows.
		await expect(page.locator('main').getByText(/all upcoming tracks.*(?:hidden|filter)|(?:hidden|filter).*all upcoming tracks/i)).toBeVisible();
		await observeOutcome(page);
		const requests: string[] = [];
		page.on('request', (request) => requests.push(request.url()));
		await button.click();
		await expectOutcome(page);
		await expect(filter).toHaveValue('Zulu future');
		await expect(libraryFind).toHaveValue('future');
		await expect(order.locator('option:checked')).toHaveText('Artist A–Z');
		expect(await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))).toEqual(browseBefore);
		await expect(libraryRow(page, 'future-a').getByRole('button', { name: 'Unstar Alpha future', exact: true })).toBeVisible();
		expect(await media(page)).toEqual(beforeMedia);
		expect(await storage(page)).toEqual(beforeStorage);
		expect(requests).toEqual([]);
		await filter.fill('');
		await expect.poll(() => queueOrder(page)).toEqual(TITLE);
		await expect(queueRow(page, 'current')).toHaveAttribute('aria-current', 'true');
		await expect(page.getByTestId('queue-position')).toContainText('2 of 7');
	});
}

test('[US-B02] artist uses stable ties and unnamed-last without replacing current row or media', async ({ page }) => {
	await openLibrary(page);
	await prepareMedia(page, true);
	const before = await media(page);
	const button = await requireAction(page, 'artist');
	await observeOutcome(page);
	await button.click();
	await expect.poll(() => queueOrder(page)).toEqual(ARTIST);
	await expectOutcome(page);
	expect(await media(page)).toEqual(before);
	expect(await queueRow(page, 'current').evaluate((el) => el === window.__upcomingSortProbe.currentRow)).toBe(true);
	await expect(queueRow(page, 'future-echo-z')).toContainText('Echo future');
	await expect(queueRow(page, 'future-echo-a')).toContainText('eCHO future');
	await expect(queueRow(page, 'future-missing')).toContainText('future-missing');
});

for (const state of ['empty', 'one-upcoming', 'last'] as const) {
	test(`[US-B03-${state}] no useful suffix disables both actions`, async ({ page }) => {
		await openLibrary(page, state !== 'empty');
		if (state !== 'empty') {
			const id = state === 'last' ? 'future-missing' : 'future-echo-a';
			const label = state === 'last' ? 'future-missing' : 'eCHO future';
			await queueRow(page, id).getByRole('button', { name: `Select ${label}`, exact: true }).click();
			await expect(queueRow(page, id)).toHaveAttribute('aria-current', 'true');
		}
		for (const mode of ['title', 'artist'] as const) {
			await expect(await requireAction(page, mode)).toBeDisabled();
		}
	});
}

test('[US-B04] repeat title sort is an honest no-op and never a persistent mode', async ({ page }) => {
	await openLibrary(page);
	const button = await requireAction(page, 'title');
	await button.click();
	await expect.poll(() => queueOrder(page)).toEqual(TITLE);
	const before = await storage(page);
	// A known identity result may be disabled; an enabled action must tell the truth.
	if (await button.isEnabled()) {
		await button.click();
		await expect(page.getByTestId('queue-announcement')).toContainText(/already|unchanged|no.*change/i);
	}
	expect(await queueOrder(page)).toEqual(TITLE);
	expect(await storage(page)).toEqual(before);
	await enqueueButton(page, 'late-added').click();
	await expect.poll(() => queueOrder(page)).toEqual([...TITLE, 'late-added']);
});

for (const theme of ['light', 'dark'] as const) {
	test(`[US-B05-${theme}] 320px keyboard/touch actions retain useful focus and fit`, async ({ page }) => {
		await page.setViewportSize({ width: 320, height: 800 });
		await page.emulateMedia({ colorScheme: theme });
		await openLibrary(page);
		const title = await requireAction(page, 'title');
		const artist = await requireAction(page, 'artist');
		await page.locator('h1').evaluate((el: HTMLElement) => { el.tabIndex = -1; el.focus(); });
		for (let tabs = 0; tabs < 160 && !(await title.evaluate((el) => el === document.activeElement)); tabs++) {
			await page.keyboard.press('Tab');
		}
		await expect(title).toBeFocused();
		expect(await title.evaluate((el) => {
			const css = getComputedStyle(el);
			return el.matches(':focus-visible') && ((css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) || css.boxShadow !== 'none');
		})).toBe(true);
		await page.keyboard.press('Enter');
		await expect.poll(() => queueOrder(page)).toEqual(TITLE);
		// Disabling an already-sorted action must not strand keyboard focus on body.
		expect(await page.evaluate(() => document.activeElement !== document.body && document.activeElement?.isConnected)).toBe(true);
		await artist.scrollIntoViewIfNeeded();
		await artist.tap();
		await expect.poll(() => queueOrder(page)).toEqual(ARTIST);
		await title.focus();
		await page.keyboard.press('Space');
		await expect.poll(() => queueOrder(page)).toEqual(TITLE);
		for (const button of [title, artist]) {
			await button.scrollIntoViewIfNeeded();
			const rect = await button.boundingBox();
			expect(rect).not.toBeNull();
			expect(rect!.x).toBeGreaterThanOrEqual(0);
			expect(rect!.x + rect!.width).toBeLessThanOrEqual(320);
			// WCAG spacing alternative for compact native-sized targets.
			expect(await button.evaluate((el) => {
				const r = el.getBoundingClientRect();
				if (r.width >= 24 && r.height >= 24) return true;
				const cx = (r.left + r.right) / 2, cy = (r.top + r.bottom) / 2;
				return [...document.querySelectorAll('button, input, select, a[href]')].every((other) => {
					if (other === el) return true;
					const b = other.getBoundingClientRect();
					if (!b.width || !b.height) return true;
					const dx = Math.max(b.left - cx, 0, cx - b.right);
					const dy = Math.max(b.top - cy, 0, cy - b.bottom);
					return Math.hypot(dx, dy) >= 12;
				});
			})).toBe(true);
		}
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
	});
}

test('[US-C01] scope control: Demo does not gain upcoming-sort actions', async ({ page }) => {
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(action(page, 'title')).toHaveCount(0);
	await expect(action(page, 'artist')).toHaveCount(0);
});
