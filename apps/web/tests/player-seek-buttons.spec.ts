import { expect, test, type ElementHandle, type Page } from '@playwright/test';

/**
 * Visible 10-second jump controls over `/demo/player` — acceptance pins.
 *
 * PROTECTED ACCEPTANCE SPEC: the paired builder implements the buttons on
 * this branch but must not edit this file. If a pin conflicts with the
 * merged sources, the builder changes the implementation, not the pin.
 *
 * Contract the build implements (stable case IDs JUMP-01..JUMP-15):
 *
 * Hooks (literal `data-testid`, same convention as the existing player
 * transport hooks — the e2e TESTIDS map does not cover this route):
 *   player-seek-back ...... native <button>, accessible name matching /back 10/i
 *   player-seek-forward ... native <button>, accessible name matching /forward 10/i
 * Both are distinct from Previous/Next track, keyboard-focusable with a
 * visible focus indicator, and usable at 360px width. Native audio
 * controls and every existing hook stay intact.
 *
 * Behavior: each activation moves the CURRENT matching audio element by
 * exactly 10 seconds (SEEK_SECONDS; the constant itself is pinned by
 * src/lib/player/shortcuts.test.ts), clamped to finite duration and
 * zero. No current track, non-finite/negative current time or
 * non-finite duration disables the control or refuses the activation —
 * never a throw through the UI, never fabricated success. Seeks
 * preserve paused/playing intent, playback rate, queue selection and
 * order, and sleep/radio state; they never call play/load, never change
 * source, never advance tracks, never trigger ended handling. Global
 * key semantics and typing/modal guards are unchanged, and no pin
 * depends on Media Session seek or elapsed-time display work.
 *
 * Determinism: every test holds the demo-audio route and stubs
 * play/pause/load (same pending-media harness as the shortcuts suite),
 * then doubles `duration`/`currentTime` per element with
 * `Object.defineProperty` — no real decoder, no sleeps, no network
 * media. CTRL-01/CTRL-02 are controls that pass with and without the
 * feature; they prove the harness, not the feature.
 *
 * RED expectations: without the buttons, JUMP-01..JUMP-14 fail on an
 * assertion naming the missing hook and its next action, while both
 * CTRL controls stay green. JUMP-15 (unchanged key semantics) is green
 * pre-implementation by design — a guard pin, like the shortcuts
 * suite's typing/modal guards — and must stay green after the build.
 * Mutation check: break any single pin (wrong delta, missing clamp,
 * extra play() call, lost disabled state) and exactly its test fails.
 */

type MediaControl = {
	playCalls: number;
	loadCalls: number;
	resolve: () => void;
	reject: () => void;
};

type Calls = { playCalls: number; loadCalls: number };

/** Demo player with synthetic pending media: only fixture events settle playback. */
async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		const control: MediaControl = {
			playCalls: 0,
			loadCalls: 0,
			resolve: () => {},
			reject: () => {}
		};
		Object.assign(window, { __jumpLifecycle: control });
		HTMLMediaElement.prototype.play = function (): Promise<void> {
			control.playCalls += 1;
			return new Promise<void>((resolve, reject) => {
				control.resolve = resolve;
				control.reject = () => reject(new Error('private decoder details'));
			});
		};
		HTMLMediaElement.prototype.pause = function () {};
		const rawLoad = HTMLMediaElement.prototype.load;
		HTMLMediaElement.prototype.load = function () {
			control.loadCalls += 1;
			return rawLoad.call(this);
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function lifecycleOf(page: Page): Promise<Calls> {
	return page.evaluate(() => {
		const control = (window as unknown as { __jumpLifecycle: MediaControl }).__jumpLifecycle;
		return { playCalls: control.playCalls, loadCalls: control.loadCalls };
	});
}

async function settle(page: Page, mode: 'resolve' | 'reject') {
	await page.evaluate((next) => {
		const control = (window as unknown as { __jumpLifecycle: MediaControl }).__jumpLifecycle;
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

/**
 * Deterministic media double on the CURRENT audio element: finite or
 * sentinel duration plus a writable currentTime. Re-apply after any
 * source replacement (track change, retry) recreates the element.
 */
async function mockMedia(page: Page, init: { currentTime: number; duration: number }) {
	await page.getByTestId('demo-audio').evaluate(
		(el: HTMLAudioElement, seed: { currentTime: number; duration: number }) => {
			let time = seed.currentTime;
			Object.defineProperty(el, 'duration', { value: seed.duration, configurable: true });
			Object.defineProperty(el, 'currentTime', {
				get: () => time,
				set: (next: number) => {
					time = next;
				},
				configurable: true
			});
		},
		init
	);
}

async function mediaTime(page: Page): Promise<number> {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime);
}

/**
 * Presence gate for the new hooks: 'missing' must fail the test with an
 * assertion naming the hook (assertion-RED, never an import/compile
 * failure). Never click a disabled button — Playwright would time out
 * waiting for it to enable instead of testing the no-op path.
 */
async function hookState(page: Page, testid: string): Promise<'missing' | 'disabled' | 'enabled'> {
	const button = page.getByTestId(testid);
	if ((await button.count()) === 0) return 'missing';
	return (await button.isDisabled()) ? 'disabled' : 'enabled';
}

async function requireHook(page: Page, caseId: string, testid: string, label: string) {
	const state = await hookState(page, testid);
	expect(
		state,
		`${caseId}: expected a visible ${label} button (data-testid="${testid}"), found ${state}. ` +
			`Next action: render the button on the demo player transport bar.`
	).not.toBe('missing');
	return state;
}

/** Flush one frame so a refused activation (or a leaked error) can land. */
async function flushFrame(page: Page) {
	await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => done(null))));
}

