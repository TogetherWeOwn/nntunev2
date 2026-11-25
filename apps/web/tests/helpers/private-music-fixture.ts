import { fileURLToPath } from 'node:url';
import { test as base, type Route } from '@playwright/test';
import { preview } from 'vite';

// Loopback HTTP uses a fixture-only cookie. The staged runner sends the real cookie over HTTPS.
export const fixtureCookie = 'private_music_fixture_session';
export function hasFixtureSession(cookie: string | null, account: string) {
	return cookie?.split('; ').includes(`${fixtureCookie}=${account}`) ?? false;
}

export function serveFlac(route: Route, body: Buffer) {
	const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
	const start = range ? Number(range[1]) : 0;
	const end = range?.[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
	if (start > end) return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${body.length}` } });
	return route.fulfill({
		status: range ? 206 : 200, contentType: 'audio/flac', body: body.subarray(start, end + 1),
		headers: { 'accept-ranges': 'bytes', ...(range && { 'content-range': `bytes ${start}-${end}/${body.length}` }) }
	});
}

export const test = base.extend<{ origin: string }>({
	serviceWorkers: 'block',
	origin: async ({}, use) => {
		// Serve the existing browser-fixtures build with the runtime arm set only here.
		const previous = process.env['PUBLIC_BROWSER_PLAYBACK'];
		process.env['PUBLIC_BROWSER_PLAYBACK'] = '1';
		const server = await preview({
			root: fileURLToPath(new URL('../../', import.meta.url)), mode: 'browser-fixtures',
			preview: { host: '127.0.0.1', port: 0 }
		}).finally(() => {
			if (previous === undefined) delete process.env['PUBLIC_BROWSER_PLAYBACK'];
			else process.env['PUBLIC_BROWSER_PLAYBACK'] = previous;
		});
		try {
			const address = server.httpServer.address();
			if (!address || typeof address === 'string') throw new Error('Missing private fixture TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	}
});
