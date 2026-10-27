import { expect, test, type ElementHandle, type Page } from '@playwright/test';

/**
 * Restart-the-current-track control over `/demo/player` — acceptance pins.
 *
 * PROTECTED ACCEPTANCE SPEC: the paired builder implements the restart
 * control on this branch but must not edit this file. If a pin conflicts
 * with the merged sources, the builder changes the implementation, not
 * the pin.
 *
 * Contract the build implements (stable case IDs RESTART-01..RESTART-16):
 *
 * Hook (literal `data-testid`, same convention as the existing player
 * transport hooks — the e2e TESTIDS map does not cover this route):
 *   player-restart ........ native <button>, accessible name matching
 *                            /restart track/i ("Restart track")
 * It is distinct from Previous/Next track, keyboard-focusable with a
 * visible focus indicator, and usable at 360px width. The `0` shortcut
 * reuses the existing guarded shortcut path and is listed in the
 * shortcuts dialog. Native audio controls and every existing hook stay
 * intact.
 *
 * Behavior: each activation seeks the CURRENT matching audio element to
 * exactly 0 through the existing validated safe-seek path (finite
 * position and duration, caught setter refusal). No current track or
 * unseekable media disables the control or refuses the activation —
 * never a throw through the UI, never fabricated success. Restarts
 * preserve paused/playing intent and playback rate; they never advance
 * tracks, never call play/load, and never trigger ended, radio, sleep
 * or recently-played accounting. The seek target is computed from the
 * current element at activation: detached/stale events cannot re-seek a
 * replacement source.
 *
 * Determinism: every test holds the demo-audio route and stubs
 * play/pause/load (same pending-media harness as the jump suite), then
 * doubles `duration`/`currentTime` per element with
 * `Object.defineProperty` — no real decoder, no sleeps, no network
 * media. CTRL-01/CTRL-02 are controls that pass with and without the
 * feature; they prove the harness, not the feature.
 *
 * RED expectations: without the control, RESTART-01..RESTART-15 fail on
 * an assertion naming the missing hook (or the missing `0` row) and its
 * next action, while both CTRL controls stay green. RESTART-16
 * (unchanged key semantics) is green pre-implementation by design — a
 * guard pin, like the jump suite's — and must stay green after the
 * build. Mutation check: break any single pin (wrong target, missing
 * clamp, extra play() call, lost disabled state, unguarded `0`) and
 * exactly its test fails.
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
		Object.assign(window, { __restartLifecycle: control });
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
		const control = (window as unknown as { __restartLifecycle: MediaControl }).__restartLifecycle;
		return { playCalls: control.playCalls, loadCalls: control.loadCalls };
	});
}

async function settle(page: Page, mode: 'resolve' | 'reject') {
	await page.evaluate((next) => {
		const control = (window as unknown as { __restartLifecycle: MediaControl }).__restartLifecycle;
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
 * Presence gate for the new hook: 'missing' must fail the test with an
 * assertion naming the hook (assertion-RED, never an import/compile
 * failure). Never click a disabled button — Playwright would time out
 * waiting for it to enable instead of testing the no-op path.
 */
async function hookState(page: Page): Promise<'missing' | 'disabled' | 'enabled'> {
	const button = page.getByTestId('player-restart');
	if ((await button.count()) === 0) return 'missing';
	return (await button.isDisabled()) ? 'disabled' : 'enabled';
}