async function queueOrder(page: Page): Promise<string[]> {
	return page.getByTestId('queue-item').allTextContents();
}

/** Drive the player to paused intent through the real k-toggle transport. */
async function reachPaused(page: Page) {
	await focusBody(page);
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await expect.poll(() => lifecycleOf(page).then((calls) => calls.playCalls)).toBe(1);
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
}

test('CTRL-01 existing hooks and native controls are intact without the feature', async ({
	page
}) => {
	await openPlayer(page);
	const audio = page.getByTestId('demo-audio');
	await expect(audio).toHaveAttribute('src', '/api/demo/audio');
	expect(await audio.evaluate((el: HTMLAudioElement) => el.hasAttribute('controls'))).toBe(true);
	await expect(page.getByTestId('player-prev')).toBeVisible();
	await expect(page.getByTestId('player-next')).toBeVisible();
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
});

test('CTRL-02 the media double reads and writes deterministically', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await expect.poll(() => mediaTime(page)).toBe(30);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 42.5;
	});
	await expect.poll(() => mediaTime(page)).toBe(42.5);
});

test('JUMP-01 back/forward buttons are labeled natives distinct from track buttons', async ({
	page
}) => {
	await openPlayer(page);
	await requireHook(page, 'JUMP-01', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-01', 'player-seek-forward', 'Forward 10 seconds');

	const back = page.getByTestId('player-seek-back');
	const forward = page.getByTestId('player-seek-forward');
	await expect(back).toHaveAccessibleName(/back 10/i);
	await expect(forward).toHaveAccessibleName(/forward 10/i);
	expect(await back.evaluate((el) => el.tagName)).toBe('BUTTON');
	expect(await forward.evaluate((el) => el.tagName)).toBe('BUTTON');

	// Distinct from Previous/Next track: neither name may collide.
	await expect(page.getByTestId('player-prev')).toHaveAccessibleName(/previous/i);
	await expect(page.getByTestId('player-next')).toHaveAccessibleName(/^next$/i);
	await expect(back).not.toHaveAccessibleName(/previous|^next$/i);
	await expect(forward).not.toHaveAccessibleName(/previous|^next$/i);

	// Keyboard-focusable, with native controls and hooks untouched.
	await back.focus();
	await expect(back).toBeFocused();
	await forward.focus();
	await expect(forward).toBeFocused();
	const audio = page.getByTestId('demo-audio');
	expect(await audio.evaluate((el: HTMLAudioElement) => el.hasAttribute('controls'))).toBe(true);
});

test('JUMP-02 narrow layout keeps both buttons visible, disjoint and keyboard-reachable', async ({
	page
}) => {
	await page.setViewportSize({ width: 360, height: 800 });
	await openPlayer(page);
	await requireHook(page, 'JUMP-02', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-02', 'player-seek-forward', 'Forward 10 seconds');

	const back = page.getByTestId('player-seek-back');
	const forward = page.getByTestId('player-seek-forward');
	await expect(back).toBeVisible();
	await expect(forward).toBeVisible();

	const backBox = await back.boundingBox();
	const forwardBox = await forward.boundingBox();
	expect(backBox, 'JUMP-02: back button has no layout box at 360px').not.toBeNull();
	expect(forwardBox, 'JUMP-02: forward button has no layout box at 360px').not.toBeNull();
	if (!backBox || !forwardBox) return;
	for (const [name, box] of [
		['back', backBox],
		['forward', forwardBox]
	] as const) {
		expect(box.x, `JUMP-02: ${name} button overflows the left edge at 360px`).toBeGreaterThanOrEqual(0);
		expect(
			box.x + box.width,
			`JUMP-02: ${name} button overflows the 360px viewport (x=${box.x}, w=${box.width}). ` +
				'Next action: let the transport bar wrap or shrink its controls.'
		).toBeLessThanOrEqual(360);
	}
	const disjoint =
		backBox.x + backBox.width <= forwardBox.x || forwardBox.x + forwardBox.width <= backBox.x;
	expect(
		disjoint,
		'JUMP-02: the two jump buttons overlap at 360px. Next action: keep them in separate layout boxes.'
	).toBe(true);

	// Real Tab presses (bounded, no sleeps) prove keyboard reachability; the
	// focus ring is read while :focus-visible genuinely matches.
	await focusBody(page);
	let focused: string | null = null;
	for (let tab = 0; tab < 40; tab += 1) {
		await page.keyboard.press('Tab');
		focused = await page.evaluate(
			() => (document.activeElement as HTMLElement | null)?.dataset?.testid ?? null
		);
		if (focused === 'player-seek-back' || focused === 'player-seek-forward') break;
	}
	expect(
		focused,
		'JUMP-02: Tab never reached a jump button within 40 stops. Next action: keep the buttons in tab order.'
	).toMatch(/^player-seek-(back|forward)$/);
	const ring = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		if (!el) return { outline: 'none', shadow: 'none' };
		const style = getComputedStyle(el);
		return { outline: style.outlineStyle, shadow: style.boxShadow };
	});
	expect(
		ring.outline !== 'none' || ring.shadow !== 'none',
		`JUMP-02: focused jump button shows no focus indicator (outline=${ring.outline}, shadow=${ring.shadow}). ` +
			'Next action: keep a visible :focus-visible ring.'
	).toBe(true);
});

