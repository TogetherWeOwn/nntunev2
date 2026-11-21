import { expect, test, type Locator, type Page } from '@playwright/test';

const track = {
	id: 'compact-coral', title: 'Coral Night', artist: 'Fixture Artist',
	streamUrl: '/api/demo/audio?track=compact-coral'
};

async function openPlayer(page: Page, width: number, theme: 'dark' | 'light' = 'light', fixture = track) {
	await page.setViewportSize({ width, height: 900 });
	await page.emulateMedia({ colorScheme: theme });
	await page.route('**/api/library', (route) => route.fulfill({ json: [fixture] }));
	await page.goto(`/library?track=${fixture.id}&t=2`);
	const region = page.getByRole('region', { name: 'Player', exact: true });
	const audio = region.locator('audio[controls][data-testid="audio-player"]');
	const identity = region.getByTestId('library-now-playing');
	const favorite = region.getByRole('button', { name: `Favorite ${fixture.title}`, exact: true });
	const play = region.getByRole('button', { name: 'Play', exact: true });
	const restart = region.getByRole('button', { name: 'Restart current track', exact: true });
	const seek = region.getByRole('slider', { name: 'Seek', exact: true });
	const back = region.getByRole('button', { name: 'Back 10 seconds', exact: true });
	const forward = region.getByRole('button', { name: 'Forward 10 seconds', exact: true });
	const feedback = region.getByTestId('player-seek-feedback');
	const volume = region.getByRole('slider', { name: 'Volume 100%', exact: true });
	const mute = region.getByRole('button', { name: 'Mute', exact: true });
	await expect(identity).toHaveText(`${fixture.title} — ${fixture.artist}`);
	await expect(play).toBeEnabled();
	await expect(restart).toBeEnabled();
	await expect(seek).toHaveAttribute('aria-valuetext', '0:02 of 0:16');
	await expect(volume).toBeEnabled();
	await expect(favorite).toBeEnabled();
	await expect(audio).toBeVisible();
	await expect(page.locator('audio')).toHaveCount(1);
	await expect(feedback).toHaveAttribute('aria-live', 'polite');
	return { region, audio, identity, favorite, play, restart, seek, back, forward, feedback, volume, mute };
}

async function tabThrough(page: Page, ...stops: Locator[]) {
	for (const stop of stops) {
		await page.keyboard.press('Tab');
		await expect(stop).toBeFocused();
	}
}

async function tabThroughNativeAudio(page: Page, audio: Locator, next: Locator) {
	await tabThrough(page, audio);
	// Chromium exposes several UA shadow controls as focus on the same audio host.
	for (let i = 0; i < 12; i++) {
		await page.keyboard.press('Tab');
		if (await next.evaluate((node) => document.activeElement === node)) return;
		await expect(audio).toBeFocused();
	}
	await expect(next).toBeFocused();
}

async function box(locator: Locator) {
	await expect(locator).toBeVisible();
	return (await locator.boundingBox())!;
}

for (const width of [800, 1024, 1280]) {
	for (const theme of ['light', 'dark'] as const) {
		test(`desktop ${width}px ${theme}: primary row and directly adjacent jump extension`, async ({ page }) => {
			const { region, audio, identity, favorite, play, restart, seek, back, forward, volume } = await openPlayer(page, width, theme);
			const primary = await box(region.locator('.nt-player-primary'));
			const [label, star, transport, native, restartButton, slider, backButton, forwardButton, sound] = await Promise.all(
				[identity, favorite, play, audio, restart, seek, back, forward, volume].map(box)
			);
			expect(primary.height).toBeGreaterThanOrEqual(72);
			for (const control of [label, star, transport, native, restartButton, slider, sound]) {
				expect(control.y).toBeGreaterThanOrEqual(primary.y);
				expect(control.y + control.height).toBeLessThanOrEqual(primary.y + primary.height + 1);
			}
			expect(label.x + label.width).toBeLessThan(primary.x);
			expect(sound.x).toBeGreaterThan(primary.x + primary.width);
			expect(transport.height).toBeGreaterThanOrEqual(56);
			expect(restartButton.height).toBeGreaterThanOrEqual(48);
			expect(native.width).toBeGreaterThanOrEqual(300);
			expect(slider.height).toBeGreaterThanOrEqual(12);
			expect(slider.y).toBeGreaterThanOrEqual(restartButton.y + restartButton.height);
			const seekRow = await box(region.locator('.nt-player-seek'));
			expect(backButton.y).toBeCloseTo(seekRow.y + seekRow.height + 8, 0);
			expect(forwardButton.y).toBe(backButton.y);
			expect(forwardButton.x).toBeCloseTo(backButton.x + backButton.width + 8, 0);
			for (const jump of [back, forward]) {
				expect((await box(jump)).height).toBeGreaterThanOrEqual(48);
				await expect(jump).toBeEnabled();
				await jump.click({ trial: true }); // browser hit-test, without activating the jump
			}
			const main = await box(page.getByRole('main'));
			expect((await box(region)).y).toBeGreaterThanOrEqual(main.y + main.height);
		});
	}
}

