import { expect, test } from '@playwright/test';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { createServer } from 'vite';

const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const fixtureRoot = decodeURIComponent(new URL('./fixtures/track-like-click/', import.meta.url).pathname);

test('the shared Track Like button dispatches a click only when not pending', async ({ page }) => {
	const server = await createServer({
		configFile: false,
		root: fixtureRoot,
		publicDir: false,
		plugins: [svelte({ configFile: false })],
		server: { host: '127.0.0.1', port: 0, fs: { allow: [webRoot] } }
	});
	try {
		await server.listen();
		const address = server.httpServer?.address();
		if (!address || typeof address === 'string') throw new Error('missing fixture TCP address');
		await page.goto(`http://127.0.0.1:${address.port}`);

		const count = page.getByTestId('toggle-count');
		await page.getByRole('button', { name: 'Like Available track' }).click();
		await expect(count).toHaveText('1');

		const pending = page.getByRole('button', { name: 'Like Pending track' });
		await expect(pending).toBeDisabled();
		// Deliver a click despite aria-disabled to exercise the handler's guard.
		await pending.dispatchEvent('click');
		await expect(count).toHaveText('1');
	} finally {
		await server.close();
	}
});