test('JUMP-03 pointer activates Back 10 seconds under paused intent without extra calls', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await reachPaused(page);
	await requireHook(page, 'JUMP-03', 'player-seek-back', 'Back 10 seconds');
	const before = await lifecycleOf(page);
	const order = await queueOrder(page);

	await page.getByTestId('player-seek-back').click();

	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(20);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	expect(await lifecycleOf(page)).toEqual(before);
	expect(await queueOrder(page)).toEqual(order);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1');
});

test('JUMP-04 pointer activates Forward 10 seconds under paused intent without extra calls', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await reachPaused(page);
	await requireHook(page, 'JUMP-04', 'player-seek-forward', 'Forward 10 seconds');
	const before = await lifecycleOf(page);

	await page.getByTestId('player-seek-forward').click();

	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(40);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	expect(await lifecycleOf(page)).toEqual(before);
});

test('JUMP-05 fractional positions keep their fraction across both directions', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30.5, duration: 120 });
	await requireHook(page, 'JUMP-05', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-05', 'player-seek-forward', 'Forward 10 seconds');
	const before = await lifecycleOf(page);

	await page.getByTestId('player-seek-back').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBeCloseTo(20.5, 5);

	await page.getByTestId('player-seek-forward').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBeCloseTo(30.5, 5);

	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
});

