import { expect, test, type Page } from '@playwright/test';

/**
 * Global transport keyboard shortcuts over `/demo/player` — acceptance pins.
 *
 * Contract the player build implements (this file is the spec; do not edit
 * these pins to match an implementation, change the implementation instead):
 *
 * Key map (`event.key`, no modifiers except `?`, which is Shift+/):
 *   k ......... play/pause toggle        n ... next track
 *   p ......... previous track           j ... seek back 10s
 *   l ......... seek forward 10s         m ... mute toggle
 *   ? ......... open the shortcuts dialog
 *   Escape .... close the shortcuts dialog
 * Aliases (Space, arrows) are allowed but must not break these pins.
 *
 * Guards: every binding above (including `?`) is inert while the event
 * target is an input, textarea or contenteditable region, and while a
 * modal dialog (`dialog[open]` or a visible `[role="dialog"]`) is open.
 * Escape still closes the shortcuts dialog itself.
 *
 * Dialog: `data-testid="shortcuts-dialog"` with `role="dialog"`, listing at
 * least the seven bindings above as `kbd` key glyphs plus action labels
 * (play/pause, next, previous, mute and the 10-second magnitude). Opened via
 * `?` and via `data-testid="shortcuts-button"` (accessible name matching
 * /shortcut/i) visible on the player page. Closed via
 * `data-testid="shortcuts-close"` and via Escape; Escape returns focus to
 * the opener.
 *
 * Determinism: transport tests hold the demo-audio route and stub
 * `HTMLMediaElement.play` to a pending promise settled only by the test
 * (same harness as the player lifecycle suite), so no real decoder runs.
 * Only the seek test uses the real deterministic fixture (8 kHz PCM WAV,
 * ~16.4s): 3s + 10s = 13s stays inside the media, no clamp ambiguity.
 *
 * RED expectations: without the keymap, the k/n/p/j/l/m tests fail on
 * unchanged transport state, and the dialog tests fail on the missing
 * button/dialog. The two guard tests (typing, modal) pass vacuously
 * pre-implementation and pin the builder against hijacking them.
 * Mutation check: unbind any single key in the implementation map and its
 * test fails while the rest stay green.
 */

type PendingControl = {
	playCalls: number;
	resolve: () => void;
	reject: () => void;
};

function lifecycleOf(page: Page) {
	return page.evaluate(
		() =>
			(
				window as unknown as {
					__shortcutsLifecycle: PendingControl;
				}
			).__shortcutsLifecycle.playCalls
	);
}

/** Demo player with synthetic pending media: only fixture events settle playback. */
async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		const control: PendingControl = {
			playCalls: 0,
			resolve: () => {},
			reject: () => {}
		};
		Object.assign(window, { __shortcutsLifecycle: control });
		HTMLMediaElement.prototype.play = function (): Promise<void> {
			control.playCalls += 1;
			return new Promise<void>((resolve, reject) => {
				control.resolve = resolve;
				control.reject = () => reject(new Error('private decoder details'));
			});
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function settle(page: Page, mode: 'resolve' | 'reject') {
	await page.evaluate((next) => {
		const control = (
			window as unknown as {
				__shortcutsLifecycle: PendingControl;
			}
		).__shortcutsLifecycle;
		control[next]();
	}, mode);
}

async function fireMediaEvent(page: Page, type: string) {
	await page.getByTestId('demo-audio').evaluate((el, name) => {
		// Native play events observe paused=false; pause observes paused=true.
		if (name === 'play' || name === 'pause') {
			Object.defineProperty(el, 'paused', { value: name === 'pause', configurable: true });
		}
		el.dispatchEvent(new Event(name));
	}, type);
}

/** Blur whatever hydrated with focus so no player control holds it. */
async function focusBody(page: Page) {
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	expect(await page.evaluate(() => document.activeElement === document.body)).toBe(true);
}

test('k toggles play and pause when focus is outside the player', async ({ page }) => {
	await openPlayer(page);
	await focusBody(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');

	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await expect.poll(() => lifecycleOf(page)).toBe(1);
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	// An unrelated focused control must not steal the global key.
	await page.getByTestId('seek-storm').focus();
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');

	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await expect.poll(() => lifecycleOf(page)).toBe(2);
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
});

test('n and p move to the next and previous track without player focus', async ({ page }) => {
	await openPlayer(page);
	await focusBody(page);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');

	await page.keyboard.press('n');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await page.keyboard.press('n');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await page.keyboard.press('p');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	// Track changes reuse the page transport: the new track is loading.
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
});

test('l and j seek the demo audio forward and back 10 seconds', async ({ page }) => {
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
	});
	await page.goto('/demo/player');
	const audio = page.getByTestId('demo-audio');
	await expect(audio).toHaveAttribute('src', '/api/demo/audio');
	// Real fixture media must be seekable: wait for metadata, not a fixed sleep.
	await audio.evaluate((el: HTMLAudioElement) => {
		if (el.readyState >= 1) return;
		return new Promise<void>((resolve) =>
			el.addEventListener('loadedmetadata', () => resolve(), { once: true })
		);
	});
	const duration = await audio.evaluate((el: HTMLAudioElement) => el.duration);
	expect(duration).toBeGreaterThan(13);
	await audio.evaluate((el: HTMLAudioElement) => {
		el.currentTime = 3;
	});
	await expect
		.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.currentTime))
		.toBeCloseTo(3, 0);
	await focusBody(page);

	await page.keyboard.press('l');
	await expect
		.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.currentTime), { timeout: 5_000 })
		.toBeCloseTo(13, 0);

	await page.keyboard.press('j');
	await expect
		.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.currentTime), { timeout: 5_000 })
		.toBeCloseTo(3, 0);
});