async function requireHook(page: Page, caseId: string) {
	const state = await hookState(page);
	expect(
		state,
		`${caseId}: expected a visible Restart track button (data-testid="player-restart"), found ${state}. ` +
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
	await expect(page.getByTestId('shortcuts-button')).toBeVisible();
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

test('RESTART-01 the restart button is a labeled native distinct from track buttons', async ({
	page
}) => {
	await openPlayer(page);
	await requireHook(page, 'RESTART-01');

	const restart = page.getByTestId('player-restart');
	await expect(restart).toHaveAccessibleName(/restart track/i);
	expect(await restart.evaluate((el) => el.tagName)).toBe('BUTTON');

	// Distinct from Previous/Next track: its name may not collide.
	await expect(page.getByTestId('player-prev')).toHaveAccessibleName(/previous/i);
	await expect(page.getByTestId('player-next')).toHaveAccessibleName(/^next$/i);
	await expect(restart).not.toHaveAccessibleName(/previous|^next$/i);

	// Keyboard-focusable, with native controls and hooks untouched.
	await restart.focus();
	await expect(restart).toBeFocused();
	const audio = page.getByTestId('demo-audio');
	expect(await audio.evaluate((el: HTMLAudioElement) => el.hasAttribute('controls'))).toBe(true);
});

test('RESTART-02 narrow layout keeps the button visible and keyboard-reachable', async ({
	page
}) => {
	await page.setViewportSize({ width: 360, height: 800 });
	await openPlayer(page);
	await requireHook(page, 'RESTART-02');

	const restart = page.getByTestId('player-restart');
	await expect(restart).toBeVisible();

	const box = await restart.boundingBox();
	expect(box, 'RESTART-02: restart button has no layout box at 360px').not.toBeNull();
	if (!box) return;
	expect(box.x, 'RESTART-02: restart button overflows the left edge at 360px').toBeGreaterThanOrEqual(
		0
	);
	expect(
		box.x + box.width,
		`RESTART-02: restart button overflows the 360px viewport (x=${box.x}, w=${box.width}). ` +
			'Next action: let the transport bar wrap or shrink its controls.'
	).toBeLessThanOrEqual(360);

	// Real Tab presses (bounded, no sleeps) prove keyboard reachability; the
	// focus ring is read while :focus-visible genuinely matches.
	await focusBody(page);
	let focused: string | null = null;
	for (let tab = 0; tab < 40; tab += 1) {
		await page.keyboard.press('Tab');
		focused = await page.evaluate(
			() => (document.activeElement as HTMLElement | null)?.dataset?.testid ?? null
		);
		if (focused === 'player-restart') break;
	}
	expect(
		focused,
		'RESTART-02: Tab never reached the restart button within 40 stops. Next action: keep the button in tab order.'
	).toBe('player-restart');
	const ring = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		if (!el) return { outline: 'none', shadow: 'none' };
		const style = getComputedStyle(el);
		return { outline: style.outlineStyle, shadow: style.boxShadow };
	});
	expect(
		ring.outline !== 'none' || ring.shadow !== 'none',
		`RESTART-02: focused restart button shows no focus indicator (outline=${ring.outline}, shadow=${ring.shadow}). ` +
			'Next action: keep a visible :focus-visible ring.'
	).toBe(true);
});

test('RESTART-03 pointer restarts mid-track under paused intent without extra calls', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await reachPaused(page);
	await requireHook(page, 'RESTART-03');
	const before = await lifecycleOf(page);
	const order = await queueOrder(page);

	await page.getByTestId('player-restart').click();

	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	expect(await lifecycleOf(page)).toEqual(before);
	expect(await queueOrder(page)).toEqual(order);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1');
});

test('RESTART-04 pointer restarts mid-track under playing intent without extra calls', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await focusBody(page);
	await page.keyboard.press('k');
	await expect.poll(() => lifecycleOf(page).then((calls) => calls.playCalls)).toBe(1);
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await requireHook(page, 'RESTART-04');
	const before = await lifecycleOf(page);

	await page.getByTestId('player-restart').click();

	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	expect(await lifecycleOf(page)).toEqual(before);
});

test('RESTART-05 Enter and Space restart under paused and playing intent', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 60, duration: 120 });
	await reachPaused(page);
	await requireHook(page, 'RESTART-05');

	const restart = page.getByTestId('player-restart');
	const pausedCalls = await lifecycleOf(page);
	await restart.focus();
	await page.keyboard.press('Enter');
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	expect(await lifecycleOf(page)).toEqual(pausedCalls);

	// Resume through the real transport, move mid-track, Space back to zero.
	await focusBody(page);
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 45;
	});
	await expect.poll(() => mediaTime(page)).toBe(45);
	const playingCalls = await lifecycleOf(page);
	await restart.focus();
	await page.keyboard.press('Space');
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await lifecycleOf(page)).toEqual(playingCalls);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('RESTART-06 restart at zero is a no-op with no side effects', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 0, duration: 120 });
	const state = await requireHook(page, 'RESTART-06');

	// Disabled at zero counts as a no-op too; enabled must keep time put
	// with no transport calls and no accounting of any kind.
	if (state === 'enabled') {
		const before = await lifecycleOf(page);
		await page.getByTestId('player-restart').click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBe(0);
		expect(await lifecycleOf(page)).toEqual(before);
	} else {
		expect(await mediaTime(page)).toBe(0);
	}
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
});

