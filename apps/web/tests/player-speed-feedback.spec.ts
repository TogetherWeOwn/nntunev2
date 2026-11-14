import { expect, test } from '@playwright/test';
// EL TWO-5795 (demo/player retirement): this spec is hosted on the retired
// `/demo/player` surface and the deleted `/api/demo/*` synthetic-media endpoints.
// Vectors and case IDs are preserved; migration to the product surface is
// deferred per fallback (b) — see the PR body disposition manifest.
test.skip(true, 'EL TWO-5795: demo/player retirement — migration deferred, vectors preserved');


test('keeps failure feedback through its matching native ratechange', async ({ page }) => {
	await page.addInitScript(() => {
		localStorage.removeItem('nntune:playback-speed:v1');
		sessionStorage.removeItem('nntune:demo-queue:v1');
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('playback-speed')).toBeVisible();

	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		const audio = el as HTMLAudioElement & { setNativeRate?: (value: number) => void };
		let rate = el.playbackRate;
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => rate,
			set: (value: number) => {
				rate = Math.min(value, 1.25);
				setTimeout(() => el.dispatchEvent(new Event('ratechange')), 0);
			}
		});
		audio.setNativeRate = (value) => {
			rate = value;
			el.dispatchEvent(new Event('ratechange'));
		};
	});

	await page.getByTestId('playback-speed').selectOption('1.5');
	const feedback = page.getByTestId('speed-status');
	await expect(feedback).toHaveText('Speed 1.5x not applied — still 1.25x');

	// The event emitted by the clamped write must not immediately erase why
	// the request failed. A later, distinct native change clears that stale note.
	await page.waitForTimeout(50);
	await expect(feedback).toHaveText('Speed 1.5x not applied — still 1.25x');
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		(el as HTMLAudioElement & { setNativeRate?: (value: number) => void }).setNativeRate?.(1);
	});
	await expect(feedback).toHaveCount(0);
});
