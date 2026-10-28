import { expect, test } from '@playwright/test';

test('relative listening timestamps age without recording or replaying a track', async ({ page }) => {
	const playedAt = new Date('2026-10-28T12:00:00Z').getTime();
	await page.clock.install({ time: playedAt });
	await page.addInitScript((timestamp) => {
		sessionStorage.setItem('nntune:library-recently-played:v1', JSON.stringify([
			{ trackId: 'relative-time-track', playedAt: timestamp }
		]));
	}, playedAt);
	await page.route('**/api/library', (route) => route.fulfill({ json: [
		{ id: 'relative-time-track', title: 'Synthetic listening sample', artist: 'Synthetic artist' }
	] }));
	await page.goto('/library');
	const label = page.getByTestId('library-recent-played-at-relative-time-track');
	await expect(label).toHaveText('just now');
	const history = await page.evaluate(() => sessionStorage.getItem('nntune:library-recently-played:v1'));

	await page.clock.fastForward(120_000);

	await expect(label).toHaveText('2 min ago');
	expect(await page.evaluate(() => sessionStorage.getItem('nntune:library-recently-played:v1'))).toBe(history);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});