test('RESTART-07 an empty queue disables the button or refuses the activation', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	const state = await requireHook(page, 'RESTART-07');

	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const before = await lifecycleOf(page);
	if (state === 'enabled') {
		// Enabled with no current track must still refuse: no throw, no
		// transport calls, no track, no fabricated success.
		await page.getByTestId('player-restart').click();
		await flushFrame(page);
	}
	expect(errors).toEqual([]);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('RESTART-08 unusable duration or time refuses, and known duration restarts', async ({
	page
}) => {
	await openPlayer(page);
	await requireHook(page, 'RESTART-08');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));

	// Unknown duration: disabled, or an enabled click that leaves time put.
	await mockMedia(page, { currentTime: 30, duration: Number.NaN });
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('loadedmetadata'));
		el.dispatchEvent(new Event('timeupdate'));
	});
	await flushFrame(page);
	if ((await hookState(page)) === 'enabled') {
		await page.getByTestId('player-restart').click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBe(30);
	}

	// Infinite (live-edge) duration is not a usable finite bound either.
	await mockMedia(page, { currentTime: 30, duration: Number.POSITIVE_INFINITY });
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('loadedmetadata'));
	});
	await flushFrame(page);
	if ((await hookState(page)) === 'enabled') {
		await page.getByTestId('player-restart').click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBe(30);
	}

	// Non-finite current time refuses the same way.
	await mockMedia(page, { currentTime: Number.NaN, duration: 120 });
	await flushFrame(page);
	if ((await hookState(page)) === 'enabled') {
		await page.getByTestId('player-restart').click();
		await flushFrame(page);
		expect(await mediaTime(page)).toBeNaN();
	}

	// Duration becoming known enables real restarting on the same element.
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('loadedmetadata'));
	});
	const known = await hookState(page);
	expect(
		known,
		'RESTART-08: expected the restart button to enable once duration is known, found ' +
			`${known}. Next action: refresh availability on loadedmetadata.`
	).toBe('enabled');
	await page.getByTestId('player-restart').click();
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);

	expect(errors).toEqual([]);
});

test('RESTART-09 a rejecting seek setter is caught without throwing through the UI', async ({
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
	await requireHook(page, 'RESTART-09');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const before = await lifecycleOf(page);

	// Usable-looking media stays enabled: the refusal only surfaces on write.
	await expect(page.getByTestId('player-restart')).toBeEnabled();
	await page.getByTestId('player-restart').click();
	await flushFrame(page);

	expect(await mediaTime(page)).toBe(30);
	expect(errors).toEqual([]);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('RESTART-10 restarts follow the current element across source replacement, stale events ignored', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await requireHook(page, 'RESTART-10');
	const stale = (await page
		.getByTestId('demo-audio')
		.elementHandle()) as ElementHandle<HTMLAudioElement> | null;
	expect(stale, 'RESTART-10: expected a demo audio element before navigating').not.toBeNull();
	if (!stale) return;

	await stale.evaluate((el: HTMLAudioElement) => {
		(el as unknown as { __restartMarker?: boolean }).__restartMarker = true;
	});

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	const replaced = await page
		.getByTestId('demo-audio')
		.evaluate(
			(el: HTMLAudioElement) =>
				(el as unknown as { __restartMarker?: boolean }).__restartMarker ?? false
		);
	expect(
		replaced,
		'RESTART-10: expected track navigation to swap in a fresh audio element.'
	).toBe(false);

	// The replacement element restarts from ITS position, not the old one.
	await mockMedia(page, { currentTime: 50, duration: 200 });
	const before = await lifecycleOf(page);
	await page.getByTestId('player-restart').click();
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
	expect(
		await stale.evaluate((el: HTMLAudioElement) => el.currentTime),
		'RESTART-10: the detached element moved — restarts must target the current one.'
	).toBe(30);

	// Stale events from the detached element cannot disable, redirect or
	// complete: the current element still restarts afterwards.
	await stale.evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { value: Number.NaN, configurable: true });
		el.dispatchEvent(new Event('loadedmetadata'));
		el.dispatchEvent(new Event('error'));
		el.dispatchEvent(new Event('ended'));
	});
	await flushFrame(page);
	await expect(page.getByTestId('player-restart')).toBeEnabled();
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 80;
	});
	await expect.poll(() => mediaTime(page)).toBe(80);
	await page.getByTestId('player-restart').click();
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
});

test('RESTART-11 restarts work on the replacement element after error and retry', async ({
	page
}) => {
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

	// The retry swapped in a fresh element: restart from its doubled position.
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await requireHook(page, 'RESTART-11');
	const before = await lifecycleOf(page);
	await page.getByTestId('player-restart').click();
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);
	expect(await lifecycleOf(page)).toEqual(before);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('RESTART-12 restarts preserve rate, queue, sleep, recents and status without ended behavior', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('playback-speed').selectOption('1.5');
	await page.getByTestId('sleep-timer-set-duration').click();
	await expect(page.getByTestId('sleep-timer-status')).toHaveText('Sleep in 15 minutes');
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await requireHook(page, 'RESTART-12');
	const before = await lifecycleOf(page);
	const order = await queueOrder(page);
	const recentCount = await page.getByTestId('recent-item').count();

	await page.getByTestId('player-restart').click();
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(0);

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
	expect(await page.getByTestId('recent-item').count()).toBe(recentCount);
});

