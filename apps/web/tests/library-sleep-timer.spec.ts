import { expect, test } from '@playwright/test';

test('a duration armed in Library pauses the same native player on Home', async ({ page }) => {
	await page.clock.install();
	await page.route('**/api/library', (route) => route.fulfill({ json: [
		{ id: 'coral', title: 'Coral', streamUrl: '/api/demo/audio?track=coral' }
	] }));
	await page.goto('/library?track=coral&t=2');
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const status = player.getByRole('status');
	const timer = page.getByRole('region', { name: 'Sleep timer' });
	const native = player.locator('audio[controls][data-testid="audio-player"]');
	await expect(native).toBeVisible();
	const audio = (await native.elementHandle())!;
	await player.getByRole('button', { name: 'Play', exact: true }).press('Enter');
	await expect(status).toHaveText('Playing');
	const minutes = timer.getByRole('spinbutton', { name: 'Sleep timer (minutes)' });
	await minutes.fill('1');
	await timer.getByRole('button', { name: 'Sleep after duration' }).press('Enter');
	await expect(timer.getByTestId('sleep-timer-status')).toHaveText('Sleep in 1 minute');
	await minutes.fill('0');
	await timer.getByRole('button', { name: 'Sleep after duration' }).press('Enter');
	await expect(minutes).toHaveAttribute('aria-invalid', 'true');
	await expect(timer.getByRole('button', { name: 'Cancel sleep timer' })).toBeEnabled();
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await page.clock.fastForward(61_000);
	await expect(timer.getByTestId('sleep-timer-status')).toHaveText('Sleep timer fired — playback paused');
	await expect(status).toHaveText('Paused');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	expect(await audio.evaluate((node: HTMLAudioElement) => node.isConnected && node.controls && node.paused)).toBe(true);
});
