import { expect, test } from '@playwright/test';

test('a duration armed in Library pauses the same player on return to a visible Home tab', async ({ page }) => {
	await page.clock.install({ time: '2026-01-01T00:00:00Z' });
	await page.clock.pauseAt('2026-01-01T00:01:00Z');
	const coral = { id: 'coral', title: 'Coral', streamUrl: '/api/demo/audio?track=coral' };
	await page.route('**/api/library', (route) => route.fulfill({ json: [coral] }));
	await page.goto('/library?track=coral&t=2');
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const status = player.getByRole('status');
	const timer = page.getByRole('region', { name: 'Sleep timer' });
	const audio = (await player.locator('audio[controls][data-testid="audio-player"]').elementHandle())!;
	await audio.evaluate((node: HTMLAudioElement) => {
		node.loop = true; // Keep the short fixture from ending on slow runners.
		const pause = node.pause.bind(node);
		node.pause = () => { node.dataset.pauses = String(Number(node.dataset.pauses ?? 0) + 1); pause(); };
	});
	const identity = player.getByTestId('library-now-playing');
	await expect(identity).toHaveText('Coral');
	await player.getByRole('button', { name: 'Play', exact: true }).press('Enter');
	await expect(status).toHaveText('Playing');
	const minutes = timer.getByRole('spinbutton', { name: 'Sleep timer (minutes)' });
	const arm = timer.getByRole('button', { name: 'Sleep after duration' });
	const timerStatus = timer.getByTestId('sleep-timer-status');
	await minutes.fill('1');
	await arm.press('Enter');
	await page.clock.runFor(30_000);
	await minutes.fill('0');
	await arm.press('Enter');
	await expect(minutes).toHaveAttribute('aria-invalid', 'true');
	await expect(minutes).toHaveAccessibleDescription(/positive finite number/);
	await expect(timer.getByRole('alert')).toBeVisible();
	await expect(timerStatus).toHaveText('Sleep in 1 minute');
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).press('Enter');
	await page.clock.runFor(29_500); // Also releases the router's deferred link navigation.
	await expect(page).toHaveURL(/\/home$/);
	await expect(timer.getByTestId('sleep-timer-remaining')).toHaveText('0:01');
	await page.evaluate(() => Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' }));
	await page.clock.setSystemTime(new Date('2026-01-01T00:02:01Z')); // No interval callbacks.
	expect(await audio.evaluate((node: HTMLAudioElement) => [node.paused, node.dataset.pauses ?? '0'])).toEqual([false, '0']);
	await page.evaluate(() => {
		Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect(timerStatus).toHaveText('Sleep timer fired — playback paused');
	await expect(status).toHaveText('Paused');
	await expect(identity).toHaveText('Coral');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	expect(await audio.evaluate((node: HTMLAudioElement) => [node.isConnected, node.controls, node.paused, node.dataset.pauses])).toEqual([true, true, true, '1']);
});
