import { expect, test, type Page } from '@playwright/test';

const SPEED_KEY = 'nntune:playback-speed:v1';

async function openPlayerAtHalfSpeed(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.goto('/demo/player');
	await page.waitForFunction(
		() => typeof (window as unknown as { __seekStorm?: unknown }).__seekStorm === 'function'
	);
	await page.getByTestId('playback-speed').selectOption('0.5');
	await expect.poll(() => decoderRate(page)).toBe(0.5);
	await expect.poll(() => storedSpeed(page)).toBe('0.5');
}

function decoderRate(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate);
}

function storedSpeed(page: Page) {
	return page.evaluate((key) => localStorage.getItem(key), SPEED_KEY);
}

async function saveSpeedFromAnotherTab(page: Page) {
	const otherTab = await page.context().newPage();
	try {
		const fixture = new URL('/speed-storage-fixture', page.url()).href;
		await otherTab.route(fixture, (route) =>
			route.fulfill({ contentType: 'text/html', body: '<!doctype html>' })
		);
		await otherTab.goto(fixture);
		await otherTab.evaluate((key) => localStorage.setItem(key, '1.75'), SPEED_KEY);
		await expect.poll(() => storedSpeed(page)).toBe('1.75');
	} finally {
		await otherTab.close();
	}
}

test('decoder replacement and its queued restore event preserve a newer shared speed', async ({
	page
}) => {
	await openPlayerAtHalfSpeed(page);
	await saveSpeedFromAnotherTab(page);
	await page.getByTestId('demo-audio').evaluate((oldAudio) => {
		document.addEventListener(
			'ratechange',
			(event) => {
				const el = event.target;
				if (
					el instanceof HTMLAudioElement &&
					el !== oldAudio &&
					el.dataset.testid === 'demo-audio' &&
					el.playbackRate === 0.5
				) {
					el.dataset.restoreEvent = 'seen';
				}
			},
			{ capture: true }
		);
	});

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('demo-audio')).toHaveAttribute('data-restore-event', 'seen');
	await expect(page.getByTestId('playback-speed')).toHaveValue('0.5');
	expect(await decoderRate(page)).toBe(0.5);
	expect(await storedSpeed(page)).toBe('1.75');

	await page.reload();
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.75');
	await expect.poll(() => decoderRate(page)).toBe(1.75);
});

test('a new native quarter speed is shown raw and replaces the shared value with the bound', async ({
	page
}) => {
	await openPlayerAtHalfSpeed(page);
	await saveSpeedFromAnotherTab(page);

	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.playbackRate = 0.25;
	});
	const speed = page.getByTestId('playback-speed');
	await expect(speed).toHaveValue('0.25');
	await expect(speed.locator('option:checked')).toHaveText('0.25x');
	expect(await decoderRate(page)).toBe(0.25);
	expect(await storedSpeed(page)).toBe('0.5');

	await page.reload();
	await expect(page.getByTestId('playback-speed')).toHaveValue('0.5');
	await expect.poll(() => decoderRate(page)).toBe(0.5);
});