test('JUMP-06 seeks clamp to zero and finite duration, then no-op at the bounds', async ({
	page
}) => {
	await openPlayer(page);
	await requireHook(page, 'JUMP-06', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-06', 'player-seek-forward', 'Forward 10 seconds');

	// Start bound: 3s back lands on 0, and a second Back at 0 is a no-op
	// (disabled there counts as a no-op too).
	await mockMedia(page, { currentTime: 3, duration: 120 });
	const back = page.getByTestId('player-seek-back');
	if (await back.isDisabled()) {
		expect(await mediaTime(page)).toBe(3);
	} else {
		await back.click();
		await expect
			.poll(() => mediaTime(page), { timeout: 5_000 })
			.toBe(0);
	}
	const atZero = await hookState(page, 'player-seek-back');
	if (atZero === 'enabled') {
		const before = await lifecycleOf(page);
		await back.click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBe(0);
		expect(await lifecycleOf(page)).toEqual(before);
	}

	// End bound: 115s forward lands on the 120s duration, then no-ops.
	await mockMedia(page, { currentTime: 115, duration: 120 });
	const forward = page.getByTestId('player-seek-forward');
	if (await forward.isDisabled()) {
		expect(await mediaTime(page)).toBe(115);
	} else {
		await forward.click();
		await expect
			.poll(() => mediaTime(page), { timeout: 5_000 })
			.toBe(120);
	}
	const atEnd = await hookState(page, 'player-seek-forward');
	if (atEnd === 'enabled') {
		const before = await lifecycleOf(page);
		await forward.click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBe(120);
		expect(await lifecycleOf(page)).toEqual(before);
	}
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
});

test('JUMP-07 Enter and Space activate both buttons under playing intent', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await focusBody(page);
	await page.keyboard.press('k');
	await expect.poll(() => lifecycleOf(page).then((calls) => calls.playCalls)).toBe(1);
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await requireHook(page, 'JUMP-07', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-07', 'player-seek-forward', 'Forward 10 seconds');
	const before = await lifecycleOf(page);

	await page.getByTestId('player-seek-back').focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(20);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await page.getByTestId('player-seek-forward').focus();
	await page.keyboard.press('Space');
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(30);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('JUMP-08 Enter and Space activate both buttons under paused intent', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 60, duration: 120 });
	await reachPaused(page);
	await requireHook(page, 'JUMP-08', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-08', 'player-seek-forward', 'Forward 10 seconds');
	const before = await lifecycleOf(page);

	await page.getByTestId('player-seek-back').focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(50);

	await page.getByTestId('player-seek-forward').focus();
	await page.keyboard.press('Space');
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(60);

	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	expect(await lifecycleOf(page)).toEqual(before);
});

test('JUMP-09 an empty queue disables the buttons or refuses the activation', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	const backState = await requireHook(page, 'JUMP-09', 'player-seek-back', 'Back 10 seconds');
	const forwardState = await requireHook(
		page,
		'JUMP-09',
		'player-seek-forward',
		'Forward 10 seconds'
	);

	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const before = await lifecycleOf(page);
	for (const [testid, state] of [
		['player-seek-back', backState],
		['player-seek-forward', forwardState]
	] as const) {
		if (state === 'enabled') {
			// Enabled with no current track must still refuse: no throw, no
			// transport calls, no track, no fabricated success.
			await page.getByTestId(testid).click();
			await flushFrame(page);
		}
	}
	expect(errors).toEqual([]);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('JUMP-10 unusable duration or time refuses, and known duration seeks', async ({ page }) => {
	await openPlayer(page);
	await requireHook(page, 'JUMP-10', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-10', 'player-seek-forward', 'Forward 10 seconds');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));

	// Unknown duration: disabled, or an enabled click that leaves time put.
	await mockMedia(page, { currentTime: 30, duration: Number.NaN });
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('loadedmetadata'));
		el.dispatchEvent(new Event('timeupdate'));
	});
	await flushFrame(page);
	if ((await hookState(page, 'player-seek-forward')) === 'enabled') {
		await page.getByTestId('player-seek-forward').click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBe(30);
	}

	// Infinite (live-edge) duration is not a usable finite bound either.
	await mockMedia(page, { currentTime: 30, duration: Number.POSITIVE_INFINITY });
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('loadedmetadata'));
	});
	await flushFrame(page);
	if ((await hookState(page, 'player-seek-back')) === 'enabled') {
		await page.getByTestId('player-seek-back').click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBe(30);
	}

	// Non-finite current time refuses the same way.
	await mockMedia(page, { currentTime: Number.NaN, duration: 120 });
	await flushFrame(page);
	if ((await hookState(page, 'player-seek-forward')) === 'enabled') {
		await page.getByTestId('player-seek-forward').click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBeNaN();
	}

	// Duration becoming known enables real seeking on the same element.
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('loadedmetadata'));
	});
	const known = await hookState(page, 'player-seek-forward');
	expect(
		known,
		'JUMP-10: expected the forward button to enable once duration is known, found ' +
			`${known}. Next action: refresh availability on loadedmetadata.`
	).toBe('enabled');
	await page.getByTestId('player-seek-forward').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(40);

	expect(errors).toEqual([]);
});

