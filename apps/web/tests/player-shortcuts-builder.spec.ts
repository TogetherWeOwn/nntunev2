import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned pins around the protected `player-shortcuts.spec.ts` suite:
 * modified keys stay inert, lone Escape is a no-op, the dialog never
 * duplicates, transport works from queue controls, and seeks clamp at the
 * media bounds. Same pending-media harness as the acceptance suite.
 */

function lifecycleOf(page: Page) {
	return page.evaluate(
		() =>
			(
				window as unknown as {
					__shortcutsLifecycle: { playCalls: number };
				}
			).__shortcutsLifecycle.playCalls
	);
}

async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		const control = { playCalls: 0, resolve: () => {}, reject: () => {} };
		Object.assign(window, { __shortcutsLifecycle: control });
		HTMLMediaElement.prototype.play = function (): Promise<void> {
			control.playCalls += 1;
			return new Promise<void>(() => {});
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

test('modified keys and lone Escape never touch transport', async ({ page }) => {
	await openPlayer(page);
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});

	// Dispatched, not pressed: real Ctrl/Alt chords risk browser chrome
	// accelerators, while the page only sees the keydown either way.
	await page.evaluate(() => {
		for (const init of [
			{ key: 'k', ctrlKey: true },
			{ key: 'n', altKey: true },
			{ key: 'm', metaKey: true }
		] as KeyboardEventInit[]) {
			window.dispatchEvent(new KeyboardEvent('keydown', { ...init, bubbles: true }));
		}
	});
	await page.keyboard.press('Escape');

	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => lifecycleOf(page)).toBe(0);
	await expect(page.getByTestId('shortcuts-dialog')).toHaveCount(0);
});

test('transport works from queue controls and ? never duplicates the dialog', async ({
	page
}) => {
	await openPlayer(page);
	const queueButton = page.getByTestId('queue-item').first().locator('button').first();
	await queueButton.focus();
	await expect(queueButton).toBeFocused();

	await page.keyboard.press('n');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');

	await page.keyboard.press('?');
	await page.keyboard.press('?');
	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(dialog).toHaveCount(1);
	await expect(dialog).toBeVisible();

	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(queueButton).toBeFocused();
});

test('seek clamps at the media bounds instead of overshooting', async ({ page }) => {
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
	});
	await page.goto('/demo/player');
	const audio = page.getByTestId('demo-audio');
	await audio.evaluate((el: HTMLAudioElement) => {
		if (el.readyState >= 1) return;
		return new Promise<void>((resolve) =>
			el.addEventListener('loadedmetadata', () => resolve(), { once: true })
		);
	});
	const duration = await audio.evaluate((el: HTMLAudioElement) => el.duration);
	expect(duration).toBeGreaterThan(13);
	await audio.evaluate((el: HTMLAudioElement, target: number) => {
		el.currentTime = target;
	}, duration - 5);
	await expect
		.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.currentTime))
		.toBeCloseTo(duration - 5, 0);
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});

	await page.keyboard.press('l');
	await expect
		.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.currentTime), { timeout: 5_000 })
		.toBeCloseTo(duration, 0);

	await audio.evaluate((el: HTMLAudioElement) => {
		el.currentTime = 3;
	});
	await expect
		.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.currentTime))
		.toBeCloseTo(3, 0);

	await page.keyboard.press('j');
	await expect
		.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.currentTime), { timeout: 5_000 })
		.toBeCloseTo(0, 0);
});
