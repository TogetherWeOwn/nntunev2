import { expect, test } from '@playwright/test';
import { seekProofFailure, seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';

const items = ['Coral', 'Pine'].map((title, i) => ({
	id: `seek-${i}`, title, artist: 'Synthetic audio', streamUrl: `/api/demo/audio?track=seek-${i}`
}));

test('pointer and keyboard seek the real layout decoder and follow its readback', async ({ page }) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	// This route serves a real generated WAV with HTTP Range responses.
	const rangeResponse = page.waitForResponse((response) =>
		response.url().endsWith(items[0].streamUrl) && response.status() === 206);
	await page.goto(`/library?track=${items[0].id}&t=2`);
	const region = page.getByRole('region', { name: 'Player', exact: true });
	const audio = page.getByTestId('audio-player');
	const slider = region.getByRole('slider', { name: 'Seek', exact: true });
	await rangeResponse;
	await expect(slider).toBeEnabled();
	await expect(audio).toHaveCount(1);
	await expect(audio).toHaveJSProperty('controls', true);
	const firstAudio = await audio.elementHandle();
	if (!firstAudio) throw new Error('The sole native decoder did not mount.');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => !el.seeking && el.paused &&
		Math.abs(el.currentTime - 2) < 0.05)).toBe(true);
	await audio.evaluate((el: HTMLAudioElement) => {
		el.addEventListener('seeked', () => { el.dataset.seeked = 'yes'; }, { once: true });
	});

	const nonMedia: string[] = [];
	page.on('request', (request) => { if (request.resourceType() !== 'media') nonMedia.push(request.url()); });
	const target = await seekViaPlayerUi(page, audio, 0.55);
	expect(target.surface).toBe('custom');
	const read = () => audio.evaluate((el: HTMLAudioElement) => ({
		currentTime: el.currentTime, duration: el.duration, paused: el.paused, seeking: el.seeking,
		seeked: el.dataset.seeked === 'yes'
	}));
	await expect.poll(async () => {
		const state = await read();
		return seekProofFailure(state, target.fraction, state.seeked);
	}).toBeNull();
	const sought = await read();
	expect(sought.currentTime / sought.duration).toBeCloseTo(0.55, 1);
	await expect(slider).toHaveAttribute('aria-valuetext', `0:${String(Math.floor(sought.currentTime)).padStart(2, '0')} of 0:16`);

	await slider.focus();
	await page.keyboard.press('Home');
	await expect.poll(async () => (await read()).currentTime).toBe(0);
	await expect(slider).toHaveAttribute('aria-valuetext', '0:00 of 0:16');
	await page.keyboard.press('ArrowRight');
	await expect.poll(async () => (await read()).currentTime).toBe(1);
	await expect(slider).toHaveAttribute('aria-valuetext', '0:01 of 0:16');
	const expectTime = (seconds: number) => expect.poll(async () => {
		const state = await read();
		const value = Number(await slider.inputValue());
		return !state.seeking && Math.abs(state.currentTime - seconds) < 0.05 && Math.abs(value - state.currentTime) < 0.01;
	}).toBe(true);
	await page.keyboard.press('PageUp');
	await expectTime(1 + sought.duration / 10);
	await page.keyboard.press('PageDown');
	await expectTime(1);
	for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowRight');
	await expectTime(4);
	await expect(slider).toBeFocused();

	// Simulate a partial seekable range without modifying the decoder time.
	await audio.evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'seekable', { configurable: true, get: () => ({ length: 1, start: () => 0, end: () => 5 }) });
	});
	const refusal = region.locator('p[aria-live="polite"][aria-atomic="true"] span:not([aria-hidden])');
	await page.keyboard.press('End');
	await expect(refusal).toHaveText('That position is not seekable yet.');
	const firstRefusal = await refusal.elementHandle();
	if (!firstRefusal) throw new Error('Seek feedback did not mount.');
	await expectTime(4);
	await page.keyboard.press('End');
	await expect.poll(() => firstRefusal.evaluate((el) => el.isConnected)).toBe(false);
	await expect(refusal).toHaveText('That position is not seekable yet.');
	await expectTime(4);
	await audio.evaluate((el) => { Reflect.deleteProperty(el, 'seekable'); });
	await page.keyboard.press('End');
	await expect.poll(async () => {
		const state = await read();
		return !state.seeking && Math.abs(state.currentTime - state.duration) < 0.05;
	}).toBe(true);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(slider).toHaveAttribute('aria-valuetext', '0:16 of 0:16');
	expect(nonMedia).toEqual([]);

	let release!: () => void;
	const held = new Promise<void>((resolve) => { release = resolve; });
	await page.route(/\/api\/demo\/audio\?track=seek-1$/, async (route) => {
		await held;
		await route.continue();
	});
	try {
		await page.locator('[data-testid="library-item"][data-id="seek-1"]').getByRole('button', { name: /^Pine/ }).click();
		await page.getByTestId('player-next').click();
		await expect(audio).toHaveAttribute('src', items[1].streamUrl);
		expect(await firstAudio.evaluate((el) => el.isConnected)).toBe(false);
		await expect(slider).toBeDisabled();
		await expect(slider).toHaveAttribute('aria-valuetext', '0:00 of unknown duration');
	} finally { release(); }
	await expect(slider).toBeEnabled();
	await expect(audio).toHaveCount(1);
});