test('desktop 800px: wrapped metadata does not separate Seek from the jump buttons', async ({ page }) => {
	const fixture = { ...track, title: Array(12).fill(track.title).join(' '), artist: Array(4).fill(track.artist).join(' ') };
	const { region, identity, back, forward } = await openPlayer(page, 800, 'light', fixture);
	const [label, seekRow, backButton, forwardButton] = await Promise.all(
		[identity, region.locator('.nt-player-seek'), back, forward].map(box)
	);
	expect(label.height).toBeGreaterThan(120);
	for (const jump of [backButton, forwardButton]) {
		expect(jump.y).toBeCloseTo(seekRow.y + seekRow.height + 8, 0);
	}
});

test('desktop Tab reaches named controls in order, with native stops and keyboard jump feedback', async ({ page }) => {
	const { audio, favorite, play, restart, seek, back, forward, feedback, volume, mute } = await openPlayer(page, 1024);
	await favorite.focus();
	await tabThrough(page, play);
	await tabThroughNativeAudio(page, audio, restart);
	await tabThrough(page, seek, back);
	await page.keyboard.press('Enter');
	await expect(back).toBeFocused();
	await expect(feedback).toHaveText('Back to the start, 0:00.');
	await tabThrough(page, forward);
	await page.keyboard.press('Space');
	await expect(forward).toBeFocused();
	await expect(feedback).toHaveText('Forward 10 seconds, now at 0:10.');
	await tabThrough(page, volume, mute);
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await expect(page.getByTestId('player-next')).toBeVisible();
});

for (const width of [320, 360, 390, 799]) {
	test(`ordinary ${width}px stays stacked with the legacy keyboard order`, async ({ page }) => {
		const { region, audio, play, restart, favorite, seek, volume, mute, back, forward } = await openPlayer(page, width);
		await expect(region.locator('.nt-player-row')).toHaveCSS('display', 'contents');
		expect((await box(volume)).y).toBeGreaterThan((await box(seek)).y);
		expect((await box(back)).y).toBeGreaterThan((await box(mute)).y);
		await play.focus();
		await tabThroughNativeAudio(page, audio, restart);
		await tabThrough(page, favorite, seek, volume, mute, back, forward);
	});
}

test('crossing the shell breakpoint keeps decoder, seek node, position and accessory focus', async ({ page }) => {
	const { audio, seek, favorite, mute } = await openPlayer(page, 800);
	const decoder = await audio.elementHandle();
	const slider = await seek.elementHandle();
	await favorite.focus();
	await page.setViewportSize({ width: 799, height: 900 });
	await expect(favorite).toBeFocused();
	await mute.focus();
	await page.setViewportSize({ width: 800, height: 900 });
	await expect(mute).toBeFocused();
	expect(await audio.evaluate((node, original) => node === original, decoder)).toBe(true);
	expect(await seek.evaluate((node, original) => node === original, slider)).toBe(true);
	await expect(audio).toHaveJSProperty('currentTime', 2);
	await expect(audio).toHaveJSProperty('paused', true);
});
