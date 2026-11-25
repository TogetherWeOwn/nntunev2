import { fileURLToPath } from 'node:url';
import { test as base, expect, type Page, type Route } from '@playwright/test';
import { preview } from 'vite';
import { PRIVATE_MUSIC } from '../../../../e2e/helpers/private-music-journey.js';

// Loopback HTTP uses a fixture-only cookie. The staged runner sends the real cookie over HTTPS.
export const fixtureCookie = 'private_music_fixture_session';
export function hasFixtureSession(cookie: string | null, account: string) {
	return cookie?.split('; ').includes(`${fixtureCookie}=${account}`) ?? false;
}

export function serveFlac(route: Route, body: Buffer) {
	return serveRangedAudio(route, body, 'audio/flac');
}

export function serveRangedAudio(route: Route, body: Buffer, contentType: 'audio/flac' | 'audio/wav') {
	const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
	const start = range ? Number(range[1]) : 0;
	const end = range?.[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
	if (start > end) return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${body.length}` } });
	return route.fulfill({
		status: range ? 206 : 200, contentType, body: body.subarray(start, end + 1),
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

// The private listening access check refuses new grants with this response.
export const privateListeningRefusal = { code: 'forbidden', message: 'private listening access required', retryable: false };

export async function playQueuedTrack(page: Page, origin: string, title: string) {
	await page.getByTestId(PRIVATE_MUSIC.player.queueItem)
		.getByRole('button', { name: `More actions for ${title}`, exact: true }).click();
	const [response] = await Promise.all([
		page.waitForResponse(origin + PRIVATE_MUSIC.player.plays),
		page.getByRole('menuitem', { name: 'Play now', exact: true }).click()
	]);
	return response;
}

export function expectPlaybackRefusal(page: Page) {
	return expect(page.getByRole('status').and(page.getByTestId('queue-announcement')))
		.toHaveText('Could not start playback.');
}

export function observeMediaSources(page: Page) {
	// Capture transient src assignments as well as sources that make a media request.
	return page.evaluateHandle(() => {
		const seen = new Set<string>();
		const selector = 'audio[src], audio source[src]';
		const record = (node: Element) => {
			if (node.matches(selector)) seen.add(node.getAttribute('src')!);
			node.querySelectorAll(selector).forEach((source) => seen.add(source.getAttribute('src')!));
		};
		record(document.documentElement);
		new MutationObserver((entries) => {
			for (const entry of entries) {
				if (entry.target instanceof Element && entry.target.matches('audio, audio source')) {
					if (entry.oldValue) seen.add(entry.oldValue);
					record(entry.target);
				}
				for (const node of [...entry.addedNodes, ...entry.removedNodes]) {
					if (node instanceof Element) record(node);
				}
			}
		}).observe(document, { childList: true, subtree: true, attributeFilter: ['src'], attributeOldValue: true });
		return seen;
	});
}