test('JUMP-11 a rejecting seek setter is caught without throwing through the UI', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { value: 120, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => 30,
			set: () => {
				throw new DOMException('seek refused by media', 'InvalidStateError');
			},
			configurable: true
		});
	});
	await requireHook(page, 'JUMP-11', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-11', 'player-seek-forward', 'Forward 10 seconds');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const before = await lifecycleOf(page);

	// Usable-looking media stays enabled: the refusal only surfaces on write.
	await expect(page.getByTestId('player-seek-back')).toBeEnabled();
	await expect(page.getByTestId('player-seek-forward')).toBeEnabled();
	await page.getByTestId('player-seek-back').click();
	await flushFrame(page);
	await page.getByTestId('player-seek-forward').click();
	await flushFrame(page);

	expect(await mediaTime(page)).toBe(30);
	expect(errors).toEqual([]);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('JUMP-12 seeks follow the current element across source replacement, stale events ignored', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await requireHook(page, 'JUMP-12', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-12', 'player-seek-forward', 'Forward 10 seconds');
	const stale = (await page
		.getByTestId('demo-audio')
		.elementHandle()) as ElementHandle<HTMLAudioElement> | null;
	expect(stale, 'JUMP-12: expected a demo audio element before navigating').not.toBeNull();
	if (!stale) return;

	await stale.evaluate((el: HTMLAudioElement) => {
		(el as unknown as { __jumpMarker?: boolean }).__jumpMarker = true;
	});

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	const replaced = await page
		.getByTestId('demo-audio')
		.evaluate(
			(el: HTMLAudioElement) =>
				(el as unknown as { __jumpMarker?: boolean }).__jumpMarker ?? false
		);
	expect(
		replaced,
		'JUMP-12: expected track navigation to swap in a fresh audio element.'
	).toBe(false);

	// The replacement element seeks from ITS position, not the old one.
	await mockMedia(page, { currentTime: 50, duration: 200 });
	const before = await lifecycleOf(page);
	await page.getByTestId('player-seek-back').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(40);
	expect(
		await stale.evaluate((el: HTMLAudioElement) => el.currentTime),
		'JUMP-12: the detached element moved — seeks must target the current one.'
	).toBe(30);

	// Stale events from the detached element cannot disable or redirect.
	await stale.evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { value: Number.NaN, configurable: true });
		el.dispatchEvent(new Event('loadedmetadata'));
		el.dispatchEvent(new Event('error'));
	});
	await flushFrame(page);
	await expect(page.getByTestId('player-seek-forward')).toBeEnabled();
	await page.getByTestId('player-seek-forward').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(50);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
});

test('JUMP-13 seeks work on the replacement element after error and retry', async ({ page }) => {
	await openPlayer(page);
	await focusBody(page);
	await page.keyboard.press('k');
	await expect.poll(() => lifecycleOf(page).then((calls) => calls.playCalls)).toBe(1);
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await fireMediaEvent(page, 'error');
	await expect(page.getByTestId('player-error')).toBeVisible();
	await page.getByTestId('player-retry').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	// The retry swapped in a fresh element: seek from its doubled position.
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await requireHook(page, 'JUMP-13', 'player-seek-back', 'Back 10 seconds');
	const before = await lifecycleOf(page);
	await page.getByTestId('player-seek-back').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(20);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('JUMP-14 seeks preserve rate, queue, sleep and status without ended behavior', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('playback-speed').selectOption('1.5');
	await page.getByTestId('sleep-timer-set-duration').click();
	await expect(page.getByTestId('sleep-timer-status')).toHaveText('Sleep in 15 minutes');
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await requireHook(page, 'JUMP-14', 'player-seek-back', 'Back 10 seconds');
	await requireHook(page, 'JUMP-14', 'player-seek-forward', 'Forward 10 seconds');
	const before = await lifecycleOf(page);
	const order = await queueOrder(page);

	await page.getByTestId('player-seek-back').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(20);
	await page.getByTestId('player-seek-forward').click();
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(30);

	expect(await lifecycleOf(page)).toEqual(before);
	expect(await queueOrder(page)).toEqual(order);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.5');
	expect(await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate)).toBe(
		1.5
	);
	await expect(page.getByTestId('sleep-timer-status')).toHaveText('Sleep in 15 minutes');
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
});

test('JUMP-15 global key semantics and typing guards are unchanged', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await focusBody(page);

	// j/l still move by exactly 10 seconds through the mocked media.
	await page.keyboard.press('l');
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(40);
	await page.keyboard.press('j');
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(30);

	// Typing surfaces still swallow transport keys.
	await page.getByTestId('sleep-timer-minutes').focus();
	await page.keyboard.press('j');
	await page.keyboard.press('l');
	await flushFrame(page);
	expect(await mediaTime(page)).toBe(30);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');

	// Mute still toggles, and play still routes through the pending stub.
	await focusBody(page);
	await page.keyboard.press('m');
	await expect
		.poll(() => page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.muted))
		.toBe(true);
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await expect.poll(() => lifecycleOf(page).then((calls) => calls.playCalls)).toBe(1);
});