test('m toggles mute on the demo audio', async ({ page }) => {
	await openPlayer(page);
	await focusBody(page);
	const audio = page.getByTestId('demo-audio');
	await expect.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(false);

	await page.keyboard.press('m');
	await expect.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(true);

	await page.keyboard.press('m');
	await expect.poll(async () => audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(false);
});

test('transport keys never hijack typing in inputs, textareas or editable regions', async ({
	page
}) => {
	await openPlayer(page);
	await page.evaluate(() => {
		const host = document.querySelector('main');
		if (!host) throw new Error('demo main missing');
		host.insertAdjacentHTML(
			'beforeend',
			'<input data-testid="probe-input" aria-label="probe input" />' +
				'<textarea data-testid="probe-textarea" aria-label="probe textarea"></textarea>' +
				'<div data-testid="probe-editable" contenteditable="true" aria-label="probe editable"></div>'
		);
	});

	for (const testid of ['probe-input', 'probe-textarea', 'probe-editable']) {
		const field = page.getByTestId(testid);
		await field.focus();
		await expect(field).toBeFocused();

		for (const key of ['k', 'n', 'p', 'j', 'l', 'm', '?']) await page.keyboard.press(key);

		// Keys land as text; transport, queue and dialog stay untouched.
		if (testid === 'probe-editable') {
			await expect(field).toHaveText('knpjlm?');
		} else {
			await expect(field).toHaveValue('knpjlm?');
		}
		await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
		await expect(page.getByTestId('now-playing')).toContainText('Demo track');
		await expect.poll(() => lifecycleOf(page)).toBe(0);
		await expect(page.getByTestId('shortcuts-dialog')).toHaveCount(0);
	}
});

test('transport keys are inert while a modal dialog captures keys', async ({ page }) => {
	await openPlayer(page);
	await page.evaluate(() => {
		const dialog = document.createElement('dialog');
		dialog.setAttribute('data-testid', 'probe-dialog');
		dialog.innerHTML = '<button data-testid="probe-dialog-ok">OK</button>';
		document.querySelector('main')?.append(dialog);
		dialog.showModal();
		dialog.querySelector('button')?.focus();
	});
	await expect(page.getByTestId('probe-dialog')).toBeVisible();
	await expect(page.getByTestId('probe-dialog-ok')).toBeFocused();

	for (const key of ['k', 'n', 'p', 'j', 'l', 'm', '?']) await page.keyboard.press(key);

	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => lifecycleOf(page)).toBe(0);
	await expect(page.getByTestId('shortcuts-dialog')).toHaveCount(0);
	// The probe modal itself is undisturbed: still open, focus still inside.
	const probe = await page.evaluate(() => {
		const dialog = document.querySelector<HTMLDialogElement>('[data-testid="probe-dialog"]');
		return { open: dialog?.open ?? false, inside: dialog?.contains(document.activeElement) ?? false };
	});
	expect(probe).toEqual({ open: true, inside: true });
});

test('? opens the shortcuts dialog listing the full key map', async ({ page }) => {
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await focusBody(page);

	await page.keyboard.press('?');

	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(dialog).toBeVisible();
	await expect(dialog).toHaveRole('dialog');
	// At least the seven bindings (aliases allowed): one key glyph each.
	expect(await dialog.locator('kbd').count()).toBeGreaterThanOrEqual(7);
	for (const key of ['k', 'n', 'p', 'j', 'l', 'm', '?']) {
		expect(await dialog.locator('kbd', { hasText: key }).count()).toBeGreaterThanOrEqual(1);
	}
	// Action labels name play/pause, next, previous, mute and the 10s magnitude.
	for (const label of [/play/i, /pause/i, /next/i, /prev/i, /mute/i, /shortcut/i]) {
		await expect(dialog).toContainText(label);
	}
	await expect(dialog).toContainText('10');
});

test('the shortcuts dialog opens from the player bar and closes cleanly', async ({ page }) => {
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	const button = page.getByTestId('shortcuts-button');
	await expect(button).toBeVisible();
	await expect(button).toHaveAccessibleName(/shortcut/i);
	await button.click();
	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(dialog).toBeVisible();

	await page.getByTestId('shortcuts-close').click();
	await expect(dialog).toBeHidden();

	await button.click();
	await expect(dialog).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(dialog).toBeHidden();
	await expect(button).toBeFocused();
});