test('RESTART-13 the 0 shortcut restarts mid-track under paused and playing intent', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 45, duration: 120 });
	await reachPaused(page);
	await focusBody(page);
	const pausedCalls = await lifecycleOf(page);

	await page.keyboard.press('0');

	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	expect(await lifecycleOf(page)).toEqual(pausedCalls);

	// Resume through the real transport, move mid-track, restart from an
	// unrelated focused control: the binding is global, not body-only.
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await settle(page, 'resolve');
	await fireMediaEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 60;
	});
	await expect.poll(() => mediaTime(page)).toBe(60);
	await page.getByTestId('seek-storm').focus();
	const playingCalls = await lifecycleOf(page);
	await page.keyboard.press('0');
	await expect
		.poll(() => mediaTime(page), { timeout: 5_000 })
		.toBe(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await lifecycleOf(page)).toEqual(playingCalls);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('RESTART-14 the 0 shortcut is inert while typing, modal or modified', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 45, duration: 120 });

	// Native typing context: the digit lands in the field, transport untouched.
	await page.getByTestId('sleep-timer-minutes').focus();
	await page.keyboard.press('0');
	await flushFrame(page);
	expect(await mediaTime(page)).toBe(45);

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
		await page.keyboard.press('0');
		if (testid === 'probe-editable') {
			await expect(field).toHaveText('0');
		} else {
			await expect(field).toHaveValue('0');
		}
		expect(await mediaTime(page)).toBe(45);
	}

	// A modal dialog captures the key: still no restart, modal undisturbed.
	await page.evaluate(() => {
		const dialog = document.createElement('dialog');
		dialog.setAttribute('data-testid', 'probe-dialog');
		dialog.innerHTML = '<button data-testid="probe-dialog-ok">OK</button>';
		document.querySelector('main')?.append(dialog);
		dialog.showModal();
		dialog.querySelector('button')?.focus();
	});
	await expect(page.getByTestId('probe-dialog')).toBeVisible();
	await page.keyboard.press('0');
	await flushFrame(page);
	expect(await mediaTime(page)).toBe(45);
	const probe = await page.evaluate(() => {
		const dialog = document.querySelector<HTMLDialogElement>('[data-testid="probe-dialog"]');
		return { open: dialog?.open ?? false, inside: dialog?.contains(document.activeElement) ?? false };
	});
	expect(probe).toEqual({ open: true, inside: true });
	await page.evaluate(() => {
		document.querySelector<HTMLDialogElement>('[data-testid="probe-dialog"]')?.close();
	});

	// Modified chords never restart: dispatched, not pressed, so no browser
	// chrome accelerator (zoom reset on Ctrl+0) can fire; the page only sees
	// the keydown either way.
	await focusBody(page);
	await page.evaluate(() => {
		for (const init of [
			{ key: '0', ctrlKey: true },
			{ key: '0', altKey: true },
			{ key: '0', metaKey: true }
		] as KeyboardEventInit[]) {
			window.dispatchEvent(new KeyboardEvent('keydown', { ...init, bubbles: true }));
		}
	});
	await flushFrame(page);
	expect(await mediaTime(page)).toBe(45);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => lifecycleOf(page).then((calls) => calls.playCalls)).toBe(0);
});

test('RESTART-15 the shortcuts dialog lists the 0 restart binding', async ({ page }) => {
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await focusBody(page);

	await page.keyboard.press('?');

	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(dialog).toBeVisible();
	expect(
		await dialog.locator('kbd', { hasText: '0' }).count(),
		'RESTART-15: expected a kbd 0 row in the shortcuts dialog. ' +
			'Next action: list the 0 restart binding alongside the existing rows.'
	).toBeGreaterThanOrEqual(1);
	await expect(dialog).toContainText(/restart/i);
	// The existing rows stay listed alongside the new one.
	for (const key of ['k', 'n', 'p', 'j', 'l', 'm', '?']) {
		expect(await dialog.locator('kbd', { hasText: key }).count()).toBeGreaterThanOrEqual(1);
	}
});

test('RESTART-16 global key semantics and typing guards are unchanged', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 30, duration: 120 });
	await focusBody(page);

	// j/l still move by exactly 10 seconds through the mocked media.
	await page.keyboard.press('l');
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(40);
	await page.keyboard.press('j');
	await expect.poll(() => mediaTime(page), { timeout: 5_000 }).toBe(30);

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
