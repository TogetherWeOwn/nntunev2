import { expect, test, type Browser, type Page } from '@playwright/test';

/**
 * Acceptance: auto-continue radio when the queue ends (test half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build half continues
 * this branch and must make these tests pass WITHOUT editing this file; a
 * wrong test comes back to the acceptance card instead.
 *
 * Surface: `/demo/player` is the only surface with natural track end today
 * (audio element + `ended` handling + transport states), so radio is pinned
 * here. The demo catalogue (Demo track, Second Signal, Third Light) is the
 * loaded-library stand-in, exactly as in the neighbouring queue specs; the
 * library page carries queue metadata but no playback, so end-of-media
 * cannot happen there. Media loading is held pending (route-held, never
 * fulfilled) and `play()` is stubbed to resolve, so play-through is driven
 * by synthetic media events with no network beyond the local preview server.
 *
 * Contract seams the build half implements:
 * - `radio-badge`: visible while radio plays, containing the text "Radio",
 *   with `role="status"` so the start of radio is announced (the codebase's
 *   established announce pattern).
 * - `radio-stop`: a button, visible and enabled while radio plays.
 *   Activating it stops radio and returns the transport to idle: the badge
 *   is removed, `player-status` reads `Status: ended` (the established
 *   end-of-queue idle state), and nothing restarts on its own.
 * - `?radio-seed=N`: seeds the radio shuffle. Same seed + same loaded set
 *   gives the same mix order. Only the explicit seed is pinned; the
 *   default seed source (when the param is absent) is the builder's choice.
 *
 * What's pinned: natural end of the last queued track starts a
 * non-repeating seeded mix (each catalogue track exactly once per pass)
 * with an announced badge; stop returns to idle with no loop; same seed
 * gives the same order across fresh sessions; pause never starts radio,
 * even on the last track, even when a stale `ended` arrives while paused.
 *
 * Sensitivity note: with a 3-track catalogue an unseeded shuffle passes
 * the 3-session determinism pin with probability (1/6)^2 ~= 3%. The
 * builder's determinism units over larger fixtures carry the rest; this
 * pin carries the browser-level same-seed/same-order contract.
 *
 * Out of scope (not pinned): behaviour after a full radio pass (loop with
 * a new seed vs. stop), queue-panel edits during radio, MediaSession
 * metadata for radio tracks, default-seed stability across reloads.
 */

const SEED = 7;
const DETERMINISM_SEED = 11;
const CATALOGUE_TITLES = ['Demo track', 'Second Signal', 'Third Light'];

/** Drive the player with synthetic media events; the stubbed play resolves. */
async function openPlayer(page: Page, seed: number = SEED): Promise<void> {
	// Hold media loading synthetic and pending (the lifecycle-spec pattern):
	// fulfilling garbage bytes would fire a native error event on the real
	// element. Only fixture events settle playback.
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		const control = { playCalls: 0 };
		Object.assign(window, { __radio: control });
		HTMLMediaElement.prototype.play = function () {
			control.playCalls += 1;
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto(`/demo/player?radio-seed=${seed}`);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function playCalls(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __radio: { playCalls: number } }).__radio.playCalls
	);
}

async function naturalEnd(page: Page): Promise<void> {
	await page.getByTestId('demo-audio').evaluate((el) => {
		// Browser end-of-media steps set both flags, then fire pause before ended.
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}

/** Ordinary user pause: paused with ended=false. */
async function pauseOnly(page: Page): Promise<void> {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('pause'));
	});
}

async function nowPlayingText(page: Page): Promise<string> {
	return (await page.getByTestId('now-playing').textContent()) ?? '';
}

function whichCatalogueTitle(text: string): string {
	const found = CATALOGUE_TITLES.filter((title) => text.includes(title));
	expect(found, `now-playing names a catalogue track, got: ${text}`).toHaveLength(1);
	return found[0]!;
}

/** Play the first track, then advance to the second and third by natural end. */
async function playQueueToLastTrack(page: Page): Promise<void> {
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}

