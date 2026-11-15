import { expect, type Page } from '@playwright/test';
import { openQueue } from './helpers/queue-host';
import { test, observedQueue, expectMobileQueue, returnToMobileLibrary, metadataManagementState, installManagementMediaProbe, mobileQueueTransport } from './helpers/favorites-mobile';

/*
 * Narrow exception: the Engineering Lead authorized a one-time migration, in
 * the same pull request, of this file's physical queue host, opening, locator
 * and lifecycle setup to the persistent shell's Queue panel. Every assertion
 * and behavior vector remains protected, and an independent equivalence
 * review is required. The two mobile FM-R01 variants additionally have a
 * one-time test-only read observer exception for absent queue DOM; all local
 * actions, focus and literal no-network comparisons remain protected. For
 * these two metadata-only variants only, a further technical amendment uses
 * explicit zero-count native transport/media state, initialized unchanged call
 * counters and real visible enabled Next inventory before/after. The separate
 * playable bank remains required; absence is never an enabled-control claim.
 */

// Builder-owned review regressions; the paired acceptance bank stays unchanged.
const ITEMS = [
	{ id: 'focus-alpha', title: 'Alpha Song' },
	{ id: 'focus-bravo', title: 'Bravo Song' },
	{ id: 'focus-charlie', title: 'Charlie Song' }
];
const KEY = 'nntune.library.favorites.v1.focus-account';
const libraryRow = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const shelfUnstar = (page: Page, id: string) => page.locator(`[data-testid="favorite-item"][data-id="${id}"]`).getByRole('button', { name: /^Unstar / });
const view = (page: Page) => page.getByRole('combobox', { name: 'Show', exact: true });

async function setup(page: Page, initial = ITEMS.map((item) => item.id), libraryUrl = '/library') {
	const requests: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => {
		if (['fetch', 'xhr', 'media'].includes(request.resourceType()) || request.method() !== 'GET') {
			requests.push(`${request.method()} ${new URL(request.url()).pathname}`);
		}
	});
	await page.context().route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) {
			return route.fulfill({ status: 404, json: { error: 'unmocked endpoint' } });
		}
		return route.continue();
	});
	await page.addInitScript(({ key, initial }) => {
		sessionStorage.setItem('nntune.library.auth-context.v1', 'focus-account');
		localStorage.setItem(key, JSON.stringify(initial));
	}, { key: KEY, initial });
	const reply: { status: number; items: typeof ITEMS; hold?: Promise<void> } = { status: 200, items: ITEMS };
	await page.route('**/api/library', async (route) => {
		if (reply.hold) await reply.hold;
		await route.fulfill({ status: reply.status, json: reply.items });
	});
	await page.goto(libraryUrl);
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	for (const item of ITEMS) await libraryRow(page, item.id).getByRole('button', { name: item.title, exact: true }).click();
	await openQueue(page);
	await expect(page.getByTestId('player-next')).toHaveCount(1);
	await expect(page.getByTestId('player-next')).toBeVisible();
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await page.getByTestId('player-next').click();
	return { reply, requests, errors };
}

async function queueState(page: Page, observe = false) {
	const observed = observe ? await observedQueue(page) : null;
	const dom = await page.evaluate(() => ({
		rows: [...document.querySelectorAll('[data-testid="queue-item"]')].map((node) => ({
			id: node.getAttribute('data-track-id'), current: node.getAttribute('aria-current')
		})),
		now: document.querySelector('[data-testid="now-playing"]')?.textContent,
		nextDisabled: document.querySelector('[data-testid="player-next"]')?.hasAttribute('disabled'),
		media: [...document.querySelectorAll('audio, video')].map((node) => ({
			src: (node as HTMLMediaElement).currentSrc, paused: (node as HTMLMediaElement).paused
		}))
	}));
	if (!observed) return dom;
	const { nextDisabled, ...retained } = dom;
	return { ...retained, rows: observed.ids.map((id, index) => ({ id, current: index === observed.currentIndex ? 'true' : null })),
		now: observed.selectedTitle, currentIndex: observed.currentIndex, currentId: observed.currentId,
		transportMount: await metadataManagementState(page) };
}

