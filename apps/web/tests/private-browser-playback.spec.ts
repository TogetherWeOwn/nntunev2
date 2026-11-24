import { expect, test } from '@playwright/test';
import { mockPrivatePlayback } from './fixtures/private-playback.js';
test('the verified Library player can start a track without a catalogue URL', async ({ page }) => {
	await mockPrivatePlayback(page);
	await page.goto('/library');
	await page.getByRole('button', { name: 'Fixture track', exact: true }).click();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await page.getByRole('region', { name: 'Player', exact: true }).getByRole('button', { name: 'Play', exact: true }).click();
	await expect(page.getByTestId('audio-player')).toHaveAttribute('src', /^\/v1\/plays\/media\/first\/[^/]+\/sample$/);
});