/**
 * Play the whole queue through natural ends until radio starts. Returns the
 * first radio track's catalogue title.
 */
async function playThroughToRadio(page: Page): Promise<string> {
	await playQueueToLastTrack(page);
	await naturalEnd(page);
	const badge = page.getByTestId('radio-badge');
	await expect(badge).toContainText('Radio');
	return whichCatalogueTitle(await nowPlayingText(page));
}

/** Collect `count` radio-track titles, starting with the current one. */
async function collectRadioOrder(page: Page, count: number): Promise<string[]> {
	const order: string[] = [whichCatalogueTitle(await nowPlayingText(page))];
	for (let i = 1; i < count; i += 1) {
		await naturalEnd(page);
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await expect(page.getByTestId('radio-badge')).toContainText('Radio');
		order.push(whichCatalogueTitle(await nowPlayingText(page)));
	}
	return order;
}

async function radioOrderInFreshContext(browser: Browser, seed: number): Promise<string[]> {
	const context = await browser.newContext();
	try {
		const page = await context.newPage();
		await openPlayer(page, seed);
		await playThroughToRadio(page);
		return await collectRadioOrder(page, CATALOGUE_TITLES.length);
	} finally {
		await context.close();
	}
}

test('natural end of the queue starts radio with an announced Radio badge', async ({
	page
}) => {
	await openPlayer(page);
	await playQueueToLastTrack(page);

	await naturalEnd(page);

	const badge = page.getByTestId('radio-badge');
	await expect(badge).toBeVisible();
	await expect(badge).toContainText('Radio');
	await expect(badge).toHaveAttribute('role', 'status');
	// A radio track is up: a catalogue title shows and the transport is not idle.
	whichCatalogueTitle(await nowPlayingText(page));
	await expect(page.getByTestId('player-status')).not.toContainText('ended');
	const stop = page.getByTestId('radio-stop');
	await expect(stop).toBeVisible();
	await expect(stop).toBeEnabled();
});

test('radio mix plays each catalogue track exactly once before repeating', async ({ page }) => {
	await openPlayer(page);
	await playThroughToRadio(page);

	const order = await collectRadioOrder(page, CATALOGUE_TITLES.length);
	expect([...order].sort()).toEqual([...CATALOGUE_TITLES].sort());
});

test('same seed and library produce the same radio order', async ({ browser }) => {
	const first = await radioOrderInFreshContext(browser, DETERMINISM_SEED);
	const second = await radioOrderInFreshContext(browser, DETERMINISM_SEED);
	const third = await radioOrderInFreshContext(browser, DETERMINISM_SEED);

	// Guard against a degenerate all-equal pass: each run is a full permutation.
	for (const order of [first, second, third]) {
		expect([...order].sort()).toEqual([...CATALOGUE_TITLES].sort());
	}
	expect(second).toEqual(first);
	expect(third).toEqual(first);
});

test('radio stop returns to idle and stays stopped', async ({ page }) => {
	await openPlayer(page);
	await playThroughToRadio(page);

	// Keyboard activation pins that the stop control is keyboard reachable.
	await page.getByTestId('radio-stop').focus();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');

	// No loop: a further natural end must not restart radio on its own.
	const callsBefore = await playCalls(page);
	await naturalEnd(page);
	await page.waitForTimeout(500);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
	await expect.poll(() => playCalls(page)).toBe(callsBefore);
});

test('pause never starts radio, even on the last track', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	// Mid-queue pause: wait, then assert silence and no radio.
	await pauseOnly(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	let callsBefore = await playCalls(page);
	await page.waitForTimeout(500);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => playCalls(page)).toBe(callsBefore);

	// Last-track pause: radio must not start while paused either.
	await page.getByTestId('queue-select-third-light').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await pauseOnly(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	callsBefore = await playCalls(page);
	await page.waitForTimeout(500);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect.poll(() => playCalls(page)).toBe(callsBefore);

	// A stale `ended` arriving while paused is not a natural end: still no radio.
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('ended')));
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect.poll(() => playCalls(page)).toBe(callsBefore);
});
