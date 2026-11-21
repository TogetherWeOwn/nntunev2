import { expect, test, type Page } from '@playwright/test';

const tracks = [
	{ id: 'lib-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/lib-coral' },
	{ id: 'lib-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/lib-pine' }
];
const audio = (page: Page) => page.getByTestId('audio-player').and(page.locator('audio'));
const speed = (page: Page) => page.getByRole('combobox', { name: 'Playback speed', exact: true });
const stored = (page: Page) => page.evaluate(() => localStorage.getItem('nntune:playback-speed:v1'));

async function openLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: tracks }));
	await page.route('**/test-audio/**', () => {});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
}

async function enqueue(page: Page, index: number) {
	const track = tracks[index]!;
	await page.locator(`[data-testid="library-item"][data-id="${track.id}"]`)
		.getByRole('button', { name: new RegExp(`^${track.title}( — |$)`) }).click();
}

test('speed follows the current decoder and restores across source change and reload', async ({ page }) => {
	await openLibrary(page);
	await expect(speed(page)).toHaveCount(0);
	await enqueue(page, 0);
	await expect(speed(page)).toHaveValue('1');
	await expect(speed(page).locator('option')).toHaveText(['0.5x', '0.75x', '1x', '1.25x', '1.5x', '1.75x', '2x']);
	await speed(page).selectOption('1.5');
	await expect(audio(page)).toHaveJSProperty('playbackRate', 1.5);
	await expect.poll(() => stored(page)).toBe('1.5');
	const old = (await audio(page).elementHandle())!;
	await old.evaluate((element) => { (element as HTMLAudioElement).playbackRate = 1.3; });
	await expect(speed(page)).toHaveValue('1.3');
	await expect.poll(() => stored(page)).toBe('1.3');
	await enqueue(page, 1);
	await old.evaluate((element) => {
		(element as HTMLAudioElement).playbackRate = 0.25;
		document.querySelector<HTMLButtonElement>('[data-testid="player-next"]')!.click(); // Before the queued ratechange.
	});
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await expect(audio(page)).toHaveAttribute('src', tracks[1]!.streamUrl);
	await expect(page.locator('audio')).toHaveCount(1);
	await expect(audio(page)).toHaveJSProperty('controls', true);
	await expect(audio(page)).toHaveJSProperty('playbackRate', 0.25);
	await old.evaluate((element) => {
		(element as HTMLAudioElement).playbackRate = 2;
		element.dispatchEvent(new Event('ratechange'));
	});
	await expect(speed(page)).toHaveValue('0.25');
	expect(await stored(page)).toBe('0.5');
	await page.reload();
	await enqueue(page, 0);
	await expect(speed(page)).toHaveValue('0.5');
	await expect(audio(page)).toHaveJSProperty('playbackRate', 0.5);
});

test('a demo restore and its ratechange leave a newer Library choice intact', async ({ page, context }) => {
	const demo = await context.newPage();
	await demo.goto('/demo/player');
	await demo.evaluate(() => {
		const { set } = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'playbackRate')!;
		Object.defineProperty(HTMLMediaElement.prototype, 'playbackRate', {
			set(value: number) { set!.call(this, value === 2 ? 1.5 : value === 1.5 ? 1.25 : value); }
		});
	});
	await speed(demo).selectOption('2');
	await expect(demo.getByTestId('speed-status')).toHaveText('Speed 2x not applied — still 1.5x');
	await openLibrary(page);
	await enqueue(page, 0);
	await speed(page).selectOption('1.75');
	await demo.evaluate(() => document.addEventListener('ratechange',
		(event) => { (event.target as HTMLAudioElement).dataset.rateChanged = 'yes'; }, true));
	await demo.getByTestId('player-next').click();
	const demoAudio = demo.getByTestId('demo-audio').and(demo.locator('audio[src$="track=second-signal"]'));
	await expect(demoAudio).toHaveAttribute('data-rate-changed', 'yes');
	await expect(demoAudio).toHaveJSProperty('playbackRate', 1.25);
	await expect(speed(demo)).toHaveValue('1.25');
	await expect(demo.getByTestId('speed-status')).toBeEmpty();
	expect(await stored(page)).toBe('1.75');
	await speed(demo).selectOption('0.75');
	await expect.poll(() => stored(page)).toBe('0.75');
});

for (const refusal of ['ignore', 'clamp', 'throw'] as const) {
	test(`speed reports the observed rate when a write fails (${refusal})`, async ({ page }) => {
		await openLibrary(page);
		await enqueue(page, 0);
		await expect(speed(page)).toHaveValue('1');
		await audio(page).evaluate((element, mode) => {
			let rate = (element as HTMLAudioElement).playbackRate;
			Object.defineProperty(element, 'playbackRate', {
				get: () => rate,
				set: () => {
					if (mode === 'throw') throw new Error('Synthetic refused rate');
					if (mode === 'clamp') rate = 1.3;
					queueMicrotask(() => element.dispatchEvent(new Event('ratechange')));
				}
			});
		}, refusal);
		const status = page.getByTestId('speed-status').and(page.getByRole('status'));
		await expect(status).toBeEmpty();
		await speed(page).focus();
		await speed(page).selectOption('2');
		const observed = refusal === 'clamp' ? '1.3' : '1';
		await expect(status).toHaveText(`Speed 2x not applied — still ${observed}x`);
		await expect(speed(page)).toHaveValue(observed);
		await status.evaluate((node) => new MutationObserver(() => { node.dataset.refreshed = 'yes'; })
			.observe(node, { childList: true, characterData: true, subtree: true }));
		await speed(page).selectOption('2');
		await expect(status).toHaveAttribute('data-refreshed', 'yes');
		await expect(status).toHaveText(`Speed 2x not applied — still ${observed}x`);
		await expect(speed(page)).toHaveValue(observed);
		await expect(speed(page)).toBeFocused();
		await expect(audio(page)).toHaveJSProperty('playbackRate', Number(observed));
		expect(await stored(page)).toBe(observed);
		await expect(audio(page)).toHaveJSProperty('paused', true);
		await expect(audio(page)).toHaveJSProperty('currentTime', 0);
	});
}
