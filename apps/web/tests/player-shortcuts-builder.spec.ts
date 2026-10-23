import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned pins around the protected `player-shortcuts.spec.ts` suite:
 * modified keys stay inert, lone Escape is a no-op, the dialog never
 * duplicates, transport works from queue controls, mute survives track
 * changes and follows native control changes, and seeks clamp at the
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

test('role=dialog guards visible modals without breaking shortcuts when hidden', async ({ page }) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await openPlayer(page);
	await page.evaluate(() => {
		const dialog = document.createElement('div');
		dialog.setAttribute('role', 'dialog');
		dialog.setAttribute('data-testid', 'role-dialog-probe');
		dialog.textContent = 'Confirmation';
		dialog.hidden = true;
		document.querySelector('main')?.append(dialog);
	});
	const audio = page.getByTestId('demo-audio');
	const probe = page.getByTestId('role-dialog-probe');

	await page.keyboard.press('m');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(true);

	await probe.evaluate((el: HTMLElement) => { el.hidden = false; });
	await expect(probe).toBeVisible();
	for (const key of ['k', 'n', 'p', 'j', 'l', 'm', '?']) await page.keyboard.press(key);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(true);
	await expect(page.getByTestId('shortcuts-dialog')).toHaveCount(0);

	await probe.evaluate((el: HTMLElement) => { el.hidden = true; });
	await page.keyboard.press('m');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(false);
	expect(errors).toEqual([]);
});

for (const opener of ['?', 'button'] as const) {
	test(`shortcuts dialog takes modal focus from ${opener} and restores it on close`, async ({ page }) => {
		await openPlayer(page);
		const button = page.getByTestId('shortcuts-button');
		await button.focus();
		await page.keyboard.press(opener === '?' ? '?' : 'Enter');
		const dialog = page.getByTestId('shortcuts-dialog');
		const close = page.getByTestId('shortcuts-close');
		await expect(dialog).toBeVisible();
		await expect(close).toBeFocused();

		const next = page.getByTestId('player-next');
		await next.evaluate((el: HTMLButtonElement) => el.focus());
		await expect(close).toBeFocused();
		await page.keyboard.press('n');
		await expect(page.getByTestId('now-playing')).toContainText('Demo track');

		await page.keyboard.press('Escape');
		await expect(dialog).toHaveCount(0);
		await expect(button).toBeFocused();

		await page.keyboard.press('Enter');
		await expect(close).toBeFocused();
		await close.click();
		await expect(dialog).toHaveCount(0);
		await expect(button).toBeFocused();
	});
}

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

test('mute persists across track changes', async ({ page }) => {
	await openPlayer(page);
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	const audio = page.getByTestId('demo-audio');

	await page.keyboard.press('m');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(true);

	// The track change recreates the keyed audio element; the now-playing
	// assertion proves the new track rendered before mute is re-checked.
	await page.keyboard.press('n');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(true);

	await page.keyboard.press('m');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(false);
});

test('mute shortcut follows native control changes', async ({ page }) => {
	await openPlayer(page);
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	const audio = page.getByTestId('demo-audio');

	// Muting through native controls fires volumechange, which the
	// shortcut state must absorb so the next `m` unmutes instead of
	// re-applying stale state.
	await audio.evaluate((el: HTMLAudioElement) => {
		el.muted = true;
	});
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(true);

	await page.keyboard.press('m');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(false);
});
