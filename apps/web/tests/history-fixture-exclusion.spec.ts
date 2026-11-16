import { expect, test } from '@playwright/test';

test('ordinary demo route never exposes the test-only history bridge', async ({ page }) => {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => { HTMLMediaElement.prototype.play = () => Promise.resolve(); });
	await page.goto('/demo/player');
	for (let visit = 0; visit < 2; visit++) {
		await page.getByTestId('queue-select-demo-track').click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await expect(page.getByTestId('recently-played-clear')).toBeVisible();
		expect(await page.evaluate(() => '__nntuneHistoryFixture' in window)).toBe(false);
		if (visit === 0) await page.reload();
	}
});