for (const colorScheme of ['light', 'dark'] as const) {
	test(`[FM-R01:${colorScheme}] keyboard main-row unstar and zero-match reset retain focus without queue/network edits`, async ({ page, observeMobileLibrary }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await page.emulateMedia({ colorScheme });
		await installManagementMediaProbe(page);
		const h = await setup(page, undefined, await observeMobileLibrary());
		const expected = { ids: ['focus-alpha', 'focus-bravo', 'focus-charlie'], currentIndex: 1, currentId: 'focus-bravo', selectedTitle: 'Bravo Song' };
		const owner = await expectMobileQueue(page, expected);
		const transportInventory = await mobileQueueTransport(page);
		await test.info().attach('pre-management-queue', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
		await returnToMobileLibrary(page);
		const before = await queueState(page, true);
		let previous = before;
		const requests = [...h.requests];
		const unchanged = async () => {
			await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
			expect(await owner.evaluate((binding) => binding === window.__favoritesQueueOwner)).toBe(true);
			const current = await queueState(page, true);
			expect(current).toEqual(before);
			expect(current).toEqual(previous);
			expect(await observedQueue(page)).toEqual(expected);
			previous = current;
			expect(h.requests).toEqual(requests);
		};
		// All-tracks starring keeps its mounted button focused.
		const alpha = libraryRow(page, 'focus-alpha').getByRole('button', { name: 'Unstar Alpha Song', exact: true });
		await alpha.focus();
		await page.keyboard.press('Space');
		await expect(libraryRow(page, 'focus-alpha').getByRole('button', { name: 'Star Alpha Song', exact: true })).toBeFocused();
		await unchanged();
		await page.keyboard.press('Space');
		await unchanged();
		await view(page).selectOption('favorites');
		await unchanged();
		for (const [index, item] of ITEMS.entries()) {
			const button = libraryRow(page, item.id).getByRole('button', { name: `Unstar ${item.title}`, exact: true });
			await button.focus();
			await page.keyboard.press(index % 2 === 0 ? 'Space' : 'Enter');
			await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - index - 1);
			await expect(view(page)).toBeFocused();
			await unchanged();
		}
		const reset = page.getByRole('button', { name: 'Show all tracks', exact: true });
		await reset.focus();
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect(view(page)).toHaveValue('all');
		await expect(view(page)).toBeFocused();
		expect(await view(page).evaluate((node) => node.matches(':focus-visible'))).toBe(true);
		await unchanged();
		expect(h.errors).toEqual([]);
		await test.info().attach('management-runtime', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
		expect(h.requests).toEqual(requests);
		const navigationStart = h.requests.length;
		await expectMobileQueue(page, expected);
		expect(await mobileQueueTransport(page)).toEqual(transportInventory);
		await test.info().attach('queue-transport-inventory', { body: JSON.stringify(transportInventory), contentType: 'application/json' });
		await test.info().attach('post-management-queue', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
		expect(await owner.evaluate((binding) => binding === window.__favoritesQueueOwner)).toBe(true);
		expect(h.requests.slice(navigationStart).every((request) => request.startsWith('GET '))).toBe(true);
	});
}

test('[FM-R05] keyboard zero-match reset focuses the persistent selector and preserves hidden favorites', async ({ page }) => {
	const h = await setup(page, ['hidden-library-id']);
	const before = await queueState(page);
	await view(page).selectOption('favorites');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await page.getByRole('button', { name: 'Show all tracks', exact: true }).focus();
	await page.keyboard.press('Space');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(view(page)).toBeFocused();
	expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? '[]'), KEY)).toEqual(['hidden-library-id']);
	expect(await queueState(page)).toEqual(before);
	expect(h.requests).toEqual(['GET /api/library']);
	expect(h.errors).toEqual([]);
});

