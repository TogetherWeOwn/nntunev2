import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { expect, test as base } from '@playwright/test';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { ARTWORK_PLACEHOLDER_SRC } from '../../../src/lib/player/artwork.js';
import { artworkCacheHeaders, audioCacheHeaders } from '../../../src/lib/player/cacheHeaders.js';
import type { SeekFetch, SeekResult } from '../../../src/lib/player/seekController.js';
import { withIsolatedViteFixture } from '../../vite-fixture.helpers.js';
import { ARTWORK_PATH, AUDIO_PATH } from './paths.js';

declare global {
	interface Window {
		seekFixtureStorm(): Promise<{ completed: number; aborted: number; lastBytes: number }>;
		createSeekFixture(fetchRange: SeekFetch): {
			seek(start: number, end: number): Promise<SeekResult>;
			snapshot(): { position: number | null; lastBytes: number; completed: number; aborted: number };
		};
	}
}

const root = fileURLToPath(new URL('.', import.meta.url));
const webRoot = fileURLToPath(new URL('../../../', import.meta.url));
const mediaRoot = new URL('../../../../../fixtures/synthetic-audio/player-seek/', import.meta.url);

export const test = base.extend<{ openPlayer: () => Promise<void> }, { playerOrigin: string }>({
	playerOrigin: [async ({}, use) => {
		const manifest: { tracks: Array<{ file: string; bytes: number; sha256: string }> } =
			JSON.parse(await readFile(new URL('manifest.json', mediaRoot), 'utf8'));
		const [track] = manifest.tracks;
		const [wav, artwork] = await Promise.all([
			readFile(new URL(track.file, mediaRoot)),
			readFile(new URL(`../../../static${ARTWORK_PLACEHOLDER_SRC}`, import.meta.url))
		]);
		if (wav.length !== track.bytes || createHash('sha256').update(wav).digest('hex') !== track.sha256) {
			throw new Error('Player seek WAV does not match its fixture manifest');
		}

		await withIsolatedViteFixture({
			configFile: false,
			root,
			appType: 'mpa',
			publicDir: false,
			server: { host: '127.0.0.1', port: 0, ws: false, fs: { allow: [webRoot] } },
			plugins: [
				svelte({ configFile: false, compilerOptions: { runes: true } }),
				{
					name: 'player-seek-fixture-media',
					configureServer(server) {
						server.middlewares.use((request, response, next) => {
							const path = request.url?.split('?')[0];
							if (path === ARTWORK_PATH || path === ARTWORK_PLACEHOLDER_SRC) {
								response.writeHead(200, { 'content-type': 'image/svg+xml', ...artworkCacheHeaders() });
								response.end(artwork);
								return;
							}
							if (path !== AUDIO_PATH) return next();

							const headers = { 'content-type': 'audio/wav', 'accept-ranges': 'bytes', ...audioCacheHeaders() };
							const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? '');
							if (!range) {
								response.writeHead(200, { ...headers, 'content-length': wav.length });
								response.end(wav);
								return;
							}
							const start = Number(range[1]);
							const end = range[2] === '' ? wav.length - 1 : Math.min(Number(range[2]), wav.length - 1);
							if (start > end) {
								response.writeHead(416, { ...headers, 'content-range': `bytes */${wav.length}`, 'content-length': 0 });
								response.end();
								return;
							}
							response.writeHead(206, {
								...headers,
								'content-range': `bytes ${start}-${end}/${wav.length}`,
								'content-length': end - start + 1
							});
							response.end(wav.subarray(start, end + 1));
						});
					}
				}
			]
		}, use);
	}, { scope: 'worker' }],
	openPlayer: async ({ page, playerOrigin }, use) => {
		const unexpected: string[] = [];
		await page.route('**/*', (route) => {
			const url = new URL(route.request().url());
			if (url.origin === playerOrigin && !/^\/(api|demo)(\/|$)/.test(url.pathname)) return route.fallback();
			unexpected.push(url.href);
			return route.abort();
		});
		await use(async () => {
			await page.goto(playerOrigin);
			await page.waitForFunction(() => typeof window.seekFixtureStorm === 'function');
		});
		expect(unexpected, 'player fixture must use only local test resources').toEqual([]);
	}
});
