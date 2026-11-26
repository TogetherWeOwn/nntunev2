import { expect, type APIResponse } from '@playwright/test';
import { ARTWORK_PLACEHOLDER_SRC } from '../src/lib/player/artwork.js';
import { ARTWORK_PATH, AUDIO_PATH } from './fixtures/player-seek/paths.js';
import { test } from './fixtures/player-seek/test.js';

/**
 * `APIResponse` (from the `request` fixture) has no `headerValue` — that
 * exists only on page `Response`. Read headers via `headersArray`.
 */
async function responseHeader(response: APIResponse, name: string): Promise<string> {
	const headers = await response.headersArray();
	return headers.find((h) => h.name.toLowerCase() === name.toLowerCase())?.value ?? '';
}

test('audio is private-cached and artwork is public-immutable', async ({ request, playerOrigin }) => {
	const audio = await request.get(new URL(AUDIO_PATH, playerOrigin).href);
	expect(audio.ok()).toBe(true);
	expect(await responseHeader(audio, 'content-type')).toBe('audio/wav');
	const audioControl = await responseHeader(audio, 'cache-control');
	expect(audioControl).toMatch(/^private/);
	expect(audioControl).not.toContain('immutable');

	const artwork = await request.get(new URL(ARTWORK_PATH, playerOrigin).href);
	expect(artwork.ok()).toBe(true);
	const artworkControl = await responseHeader(artwork, 'cache-control');
	expect(artworkControl).toMatch(/^public/);
	expect(artworkControl).toContain('immutable');

	const range = await request.get(new URL(AUDIO_PATH, playerOrigin).href, { headers: { range: 'bytes=0-99' } });
	expect(range.status()).toBe(206);
	expect(await responseHeader(range, 'content-range')).toBe('bytes 0-99/262144');
	const rangeBody = await range.body();
	expect(rangeBody.length).toBe(100);
	expect(rangeBody.toString('ascii', 0, 4)).toBe('RIFF');
	expect(rangeBody.toString('ascii', 8, 12)).toBe('WAVE');
});

test('missing artwork falls back to the placeholder with alt text', async ({ page, openPlayer }) => {
	await openPlayer();

	const art = page.getByTestId('null-artwork').getByTestId('track-artwork');
	await expect(art).toBeVisible();
	await expect(art).toHaveAttribute('alt', 'Synthetic track artwork');
	const src = await art.getAttribute('src');
	expect(src).toContain('placeholder-artwork');
	// No broken-image state: the rendered img has dimensions once loaded.
	await expect.poll(async () => art.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
});

test('artwork that fails to load swaps to the placeholder once', async ({ page, openPlayer }) => {
	await openPlayer();

	const art = page.getByTestId('broken-artwork').getByTestId('track-artwork');
	await expect(art).toHaveAttribute('data-fallback', 'true');
	await expect(art).toHaveAttribute('src', /placeholder-artwork/);
	await expect.poll(async () => art.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
});

test('a failed artwork fallback renders alt content instead of a broken image', async ({ page, openPlayer }) => {
	await openPlayer();

	const fallback = page.getByTestId('fallback-failure-artwork');
	await expect(fallback.getByTestId('track-artwork-fallback')).toBeVisible();
	await expect(fallback.getByTestId('track-artwork')).toHaveCount(0);
	await expect(fallback.getByRole('img')).toHaveAccessibleName('Fallback failure artwork');
});

test('valid artwork renders after a failed placeholder and track change', async ({ page, openPlayer }) => {
	await page.route((url) => url.pathname === ARTWORK_PLACEHOLDER_SRC, (route) => route.fulfill({ status: 404 }));
	await openPlayer();

	const artwork = page.getByTestId('current-artwork');
	await expect(artwork.getByTestId('track-artwork-fallback')).toBeVisible();
	await page.getByTestId('player-next').click();
	const image = artwork.getByTestId('track-artwork');
	await expect(image).toBeVisible();
	await expect(image).toHaveAttribute('src', ARTWORK_PATH);
	await expect.poll(() => image.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
});

test('10x rapid seeks never wedge the player', async ({ page, openPlayer }) => {
	await openPlayer();
	const result = await page.evaluate(() => window.seekFixtureStorm());

	// Nine predecessors abort; the storm plus the proving seek complete.
	expect(result.aborted).toBe(9);
	expect(result.completed).toBe(2);
	expect(result.lastBytes).toBe(100);
	await expect(page.getByTestId('player-status')).toContainText('ready');

	// The audio element still loads after the storm (not wedged).
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveAttribute('src', AUDIO_PATH);
	await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.duration)).toBeGreaterThan(0);
});

test('a late range read cannot replace or credit the current seek', async ({ page, openPlayer }) => {
	await openPlayer();
	const result = await page.evaluate(async () => {
		const readStarted = Promise.withResolvers<void>();
		const releaseRead = Promise.withResolvers<void>();
		const requests: string[] = [];
		let lateBytes: number[] = [];
		const player = window.createSeekFixture(async (_url, { headers }) => {
			requests.push(headers.range);
			if (headers.range === 'bytes=10-11') {
				const response = new Response(Uint8Array.of(10, 11), {
					status: 206, headers: { 'content-range': 'bytes 10-11/100' }
				});
				const body = response.body;
				if (!body) throw new Error('Missing fixture response body');
				const reader = body.getReader();
				const read = reader.read.bind(reader);
				// Hold the fetched chunk at the read boundary, even if the real reader is cancelled.
				reader.read = async () => {
					reader.read = read;
					const chunk = await read();
					readStarted.resolve();
					await releaseRead.promise;
					lateBytes = Array.from(chunk.value ?? []);
					return chunk;
				};
				Object.defineProperty(body, 'getReader', { value: () => reader });
				return response;
			}
			if (headers.range !== 'bytes=30-32') throw new Error('Unexpected fixture range');
			return new Response(Uint8Array.of(30, 31, 32), {
				status: 206, headers: { 'content-range': 'bytes 30-32/100' }
			});
		});

		const earlier = player.seek(10, 11).then(
			({ bytes }) => Array.from(bytes),
			(error: DOMException) => error.name
		);
		await Promise.race([
			readStarted.promise,
			earlier.then(() => { throw new Error('Earlier seek settled before the read was parked'); })
		]);
		try {
			const current = await player.seek(30, 32);
			const before = player.snapshot();
			releaseRead.resolve();
			return {
				requests, before, currentBytes: Array.from(current.bytes),
				earlier: await earlier, lateBytes, after: player.snapshot()
			};
		} finally {
			releaseRead.resolve();
		}
	});

	expect(result).toEqual({
		requests: ['bytes=10-11', 'bytes=30-32'],
		before: { position: 30, lastBytes: 3, completed: 1, aborted: 1 },
		currentBytes: [30, 31, 32],
		earlier: 'AbortError',
		lateBytes: [10, 11],
		after: { position: 30, lastBytes: 3, completed: 1, aborted: 1 }
	});
});
