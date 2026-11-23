import { expect, test } from '@playwright/test';
import { embedOrigin, openNowPlaying, playerFootprint, setDocumentHidden, title, youtubeOrigin } from './fixtures/music-video.js';

test('the default omits video entry points and does not contact YouTube for a linked recording', async ({ page }) => {
	const { decoder, requests } = await openNowPlaying(page);
	await expect(page.getByRole('heading', { name: title })).toBeVisible();
	await expect(page.getByRole('tab', { name: 'Music Video' })).toHaveCount(0);
	await expect(page.getByText('Watch video')).toHaveCount(0);
	await expect(page.locator('iframe, script[src*="youtube"], link[href*="youtube"], link[href*="ytimg"]')).toHaveCount(0);
	expect(requests).toEqual([]);
	expect(await decoder.evaluate((node) => node.isConnected && !node.paused)).toBe(true);
});

test('verified video stays visible, retires on hide and preserves the audio queue', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 700 });
	const { decoder, requests } = await openNowPlaying(page, true);
	const audioTab = page.getByRole('tab', { name: 'Audio' });
	const videoTab = page.getByRole('tab', { name: 'Music Video' });
	const watch = page.getByRole('button', { name: 'Watch video' });
	await audioTab.press('ArrowRight');
	await expect(videoTab).toBeFocused();
	await expect(audioTab).toHaveAttribute('aria-selected', 'true');
	await videoTab.press('Enter');
	await expect(page.getByRole('link', { name: 'YouTube Terms of Service' })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Google Privacy Policy' })).toBeVisible();
	expect(requests).toEqual([]);

	const frame = page.locator('iframe[title="Fixture Video Track on YouTube"]');
	await watch.click();
	await expect(page.getByText('The video is unavailable.')).toBeVisible();
	await expect(frame).toHaveCount(0);
	await watch.click();
	await expect(page.frameLocator('iframe').getByRole('button', { name: 'Fixture native play control' })).toBeVisible();
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'playing');
	await expect(frame).toHaveAttribute('referrerpolicy', 'strict-origin-when-cross-origin');
	const embed = new URL((await frame.getAttribute('src'))!);
	expect(embed.searchParams.get('origin')).toBe(new URL(page.url()).origin);
	expect([...new Set(requests)]).toEqual([`${youtubeOrigin}/iframe_api`, `${embedOrigin}/embed/fixture0001`]);
	const { width, height, unobscured } = await playerFootprint(frame);
	expect(Math.min(width, height)).toBeGreaterThanOrEqual(200);
	expect(unobscured).toBe(true);
	const position = await decoder.evaluate((node) => node.currentTime);
	expect(await decoder.evaluate((node) => node.isConnected && node.paused)).toBe(true);

	await setDocumentHidden(page, true);
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'paused');
	await expect(frame).toHaveCount(0);
	await setDocumentHidden(page, false);
	await expect(frame).toHaveCount(0);
	expect(await decoder.evaluate((node) => ({ position: node.currentTime, paused: node.paused })))
		.toEqual({ position, paused: true });

	await watch.click();
	await expect(frame).toBeVisible();
	await page.setViewportSize({ width: 190, height: 700 });
	await expect(frame).toHaveCount(0);
	const before = [...requests];
	await watch.click();
	await expect(page.getByText('The video player needs more room.')).toBeVisible();
	expect(requests).toEqual(before);
	await page.setViewportSize({ width: 320, height: 700 });
	await watch.click();
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'playing');
	await page.evaluate(() => (window as Window & { fixtureNextTrack: () => void }).fixtureNextTrack());
	await expect(page.getByRole('heading', { name: 'Unlinked Track' })).toBeVisible();
	await expect(videoTab).toHaveCount(0);
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'paused');
});
