import { expect, test as base, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { createQueue, type Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/queue-direct-selection/mount.js';

/**
 * Builder-owned supplement; the protected direct-selection bank stays unchanged.
 * Catches visible-index callbacks, missing identity validation before a render
 * flush, and missing connectivity validation during native click dispatch.
 * Mounts the real component through the existing parent; no handler substitutes.
 */
const test = base.extend<{ componentPage: Page }, { componentOrigin: string }>({
	componentOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/queue-direct-selection/', import.meta.url).pathname),
			publicDir: false,
			plugins: [svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [decodeURIComponent(new URL('../', import.meta.url).pathname)] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('missing component TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally { await server.close(); }
	}, { scope: 'worker' }],
	componentPage: async ({ page, componentOrigin }, use) => {
		await page.route('**/*', (route) => new URL(route.request().url()).origin === componentOrigin
			? route.continue() : route.abort());
		await page.goto(componentOrigin);
		await expect.poll(() => page.evaluate(() => Boolean(window.queuePick))).toBe(true);
		await use(page);
	}
});

const TRACKS: Track[] = [
	{ id: 'reg-mike', title: 'Mike Song', artist: 'Match Band', streamUrl: '', artworkUrl: null },
	{ id: 'reg-zulu', title: 'Zulu Song', artist: 'Queue Band', streamUrl: '', artworkUrl: null },
	{ id: 'reg-alpha', title: 'Alpha Song', artist: 'Match Band', streamUrl: '', artworkUrl: null }
];
const current = (page: Page) => page.locator('[data-testid="queue-item"][aria-current="true"]');
const alpha = (page: Page) => page.getByRole('button', { name: 'Select Alpha Song', exact: true });
const order = (page: Page) => page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));

async function mountQueue(page: Page) {
	const state = createQueue(TRACKS);
	await page.evaluate((value) => window.queuePick.replace(value), state);
	await expect(alpha(page)).toBeVisible();
	return state;
}

for (const example of [{ query: 'Alpha', visible: 1 }, { query: 'Match Band', visible: 2 }]) {
	test(`filtered component selection reports original slot 2 for ${example.query}`, async ({ componentPage: page }) => {
		const state = await mountQueue(page);
		await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill(example.query);
		await expect(page.getByTestId('queue-item')).toHaveCount(example.visible);
		await alpha(page).click();
		expect(await page.evaluate(() => window.queuePick.inspect())).toEqual({ state, calls: [2], nextCalls: 0 });
		await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
		expect(await order(page)).toEqual(['reg-mike', 'reg-zulu', 'reg-alpha']);
		await expect(current(page)).toHaveAttribute('data-track-id', 'reg-mike');
	});
}

test('filtered library selection keeps the selected identity and complete arrangement after clearing', async ({ page, baseURL }) => {
	const origin = new URL(baseURL!).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (url.origin !== origin) return route.abort();
		if (url.pathname === '/api/library' && route.request().method() === 'GET') {
			return route.fulfill({ json: TRACKS.map(({ id, title, artist }) => ({ id, title, artist })) });
		}
		if (url.pathname.startsWith('/api/') || route.request().resourceType() === 'media') return route.abort();
		return route.continue();
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	for (const id of ['reg-mike', 'reg-zulu', 'reg-alpha']) {
		await page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`).click();
	}
	await expect(current(page)).toHaveAttribute('data-track-id', 'reg-mike');
	await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill('Alpha');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await alpha(page).press('Enter');
	await expect(current(page)).toHaveAttribute('data-track-id', 'reg-alpha');
	await expect(page.getByTestId('queue-position')).toContainText('Track 3 of 3');
	await expect(page.getByTestId('queue-announcement')).toContainText('Selected Alpha Song');
	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	expect(await order(page)).toEqual(['reg-mike', 'reg-zulu', 'reg-alpha']);
	await expect(current(page)).toHaveAttribute('data-track-id', 'reg-alpha');
	await page.getByTestId('player-next').click();
	await expect(current(page)).toHaveAttribute('data-track-id', 'reg-alpha');
});

test('still-connected stale button rejects a replacement identity before the render flush', async ({ componentPage: page }) => {
	await mountQueue(page);
	// Positive control: the same mounted callback accepts the original row.
	await alpha(page).click();
	expect((await page.evaluate(() => window.queuePick.inspect())).calls).toEqual([2]);
	const replacement = { ...createQueue([
		TRACKS[0]!, TRACKS[1]!, { ...TRACKS[2]!, id: 'reg-bravo', title: 'Bravo Song' }
	]), currentIndex: 1 };
	const observed = await page.evaluate(async (value) => {
		const button = document.querySelector<HTMLButtonElement>('button[aria-label="Select Alpha Song"]')!;
		// replace assigns parent props synchronously, but only awaits tick later.
		// Dispatch in this same turn, while the old keyed DOM is still connected.
		const pending = window.queuePick.replace(value);
		const connected = button.isConnected;
		button.click();
		await pending;
		return { connected, ...window.queuePick.inspect() };
	}, replacement);
	expect(observed).toEqual({ connected: true, state: replacement, calls: [], nextCalls: 0 });
	await expect(current(page)).toHaveAttribute('data-track-id', 'reg-zulu');
	await expect(alpha(page)).toHaveCount(0);
	await page.getByRole('button', { name: 'Select Bravo Song', exact: true }).click();
	expect((await page.evaluate(() => window.queuePick.inspect())).calls).toEqual([2]);
});

test('button detached during native dispatch rejects selection at the delegated handler', async ({ componentPage: page }) => {
	const state = await mountQueue(page);
	const observed = await alpha(page).evaluate((element) => {
		const button = element as HTMLButtonElement;
		const connectedBefore = button.isConnected;
		let detachedAtTarget = false;
		// Native dispatch has already captured the path to Svelte's root. Unlike
		// clicking an already-detached node, this reaches the delegated handler.
		button.addEventListener('click', () => {
			button.remove();
			detachedAtTarget = !button.isConnected;
		}, { once: true });
		button.click();
		return { connectedBefore, detachedAtTarget, ...window.queuePick.inspect() };
	});
	expect(observed).toEqual({ connectedBefore: true, detachedAtTarget: true, state, calls: [], nextCalls: 0 });
	await expect(current(page)).toHaveAttribute('data-track-id', 'reg-mike');
	expect(await order(page)).toEqual(['reg-mike', 'reg-zulu', 'reg-alpha']);
	await page.getByRole('button', { name: 'Select Zulu Song', exact: true }).click();
	expect((await page.evaluate(() => window.queuePick.inspect())).calls).toEqual([1]);
});
