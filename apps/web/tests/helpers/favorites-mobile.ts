import { expect, test as base, type Page } from '@playwright/test';
import { createServer, type ViteDevServer } from 'vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { closeQueue, openQueue } from './queue-host';
import type { FavoritesQueueSnapshot } from './favorites-observer';

const webRoot = decodeURIComponent(new URL('../../', import.meta.url).pathname);
const layoutPath = `${webRoot}src/routes/+layout.svelte`;

// Opt-in and lazy: only the four named mobile management variants call this.
// Every other case continues to use the uninstrumented production preview.
export const test = base.extend<{ observeMobileLibrary: () => Promise<string> }>({
	observeMobileLibrary: async ({}, use, info) => {
		let server: ViteDevServer | undefined;
		await use(async () => {
			if (server) throw new Error('Observer server already started');
			server = await createServer({
				configFile: false, root: webRoot,
				cacheDir: `${webRoot}node_modules/.vite/favorites-observer-${info.workerIndex}`,
				plugins: [sveltekit({
					compilerOptions: { runes: ({ filename }) => filename.split(/[/\\]/).includes('node_modules') ? undefined : true },
					outDir: `.svelte-kit/favorites-observer-${info.workerIndex}`
				}), {
					name: 'test-only-favorites-owner', enforce: 'pre',
					transform(code, id) {
						if (id !== layoutPath) return;
						if (!code.includes('const player = createPlayerSession({ flush: tick });')) {
							throw new Error('Missing real layout session owner');
						}
						return code.replace('<script lang="ts">', `<script lang="ts">\nimport { attachFavoritesObserver } from '${webRoot}tests/helpers/favorites-observer.ts';`)
							.replace('</script>', '\nonMount(() => attachFavoritesObserver(player));\n</script>');
					}
				}],
				server: { host: '127.0.0.1', port: 0 }
			});
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('Missing observer server address');
			return `http://127.0.0.1:${address.port}/library`;
		});
		await server?.close();
	}
});

export async function observedQueue(page: Page): Promise<FavoritesQueueSnapshot> {
	return page.evaluate(() => {
		const owner = window.__favoritesQueueOwner;
		if (!owner) throw new Error('Missing actual layout queue observer');
		const snapshot = owner.snapshot;
		if (!Array.isArray(snapshot.ids) || !Number.isInteger(snapshot.currentIndex) ||
			!('currentId' in snapshot) || !('selectedTitle' in snapshot)) {
			throw new Error('Incomplete queue observation');
		}
		return snapshot;
	});
}

// Only the four explicitly opted-in metadata management variants use this
// amended mount-state contract. It never predicts a mounted control's state.
export async function metadataManagementState(page: Page) {
	const state = await page.evaluate(() => {
		const calls = Reflect.get(window, '__managementMedia');
		if (!calls || !Number.isInteger(calls.play) || !Number.isInteger(calls.pause)) {
			throw new Error('Missing initialized management media-call probe');
		}
		return {
			kind: 'no-mounted-native-transport',
			transportHosts: document.querySelectorAll('[data-player-transport]').length,
			next: document.querySelectorAll('[data-testid="player-next"]').length,
			previous: document.querySelectorAll('[data-testid="player-prev"], [data-player-transport] button[aria-label="Previous track"]').length,
			queueViews: document.querySelectorAll('[data-testid="queue-panel"]').length,
			media: document.querySelectorAll('audio, video').length,
			calls: { play: calls.play as number, pause: calls.pause as number }
		};
	});
	const { calls, ...mounted } = state;
	expect(mounted).toEqual({
		kind: 'no-mounted-native-transport', transportHosts: 0, next: 0,
		previous: 0, queueViews: 0, media: 0
	});
	return { ...mounted, calls };
}

export async function installManagementMediaProbe(page: Page) {
	await page.addInitScript(() => {
		const calls = { play: 0, pause: 0 };
		Object.assign(window, { __managementMedia: calls });
		for (const method of ['play', 'pause'] as const) {
			const original = HTMLMediaElement.prototype[method];
			Object.defineProperty(HTMLMediaElement.prototype, method, { configurable: true, value: function (this: HTMLMediaElement) {
				calls[method] += 1;
				return original.call(this);
			} });
		}
	});
}

export async function mobileQueueTransport(page: Page) {
	const next = page.getByTestId('player-next');
	await expect(next).toHaveCount(1);
	await expect(next).toBeVisible();
	await expect(next).toBeEnabled();
	return page.locator('[data-testid="player-next"], [data-testid="player-prev"], [data-player-transport] button[aria-label="Previous track"]').evaluateAll((nodes) => nodes.map((node) => ({
		hook: node.getAttribute('data-testid'), label: node.getAttribute('aria-label'), disabled: (node as HTMLButtonElement).disabled
	})));
}

export async function expectMobileQueue(page: Page, expected: FavoritesQueueSnapshot) {
	await openQueue(page);
	await expect(page).toHaveURL(/\/queue$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(1);
	const rows = page.getByTestId('queue-item');
	await expect.poll(() => rows.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-track-id')))).toEqual(expected.ids);
	await expect.poll(() => observedQueue(page)).toEqual(expected);
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', expected.currentId!);
	await expect(page.getByTestId('now-playing')).toContainText(expected.selectedTitle!);
	await mobileQueueTransport(page);
	return page.evaluateHandle(() => {
		if (!window.__favoritesQueueOwner) throw new Error('Missing queue owner');
		return window.__favoritesQueueOwner;
	});
}

export async function returnToMobileLibrary(page: Page) {
	await closeQueue(page, { url: /\/library$/, heading: 'Library' });
	await expect(page.getByTestId('library-item').first()).toBeVisible();
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
