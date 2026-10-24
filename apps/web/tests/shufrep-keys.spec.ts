import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance-owned, test-only handoff: `s` shuffles upcoming, `r` cycles repeat.
 * Builder owns implementation, GREEN and the combined ready PR. These new paths
 * and all existing tests are protected; return an incorrect assertion to its author.
 *
 * From repo root (offline locked dependencies):
 * pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 * pnpm --dir apps/web test:e2e tests/shufrep-keys.spec.ts --workers=1
 * pnpm --dir apps/web check
 * Unit companion: pnpm --dir apps/web test src/lib/player/shortcuts.shufrep-keys.acceptance.test.ts
 * Baselines: pnpm --dir apps/web test src/lib/player/shortcuts.test.ts
 * pnpm --dir apps/web test:e2e tests/player-shortcuts.spec.ts --workers=1
 *
 * Playwright builds/owns the local preview through its existing webServer config.
 * Port 4173 must belong to THIS checkout (use CI=1 to refuse silent server reuse).
 *
 * Contract hooks: the existing demo queue rows (`data-testid="queue-item"`,
 * `data-track-id`, per-track `queue-select-<id>` with `aria-current`), the
 * existing repeat select (`data-testid="player-repeat"`, values off/one/queue)
 * and its live status (`data-testid="repeat-status"`), the shortcuts dialog
 * (`data-testid="shortcuts-dialog"` rendering one `kbd` per SHORTCUT_ROWS row),
 * and the `player-status` / `now-playing` / `radio-badge` pins. No new test IDs
 * are required: `s` reuses the button's guarded queue path and `r` reuses the
 * select's repeat state.
 *
 * Media/network are synthetic (held audio route, stubbed play/pause, fixed
 * Math.random, fixed radio seed), not evidence of real decoding or streaming.
 * Only HTMLMediaElement/Math/storage I/O is faked; the real key handler,
 * queue, repeat, radio, guard and dialog code runs.
 *
 * Baseline RED is a failing transport assertion AFTER the queue fixture
 * preconditions (order/value/row), never a navigation/build/import failure.
 * Guard no-ops open behind a capability gate (`s` shuffles, `r` cycles) so the
 * suite is RED pre-implementation for the asserted reason. Deliberately
 * unpinned: shuffle completion announcements (button-component-local), `r`
 * with the select focused (native type-ahead interplay), IME composition
 * events (sibling guard-semantics lane), and the `0` RESTART lane. Stable IDs
 * SHUFREP-Bxx map to the handoff report. No standalone red-CI PR.
 */

type Probe = {
	playCalls: number;
};

declare global {
	interface Window {
		__shufrepProbe: Probe;
	}
}

const INITIAL = ['demo-track', 'second-signal', 'third-light'];
const SWAPPED = ['demo-track', 'third-light', 'second-signal'];

async function openPlayer(page: Page): Promise<void> {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		window.__shufrepProbe = { playCalls: 0 };
		// Only the external media boundary is doubled; the real key handler,
		// queue, repeat, radio, guard and dialog code runs unchanged.
		HTMLMediaElement.prototype.play = function () {
			window.__shufrepProbe.playCalls += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
		// Deterministic shuffle: Fisher-Yates over two upcoming entries picks
		// index 0, so one `s` reverses the upcoming pair and the next restores
		// it. Radio uses its own seeded PRNG and is unaffected.
		Math.random = () => 0;
	});
	await page.goto('/demo/player?radio-seed=7');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-track-id')));
}

async function playCalls(page: Page): Promise<number> {
	return page.evaluate(() => window.__shufrepProbe.playCalls);
}

/** Blur whatever hydrated with focus so no player control holds it. */
async function focusBody(page: Page): Promise<void> {
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	expect(await page.evaluate(() => document.activeElement === document.body)).toBe(true);
}

/** Two rendering frames let handlers, promises and Svelte effects settle. */
async function settle(page: Page): Promise<void> {
	await page.evaluate(
		() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())))
	);
}

/** Natural end of the last queued track with repeat Off starts a radio pass. */
async function startRadio(page: Page): Promise<void> {
	await page.getByTestId('queue-select-third-light').click();
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
	await expect(page.getByTestId('radio-badge')).toBeVisible();
}

test('SHUFREP-B01 s shuffles upcoming only: current fixed, transport untouched', async ({
	page
}) => {
	await openPlayer(page);
	await focusBody(page);
	const requests: string[] = [];
	page.on('request', (request) => requests.push(request.url()));

	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(SWAPPED);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: Off');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	expect(await playCalls(page)).toBe(0);

	// Uppercase S takes the same path and swaps the pair back.
	await page.keyboard.press('S');
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	expect(await playCalls(page)).toBe(0);
	expect(requests).toEqual([]);
});

