import { expect, test as base, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueConfirmBridge } from './fixtures/queue-confirm/mount.js';

/**
 * Acceptance-owned reusable-host coverage. The production library supplies a
 * stable callback; withdrawal is a public component contract, not a claimed
 * existing production navigation path. The host never implements transitions.
 *
 * Break caught: withdrawing an opted-in trim callback must invalidate consent;
 * supplying a different callback must not make an old Yes authorize that host.
 * Worker-owned Vite is closed even when a deliberately RED oracle fails.
 */
const test = base.extend<{ hostPage: Page }, { hostOrigin: string }>({
	hostOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/queue-confirm/', import.meta.url).pathname),
			publicDir: false,
			plugins: [svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [decodeURIComponent(new URL('../', import.meta.url).pathname)] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('confirmation fixture has no TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	}, { scope: 'worker' }],
	hostPage: async ({ page, hostOrigin }, use) => {
		await page.goto(hostOrigin);
		await expect.poll(() => page.evaluate(() => Boolean(window.queueConfirmHost))).toBe(true);
		await use(page);
	}
});
const trim = (page: Page) => page.getByRole('button', { name: 'Clear upcoming', exact: true });
const yes = (page: Page) => page.getByRole('button', { name: /^(confirm|yes\b|clear\s+\d+)/i });
const inspect = (page: Page): Promise<ReturnType<QueueConfirmBridge['inspect']>> =>
	page.evaluate(() => window.queueConfirmHost.inspect());

async function supply(page: Page, mode: 'original' | 'none' | 'replacement'): Promise<void> {
	await page.evaluate((mode) => window.queueConfirmHost.supply(mode), mode);
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
	));
}

test('QCR-07 withdrawal invalidates old consent before a replacement callback is supplied', async ({ hostPage: page }) => {
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await trim(page).click();
	await expect(yes(page)).toBeVisible();
	const staleYes = await yes(page).elementHandle();
	await supply(page, 'none');
	await expect.poll(async () => await trim(page).count() > 0 && await trim(page).isEnabled()).toBe(false);
	await expect.configure({ soft: true }).poll(async () => await yes(page).count() > 0 && await yes(page).isEnabled(),
		{ message: 'withdrawal must dismiss or disable the pending destructive action' }).toBe(false);
	await supply(page, 'replacement');
	await expect(trim(page)).toBeEnabled();
	await staleYes!.evaluate((element: HTMLButtonElement) => element.click());
	expect(await inspect(page), 'stale Yes cannot authorize a newly supplied host callback').toEqual({
		calls: [], tracks: ['host-current', 'host-next'], currentIndex: 0
	});
	await trim(page).click();
	await expect(yes(page)).toBeEnabled();
	await yes(page).click();
	expect(await inspect(page)).toEqual({
		calls: ['replacement-trim'], tracks: ['host-current', 'host-next'], currentIndex: 0
	});
});

test('QCR-07 control: a fresh opted-in request invokes only its current callback exactly once', async ({ hostPage: page }) => {
	await supply(page, 'replacement');
	await trim(page).click();
	await expect(page.getByRole('button', { name: /^cancel\b/i })).toBeFocused();
	await expect(yes(page)).toBeEnabled();
	expect((await inspect(page)).calls).toEqual([]);
	await yes(page).click();
	expect(await inspect(page)).toEqual({
		calls: ['replacement-trim'], tracks: ['host-current', 'host-next'], currentIndex: 0
	});
	await expect(yes(page)).toHaveCount(0);
});
