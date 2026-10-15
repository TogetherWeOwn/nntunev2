import { expect, test } from '@playwright/test';

test('audio is private-cached and artwork is public-immutable', async ({ request }) => {
	const audio = await request.get('/api/demo/audio');
	expect(audio.ok()).toBe(true);
	const audioControl = await audio.headerValue('cache-control');
	expect(audioControl).toMatch(/^private/);
	expect(audioControl).not.toContain('immutable');

	const artwork = await request.get('/api/demo/artwork');
	expect(artwork.ok()).toBe(true);
	const artworkControl = await artwork.headerValue('cache-control');
	expect(artworkControl).toMatch(/^public/);
	expect(artworkControl).toContain('immutable');

	const range = await request.get('/api/demo/audio', { headers: { range: 'bytes=0-99' } });
	expect(range.status()).toBe(206);
	expect(await range.headerValue('content-range')).toBe('bytes 0-99/262144');
	expect((await range.body()).length).toBe(100);
});

test('missing artwork falls back to the placeholder with alt text', async ({ page }) => {
	await page.goto('/demo/player');

	const art = page.getByTestId('track-artwork');
	await expect(art).toBeVisible();
	await expect(art).toHaveAttribute('alt', 'Demo track artwork');
	const src = await art.getAttribute('src');
	expect(src).toContain('placeholder-artwork');
	// No broken-image state: the rendered img has dimensions once loaded.
	await expect.poll(async () => art.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
});

test('10x rapid seeks never wedge the player', async ({ page }) => {
	await page.goto('/demo/player');

	const result = await page.evaluate(async () => {
		const storm = window.__seekStorm;
		if (!storm) throw new Error('seek storm hook missing');
		return storm();
	});

	// Nine predecessors abort; the storm plus the proving seek complete.
	expect(result.aborted).toBe(9);
	expect(result.completed).toBe(2);
	expect(result.lastBytes).toBe(100);
	await expect(page.getByTestId('player-status')).toContainText('ready');

	// The audio element still loads after the storm (not wedged).
	const audio = page.getByTestId('demo-audio');
	await expect(audio).toHaveAttribute('src', '/api/demo/audio');
});
