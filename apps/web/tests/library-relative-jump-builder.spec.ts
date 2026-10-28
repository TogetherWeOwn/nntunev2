import { expect, test, type Page } from '@playwright/test';

// Builder-owned rendering regressions on the real library route. The protected
// jump bank stays unchanged; media is synthetic and no provider is contacted.
const ITEMS = [
	{ id: 'jb-coral', title: 'Coral Night', streamUrl: '/test-audio/jb-coral' },
	{ id: 'jb-pine', title: 'Pine Morning', streamUrl: '/test-audio/jb-pine' }
];
const audio = (page: Page) => page.getByTestId('audio-player');
const feedback = (page: Page) => page.getByTestId('player-seek-feedback');
const back = (page: Page) => page.getByTestId('player-seek-back');
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

async function mockMedia(page: Page, time: number, refused = false, duration = 120) {
	await audio(page).evaluate((el: HTMLAudioElement, seed) => {
		let current = seed.time;
		const receipts = { writes: [] as number[], plays: 0, pauses: 0 };
		Object.defineProperty(el, '__jumpReceipts', { value: receipts, configurable: true });
		Object.defineProperty(el, 'duration', { get: () => seed.duration, configurable: true });
		Object.defineProperty(el, 'seekable', {
			get: () => ({ length: 1, start: () => 0, end: () => 120 }), configurable: true
		});
		Object.defineProperty(el, 'currentTime', {
			get: () => current,
			set: (next: number) => {
				receipts.writes.push(next);
				if (seed.refused) throw new Error('native seek refused');
				current = next;
			},
			configurable: true
		});
		Object.defineProperty(el, 'play', { value: () => { receipts.plays++; return Promise.resolve(); }, configurable: true });
		Object.defineProperty(el, 'pause', { value: () => { receipts.pauses++; }, configurable: true });
	}, { time, refused, duration });
}

type Receipts = { writes: number[]; plays: number; pauses: number };
const receipts = (page: Page) => audio(page).evaluate((el) =>
	(el as HTMLAudioElement & { __jumpReceipts: Receipts }).__jumpReceipts
);

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

for (const vector of [
	{ name: 'NaN duration', time: 30, duration: Number.NaN },
	{ name: 'infinite duration', time: 30, duration: Number.POSITIVE_INFINITY },
	{ name: 'zero duration', time: 30, duration: 0 },
	{ name: 'NaN time', time: Number.NaN, duration: 120 },
	{ name: 'infinite time', time: Number.POSITIVE_INFINITY, duration: 120 },
	{ name: 'negative time', time: -5, duration: 120 },
	{ name: 'past-end time', time: 121, duration: 120 }
]) {
	test(`successful jump is replaced by unavailable feedback for ${vector.name}`, async ({ page }) => {
		await openLibrary(page);
		await mockMedia(page, 30);
		await forward(page).click();
		await expect(feedback(page)).toHaveText('Forward 10 seconds, now at 0:40.');
		const status = await feedback(page).elementHandle();
		await mockMedia(page, vector.time, false, vector.duration);
		await forward(page).click();
		await expect(page.locator('main [data-testid="player-seek-feedback"][role="status"]')).toHaveCount(1);
		await expect(feedback(page)).toHaveText('Cannot jump yet: this track has not reported its length.');
		expect(await status?.evaluate((el) => el.isConnected)).toBe(true);
		expect(await receipts(page)).toEqual({ writes: [], plays: 0, pauses: 0 });
		await status?.dispose();
	});
}

for (const colorScheme of ['light', 'dark'] as const) {
	test.describe(`320px ${colorScheme} jump controls`, () => {
		test.use({ viewport: { width: 320, height: 700 }, colorScheme, hasTouch: true });

		test('both controls receive sequential keyboard focus and work with Enter, Space, and touch', async ({ page }) => {
			await openLibrary(page);
			for (const control of [back, forward]) {
				for (const key of ['Enter', 'Space'] as const) {
					await mockMedia(page, 30);
					const button = control(page);
					// Only actual Tab presses may move focus to the target; cycle if needed.
					let reached = false;
					for (let i = 0; i < 200; i++) {
						await page.keyboard.press('Tab');
						if (await button.evaluate((el) => el === document.activeElement)) { reached = true; break; }
					}
					expect(reached).toBe(true);
					await expect(button).toBeFocused();
					await button.evaluate((el) => el.scrollIntoView({ block: 'center' }));
					const appearance = await button.evaluate((el) => {
						const style = getComputedStyle(el);
						const rect = el.getBoundingClientRect();
						return { visible: el.matches(':focus-visible'), outline: style.outlineStyle,
							width: parseFloat(style.outlineWidth), x: rect.left, right: rect.right,
							hit: el.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2)),
							y: rect.top, bottom: rect.bottom, viewportWidth: innerWidth, viewportHeight: innerHeight };
					});
					expect(appearance.hit).toBe(true);
					expect(appearance.visible).toBe(true);
					expect(appearance.outline).not.toBe('none');
					expect(appearance.width).toBeGreaterThan(0);
					expect(appearance.x).toBeGreaterThanOrEqual(0);
					expect(appearance.right).toBeLessThanOrEqual(appearance.viewportWidth);
					expect(appearance.y).toBeGreaterThanOrEqual(0);
					expect(appearance.bottom).toBeLessThanOrEqual(appearance.viewportHeight);
					await page.keyboard.press(key);
					await expect.poll(() => audio(page).evaluate((el: HTMLAudioElement) => el.currentTime))
						.toBe(control === back ? 20 : 40);
				}
				await mockMedia(page, 30);
				await control(page).tap();
				await expect.poll(() => audio(page).evaluate((el: HTMLAudioElement) => el.currentTime))
					.toBe(control === back ? 20 : 40);
			}
		});
	});
}

test('neither jump initiates script fetch or changes playback intent', async ({ page }) => {
	await openLibrary(page);
	await mockMedia(page, 30);
	await page.evaluate(() => {
		const attempts: string[] = [];
		Object.assign(window, { __jumpFetchAttempts: attempts });
		window.fetch = (input) => {
			attempts.push(String(input));
			return Promise.resolve(new Response(null, { status: 204 }));
		};
	});
	for (const control of [back, forward]) {
		await control(page).click();
		await expect(feedback(page)).toContainText(control === back ? 'Back 10 seconds' : 'Forward 10 seconds');
	}
	// Allow deferred fire-and-forget work to run before inspecting attempts.
	await page.waitForTimeout(250);
	expect(await page.evaluate(() => (window as Window & { __jumpFetchAttempts: string[] }).__jumpFetchAttempts)).toEqual([]);
	expect(await receipts(page)).toEqual({ writes: [20, 30], plays: 0, pauses: 0 });
});

test('Forward preserves non-default volume and playback rate', async ({ page }) => {
	await openLibrary(page);
	await mockMedia(page, 30);
	await audio(page).evaluate((el: HTMLAudioElement) => { el.volume = 0.35; el.playbackRate = 1.5; });
	await forward(page).click();
	await expect(feedback(page)).toHaveText('Forward 10 seconds, now at 0:40.');
	expect(await audio(page).evaluate((el: HTMLAudioElement) => ({
		time: el.currentTime, volume: el.volume, rate: el.playbackRate
	}))).toEqual({ time: 40, volume: 0.35, rate: 1.5 });
	expect(await receipts(page)).toEqual({ writes: [40], plays: 0, pauses: 0 });
});

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