test('[FM-R02] held background refresh exposes loading, suspends all four queue statuses, and restores roles', async ({ page }) => {
	const h = await setup(page);
	const before = await queueState(page);
	const regions = page.locator('[data-testid="now-playing"], [data-testid="queue-announcement"], [data-testid="queue-filter-count"], button:has-text("Shuffle upcoming") + p');
	await expect(regions).toHaveCount(4);
	for (const region of await regions.all()) await expect(region).toHaveAttribute('role', 'status');
	await view(page).selectOption('favorites');
	await view(page).focus();
	let release!: () => void;
	h.reply.hold = new Promise<void>((resolve) => { release = resolve; });
	h.reply.items = ITEMS.map((item) => ({ ...item, title: `${item.title} refreshed` }));
	try {
		await page.getByRole('button', { name: 'Refresh', exact: true }).evaluate((node: HTMLButtonElement) => node.click());
		await expect(page.getByRole('status', { name: '', exact: true }).filter({ hasText: 'Loading your library…' })).toBeVisible();
		await expect(page.locator('main').getByRole('status')).toHaveCount(1);
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect(page.getByTestId('queue-item')).toHaveCount(3);
		for (const region of await regions.all()) {
			await expect(region).not.toHaveAttribute('role', 'status');
			await expect(region).not.toHaveAttribute('aria-live', 'polite');
		}
		await expect(view(page)).toBeFocused();
		expect(await queueState(page)).toEqual(before);
	} finally {
		release();
	}
	await expect(libraryRow(page, 'focus-alpha')).toContainText('Alpha Song refreshed');
	await expect(page.getByText('Loading your library…', { exact: true })).toBeHidden();
	for (const region of await regions.all()) await expect(region).toHaveAttribute('role', 'status');
	await expect(page.locator('button:has-text("Shuffle upcoming") + p')).toHaveAttribute('aria-live', 'polite');
	await expect(view(page)).toBeFocused();
	expect(await queueState(page)).toEqual(before);
	expect(h.requests).toEqual(['GET /api/library', 'GET /api/library']);
	expect(h.errors).toEqual([]);
});

test('[FM-R03] access refusal clears pending confirmation, old feedback and favorites-only view before retry', async ({ page }) => {
	const h = await setup(page);
	await shelfUnstar(page, 'focus-alpha').click();
	await expect(page.getByTestId('favorites-section').getByRole('status')).toContainText('Removed Alpha Song');
	await view(page).selectOption('favorites');
	await page.getByRole('button', { name: 'Clear local favorites', exact: true }).click();
	await expect(page.getByRole('button', { name: 'Cancel', exact: true })).toBeVisible();
	h.reply.status = 401;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toBeVisible();
	h.reply.status = 200;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(view(page)).toHaveValue('all');
	await expect(page.getByRole('button', { name: 'Cancel', exact: true })).toBeHidden();
	await expect(page.getByTestId('favorites-section').getByRole('status')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(h.requests).toEqual(['GET /api/library', 'GET /api/library', 'GET /api/library']);
	expect(h.errors).toEqual([]);
});

test('[FM-R04] shelf removal focuses the next item at its position, then the previous item, then the heading', async ({ page }) => {
	const h = await setup(page);
	const before = await queueState(page);
	await shelfUnstar(page, 'focus-bravo').focus();
	await page.keyboard.press('Enter');
	await expect(shelfUnstar(page, 'focus-charlie')).toBeFocused();
	await page.keyboard.press('Space');
	await expect(shelfUnstar(page, 'focus-alpha')).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('favorites-section').getByRole('heading')).toBeFocused();
	expect(await queueState(page)).toEqual(before);
	expect(h.requests).toEqual(['GET /api/library']);
	expect(h.errors).toEqual([]);
});
