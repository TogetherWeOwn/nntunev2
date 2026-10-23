import { expect, test } from '@playwright/test';
import {
	expectSound, frames, media, muteControl, openPlayer, playCount, preference, volumeControl
} from './sound-preference/fixtures';

for (const mode of ['throw', 'ignore']) {
	test(`volume becoming ${mode === 'throw' ? 'throwing' : 'ignored'} after mount rolls the range back truthfully`, async ({ page }) => {
		const errors = await openPlayer(page);
		await page.evaluate((mode) => {
			const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'volume')!;
			Object.defineProperty(HTMLMediaElement.prototype, 'volume', {
				...descriptor,
				set() { if (mode === 'throw') throw new DOMException('System volume only', 'NotSupportedError'); }
			});
		}, mode);
		await volumeControl(page).fill('35');
		await frames(page);
		await expectSound(page, 1, false);
		await expect(volumeControl(page)).toHaveValue('100');
		await expect(volumeControl(page)).toBeDisabled();
		await expect(page.getByRole('status').filter({ hasText: "Use system volume controls." })).toBeVisible();
		expect(await playCount(page)).toBe(0);
		await muteControl(page).click();
		await expectSound(page, 1, true);
		expect((await media(page)).paused).toBe(true);
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await expectSound(page, 1, true);
		expect(await playCount(page)).toBe(1);
		expect(errors).toEqual([]);
	});
}

test('same-track error retry reapplies sound before the replacement play', async ({ page }) => {
	const errors = await openPlayer(page);
	await volumeControl(page).fill('35');
	await muteControl(page).click();
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.35, muted: true });
	const outgoing = await page.getByTestId('demo-audio').elementHandle();
	await outgoing!.evaluate((el) => el.dispatchEvent(new Event('error')));
	await expect(page.getByTestId('player-status')).toHaveText('Status: error');
	await page.getByTestId('player-retry').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await outgoing!.evaluate((el) => el.isConnected)).toBe(false);
	await expectSound(page, 0.35, true);
	expect(await page.evaluate(() => window.__soundAcceptance.plays)).toEqual([{ volume: 0.35, muted: true }]);
	expect(errors).toEqual([]);
});
