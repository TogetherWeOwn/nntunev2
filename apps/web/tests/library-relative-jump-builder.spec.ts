import { expect, test, type Page } from '@playwright/test';

// Builder-owned rendering regressions on the real library route. The protected
// jump bank stays unchanged; media is synthetic and no provider is contacted.
const ITEMS = [
	{ id: 'jb-coral', title: 'Coral Night', streamUrl: '/test-audio/jb-coral' },
	{ id: 'jb-pine', title: 'Pine Morning', streamUrl: '/test-audio/jb-pine' }
];
const audio = (page: Page) => page.getByTestId('audio-player');
const feedback = (page: Page) => page.getByTestId('player-seek-feedback');
const forward = (page: Page) => page.getByTestId('player-seek-forward');

async function openLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ status: 200, json: ITEMS }));
	await page.route('**/test-audio/**', () => {});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
			.getByRole('button', { name: item.title, exact: true }).click();
	}
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
}

async function mockMedia(page: Page, time: number, refused = false) {
	await audio(page).evaluate((el: HTMLAudioElement, seed) => {
		let current = seed.time;
		Object.defineProperty(el, 'duration', { value: 120, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => current,
			set: (next: number) => {
				if (seed.refused) throw new Error('native seek refused');
				current = next;
			},
			configurable: true
		});
	}, { time, refused });
}

for (const outcome of [
	{ name: 'boundary', time: 120, refused: false, text: 'Already at the end.' },
	{ name: 'jump', time: 30, refused: false, text: 'Forward 10 seconds, now at 0:40.' },
	{ name: 'refusal', time: 30, refused: true, text: 'The player did not accept that jump.' }
]) {
	test(`repeated identical ${outcome.name} replaces content in the mounted status region`, async ({ page }) => {
		await openLibrary(page);
		await mockMedia(page, outcome.time, outcome.refused);
		await forward(page).click();
		await expect(feedback(page)).toHaveText(outcome.text);
		const previousText = await feedback(page).evaluateHandle((el) =>
			Array.from(el.childNodes).find((node) => node.nodeType === Node.TEXT_NODE && node.textContent?.trim())
		);
		expect(await previousText.evaluate((node) => node?.isConnected)).toBe(true);
		if (outcome.name === 'jump') await mockMedia(page, outcome.time);
		await forward(page).click();
		await expect(feedback(page)).toHaveText(outcome.text);
		await expect.poll(() => feedback(page).evaluate((el, previous) =>
			Array.from(el.childNodes).find((node) => node.nodeType === Node.TEXT_NODE && node.textContent?.trim()) !== previous,
			previousText
		)).toBe(true);
		expect(await previousText.evaluate((node) => node?.isConnected)).toBe(false);
		await expect(page.locator('main [data-testid="player-seek-feedback"][role="status"]')).toHaveCount(1);
		await previousText.dispose();
	});
}

test('Next clears the previous track jump readout before another gesture', async ({ page }) => {
	await openLibrary(page);
	await mockMedia(page, 30);
	await forward(page).click();
	await expect(feedback(page)).toHaveText('Forward 10 seconds, now at 0:40.');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	expect(await audio(page).evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(0);
	await expect(feedback(page)).toHaveText('');
});
