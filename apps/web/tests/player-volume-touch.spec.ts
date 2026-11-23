import { expect, test, type Locator, type Page } from '@playwright/test';

async function openPlayer(page: Page, baseURL: string) {
	const origin = new URL(baseURL).origin;
	const streamUrl = '/api/demo/audio?track=touch-volume';
	await page.route('**/*', (route) => {
		const { origin: requestOrigin, pathname, search } = new URL(route.request().url());
		if (requestOrigin !== origin) return route.abort();
		if (pathname === '/api/library') {
			return route.fulfill({ json: [{ id: 'touch-volume', title: 'Touch volume fixture', streamUrl }] });
		}
		if (pathname.startsWith('/api/') && pathname + search !== streamUrl) return route.abort();
		return route.continue();
	});
	await page.goto('/library');
	await page.getByTestId('library-item').filter({ hasText: 'Touch volume fixture' }).click();
	const player = page.getByRole('region', { name: 'Player', exact: true });
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	await expect(player.getByRole('status').filter({ hasText: /^Playing$/ })).toBeVisible();
	const audio = player.getByTestId('audio-player');
	await expect(audio).toHaveJSProperty('paused', false);
	return {
		player,
		audio,
		volume: player.getByRole('slider', { name: /^Volume / }),
		mute: player.getByRole('button', { name: 'Mute', exact: true })
	};
}

async function expectSoundLayout(player: Locator, volume: Locator, mute: Locator, width: number) {
	const playerBox = (await player.boundingBox())!;
	const controls = await player.locator('button:visible, input:visible, audio[controls]:visible').all();
	const boxes = await Promise.all(controls.map((control) => control.boundingBox()));
	expect(await player.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
	for (const target of [volume, mute]) {
		const box = (await target.boundingBox())!;
		expect(box.x).toBeGreaterThanOrEqual(playerBox.x);
		expect(box.x + box.width).toBeLessThanOrEqual(playerBox.x + playerBox.width);
		expect(box.y).toBeGreaterThanOrEqual(playerBox.y);
		expect(box.y + box.height).toBeLessThanOrEqual(playerBox.y + playerBox.height);
		const collides = boxes.filter((other) => other &&
			box.x < other.x + other.width && box.x + box.width > other.x &&
			box.y < other.y + other.height && box.y + box.height > other.y
		);
		expect(collides, 'the only intersecting interactive box is the target itself').toEqual([box]);
	}
}

async function tapUpperEdge(page: Page, control: Locator, horizontalFraction: number) {
	await control.scrollIntoViewIfNeeded();
	const { x, y, width } = (await control.boundingBox())!;
	const point = { x: Math.round(x + width * horizontalFraction), y: Math.round(y + 5) };
	expect(await control.evaluate((node, point) => document.elementFromPoint(point.x, point.y) === node, point)).toBe(true);
	await page.touchscreen.tap(point.x, point.y);
}

test.describe('coarse pointer', () => {
	test.use({ hasTouch: true, viewport: { width: 390, height: 844 } });

	test('Volume and Mute provide usable touch targets during playback', async ({ page, baseURL }) => {
		const { player, audio, volume, mute } = await openPlayer(page, baseURL!);
		expect(await page.evaluate(() => matchMedia('(pointer: coarse)').matches)).toBe(true);
		for (const control of [volume, mute]) {
			const box = (await control.boundingBox())!;
			const name = await control.getAttribute('type');
			expect.soft(box.width, `${name} interactive width (${box.width} × ${box.height})`).toBeGreaterThanOrEqual(44);
			expect.soft(box.height, `${name} interactive height (${box.width} × ${box.height})`).toBeGreaterThanOrEqual(44);
		}
		await expectSoundLayout(player, volume, mute, 390);
		await expect(volume).toHaveValue('100');
		await expect(mute).toHaveAttribute('aria-pressed', 'false');

		await tapUpperEdge(page, volume, 0.3);
		const percent = Number(await volume.inputValue());
		expect(percent).toBeGreaterThan(20);
		expect(percent).toBeLessThan(40);
		await expect(volume).toHaveAccessibleName(`Volume ${percent}%`);
		await expect(audio).toHaveJSProperty('volume', percent / 100);
		await tapUpperEdge(page, mute, 0.5);
		await expect(mute).toHaveAttribute('aria-pressed', 'true');
		await expect(audio).toHaveJSProperty('muted', true);
		await tapUpperEdge(page, mute, 0.5);
		await expect(mute).toHaveAttribute('aria-pressed', 'false');
		await expect(audio).toHaveJSProperty('muted', false);
		await expect(audio).toHaveJSProperty('volume', percent / 100);
		await expect(audio).toHaveJSProperty('paused', false);

		await page.setViewportSize({ width: 320, height: 844 });
		await expectSoundLayout(player, volume, mute, 320);
	});
});

test('desktop sound controls keep native keyboard order and visible focus', async ({ page, baseURL }) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	const { player, audio, volume, mute } = await openPlayer(page, baseURL!);
	expect(await page.evaluate(() => matchMedia('(pointer: fine)').matches)).toBe(true);
	await expectSoundLayout(player, volume, mute, 1280);

	await player.getByRole('button', { name: 'Forward 10 seconds' }).focus();
	await page.keyboard.press('Tab');
	await expect(volume).toBeFocused();
	await expect(volume).toHaveCSS('outline-style', 'solid');
	await expect(volume).toHaveCSS('outline-width', '2px');
	await page.keyboard.press('Home');
	await expect(volume).toHaveValue('0');
	await expect(audio).toHaveJSProperty('volume', 0);
	await page.keyboard.press('ArrowRight');
	await expect(volume).toHaveValue('1');
	await expect(audio).toHaveJSProperty('volume', 0.01);
	await page.keyboard.press('End');
	await expect(volume).toHaveValue('100');
	await expect(audio).toHaveJSProperty('volume', 1);
	await page.keyboard.press('Tab');
	await expect(mute).toBeFocused();
	await expect(mute).toHaveCSS('outline-style', 'solid');
	await expect(mute).toHaveCSS('outline-width', '2px');
	await page.keyboard.press('Space');
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
	await expect(audio).toHaveJSProperty('muted', true);
	await page.keyboard.press('Enter');
	await expect(mute).toHaveAttribute('aria-pressed', 'false');
	await expect(audio).toHaveJSProperty('muted', false);
	await page.keyboard.press('Shift+Tab');
	await expect(volume).toBeFocused();
	await expect(audio).toHaveJSProperty('paused', false);
});