for (const state of ['middle', 'last', 'one-track', 'empty'] as const) {
	test(`SHUFREP-B02 ${state}: s no-ops with fewer than two upcoming`, async ({ page }) => {
		await openPlayer(page);
		await focusBody(page);
		// Capability gate (RED pre-implementation): s must shuffle the full queue first.
		await page.keyboard.press('s');
		await expect.poll(() => queueOrder(page)).toEqual(SWAPPED);
		await page.keyboard.press('s');
		await expect.poll(() => queueOrder(page)).toEqual(INITIAL);

		if (state === 'middle' || state === 'last') {
			const id = state === 'middle' ? 'second-signal' : 'third-light';
			await page.getByTestId(`queue-select-${id}`).click();
			await expect(page.getByTestId('now-playing')).toContainText(
				state === 'middle' ? 'Second Signal' : 'Third Light'
			);
		} else {
			await page.getByTestId('queue-clear').click();
			if (state === 'one-track') await page.getByTestId('queue-add-second-signal').click();
			await expect(page.getByTestId('queue-item')).toHaveCount(state === 'one-track' ? 1 : 0);
		}
		const order = await queueOrder(page);
		const status = await page.getByTestId('player-status').textContent();
		const plays = await playCalls(page);

		await page.keyboard.press('s');
		await settle(page);

		expect(await queueOrder(page)).toEqual(order);
		await expect(page.getByTestId('player-status')).toHaveText(status!);
		expect(await playCalls(page)).toBe(plays);
		await expect(page.getByTestId('radio-badge')).toHaveCount(0);
		await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	});
}

test('SHUFREP-B03 s is inert while radio is active and recovers after stop', async ({
	page
}) => {
	await openPlayer(page);
	await focusBody(page);
	// Capability gate (RED pre-implementation): s must shuffle first.
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(SWAPPED);
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);

	await startRadio(page);
	expect(await queueOrder(page)).toEqual(INITIAL);
	const plays = await playCalls(page);

	await page.keyboard.press('s');
	await settle(page);

	expect(await queueOrder(page)).toEqual(INITIAL);
	expect(await playCalls(page)).toBe(plays);
	await expect(page.getByTestId('radio-badge')).toBeVisible();

	await page.getByTestId('radio-stop').click();
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await focusBody(page);
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(SWAPPED);
});

test('SHUFREP-B04 r cycles repeat off-one-queue-off through the select state', async ({
	page
}) => {
	await openPlayer(page);
	await focusBody(page);
	const control = page.getByTestId('player-repeat');
	const status = page.getByTestId('repeat-status');
	await expect(control).toHaveValue('off');

	for (const [value, text] of [
		['one', 'Repeat: One'],
		['queue', 'Repeat: Queue'],
		['off', 'Repeat: Off']
	] as const) {
		await page.keyboard.press('r');
		await expect(control).toHaveValue(value);
		await expect(status).toHaveText(text);
		expect(await queueOrder(page)).toEqual(INITIAL);
		expect(await page.evaluate(() => document.activeElement === document.body)).toBe(true);
	}
	expect(await playCalls(page)).toBe(0);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);

	// Uppercase R takes the same path.
	await page.keyboard.press('R');
	await expect(control).toHaveValue('one');
	await expect(status).toHaveText('Repeat: One');
});

test('SHUFREP-B05 r is inert while radio disables repeat and recovers after stop', async ({
	page
}) => {
	await openPlayer(page);
	await focusBody(page);
	// Capability gate (RED pre-implementation) plus a return to Off: radio
	// only starts from the Off completion path.
	await page.keyboard.press('r');
	await expect(page.getByTestId('player-repeat')).toHaveValue('one');
	await page.keyboard.press('r');
	await page.keyboard.press('r');
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');

	await startRadio(page);
	const control = page.getByTestId('player-repeat');
	await expect(control).toBeDisabled();
	await expect(control).toHaveAccessibleDescription(/radio/i);
	const plays = await playCalls(page);

	await page.keyboard.press('r');
	await settle(page);

	await expect(control).toHaveValue('off');
	await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: Off');
	expect(await playCalls(page)).toBe(plays);

	await page.getByTestId('radio-stop').click();
	await expect(control).toBeEnabled();
	await page.keyboard.press('r');
	await expect(control).toHaveValue('one');
	await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: One');
});

test('SHUFREP-B06 s and r yield to typing surfaces, modals, chords and the open dialog', async ({
	page
}) => {
	await openPlayer(page);
	await focusBody(page);
	// Capability gates (RED pre-implementation), then restore the baseline.
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(SWAPPED);
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
	await page.keyboard.press('r');
	await expect(page.getByTestId('player-repeat')).toHaveValue('one');
	await page.keyboard.press('r');
	await page.keyboard.press('r');
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');

	// Typing surfaces keep the keystrokes; transport stays untouched.
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
		for (const key of ['s', 'r', 'S', 'R']) await page.keyboard.press(key);
		if (testid === 'probe-editable') {
			await expect(field).toHaveText('srSR');
		} else {
			await expect(field).toHaveValue('srSR');
		}
		expect(await queueOrder(page)).toEqual(INITIAL);
		await expect(page.getByTestId('player-repeat')).toHaveValue('off');
		await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
		expect(await playCalls(page)).toBe(0);
		await expect(page.getByTestId('shortcuts-dialog')).toHaveCount(0);
	}

	// An open modal captures the keys; the probe itself is undisturbed.
	await page.evaluate(() => {
		const dialog = document.createElement('dialog');
		dialog.setAttribute('data-testid', 'probe-dialog');
		dialog.innerHTML = '<button data-testid="probe-dialog-ok">OK</button>';
		document.querySelector('main')?.append(dialog);
		dialog.showModal();
		dialog.querySelector('button')?.focus();
	});
	await expect(page.getByTestId('probe-dialog')).toBeVisible();
	for (const key of ['s', 'r', 'S', 'R']) await page.keyboard.press(key);
	await settle(page);
	expect(await queueOrder(page)).toEqual(INITIAL);
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	await expect(page.getByTestId('shortcuts-dialog')).toHaveCount(0);
	const probe = await page.evaluate(() => {
		const dialog = document.querySelector<HTMLDialogElement>('[data-testid="probe-dialog"]');
		return { open: dialog?.open ?? false, inside: dialog?.contains(document.activeElement) ?? false };
	});
	expect(probe).toEqual({ open: true, inside: true });

	// Modified chords never touch transport (dispatched: real Ctrl/Alt chords
	// risk browser chrome accelerators; the page sees the keydown either way).
	await page.evaluate(() => {
		document.querySelector<HTMLDialogElement>('[data-testid="probe-dialog"]')?.close();
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
		for (const init of [
			{ key: 's', ctrlKey: true },
			{ key: 'r', altKey: true },
			{ key: 's', metaKey: true },
			{ key: 'r', ctrlKey: true }
		] as KeyboardEventInit[]) {
			window.dispatchEvent(new KeyboardEvent('keydown', { ...init, bubbles: true }));
		}
	});
	await settle(page);
	expect(await queueOrder(page)).toEqual(INITIAL);
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	expect(await playCalls(page)).toBe(0);

	// The open shortcuts dialog keeps the keys for itself.
	await page.keyboard.press('?');
	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(dialog).toBeVisible();
	await page.keyboard.press('s');
	await page.keyboard.press('r');
	await settle(page);
	expect(await queueOrder(page)).toEqual(INITIAL);
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	await expect(dialog).toHaveCount(1);
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
});

test('SHUFREP-B07 shortcuts dialog lists the s and r rows; ? and Escape unchanged', async ({
	page
}) => {
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await focusBody(page);

	await page.keyboard.press('?');
	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(dialog).toBeVisible();
	await expect(dialog).toHaveRole('dialog');
	expect(await dialog.locator('kbd').count()).toBeGreaterThanOrEqual(9);
	// Exact glyphs: the Escape row contains an `s`, so substring matching
	// would pass vacuously pre-implementation.
	for (const key of [/^s$/, /^r$/]) {
		expect(await dialog.locator('kbd', { hasText: key }).count()).toBeGreaterThanOrEqual(1);
	}
	for (const key of ['k', 'n', 'p', 'j', 'l', 'm', '?']) {
		expect(await dialog.locator('kbd', { hasText: key }).count()).toBeGreaterThanOrEqual(1);
	}
	for (const label of [/shuffle/i, /repeat/i]) {
		await expect(dialog).toContainText(label);
	}

	// Close and reopen through the existing paths.
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	const button = page.getByTestId('shortcuts-button');
	await button.click();
	await expect(dialog).toBeVisible();
	await page.getByTestId('shortcuts-close').click();
	await expect(dialog).toHaveCount(0);
});

test('SHUFREP-B08 s and r change only their own state: no transport side effects', async ({
	page
}) => {
	await openPlayer(page);
	await focusBody(page);
	const audio = page.getByTestId('demo-audio');
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');

	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(SWAPPED);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	expect(await audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(false);
	expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(0);
	expect(await playCalls(page)).toBe(0);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: Off');

	for (const [value, text] of [
		['one', 'Repeat: One'],
		['queue', 'Repeat: Queue'],
		['off', 'Repeat: Off']
	] as const) {
		await page.keyboard.press('r');
		await expect(page.getByTestId('player-repeat')).toHaveValue(value);
		await expect(page.getByTestId('repeat-status')).toHaveText(text);
		expect(await queueOrder(page)).toEqual(SWAPPED);
		await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
		await expect(page.getByTestId('now-playing')).toContainText('Demo track');
		expect(await audio.evaluate((el: HTMLAudioElement) => el.muted)).toBe(false);
		expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(0);
		expect(await playCalls(page)).toBe(0);
	}

	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
});
