import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance-owned RADIO-CHOICE contract; the Builder continues this branch
 * WITHOUT editing this file or the existing player/radio/comfort tests.
 *
 * New seam: a checkbox named "Continue with radio when queue ends" on the
 * existing /demo/player host. Persist ONLY JSON boolean true/false at
 * localStorage `nntune:radio-continue:v1`, like the existing versioned player
 * preferences. Missing/invalid/unavailable storage defaults to enabled.
 * These are test-first interface choices, not imports from an implementation.
 *
 * Breaks caught: unconditional radio (RC-03), inert/inaccessible toggle (01),
 * losing the existing mix (02), unsafe restore/write (04-06), using preference
 * changes as Stop radio/reload (07-08), confusing Next/pause/stale events with
 * natural exhaustion (09-10), sleep losing precedence (11), and an early
 * disabled-return skipping resume clearing (12). No repeat dependency: this
 * baseline has no repeat control; a future repeat implementation owns its tests.
 *
 * Synthetic media is held pending, as in radio-continue.spec.ts: no provider,
 * decoder errors, or wall-clock track durations. Only native media boundaries
 * are stubbed; the real host handles events, storage, transport and timer state.
 */
const LABEL = 'Continue with radio when queue ends';
const PREFERENCE_KEY = 'nntune:radio-continue:v1';
const RESUME_KEY = 'nntune:resume:v1';
const TITLES = ['Demo track', 'Second Signal', 'Third Light'];
type Fault = 'getter' | 'read' | 'write';
type MediaProbe = { plays: number; pauses: number };

// Missing controls should fail promptly rather than consuming the whole case.
test.use({ actionTimeout: 5_000 });

async function openPlayer(page: Page, options: { stored?: string; fault?: Fault } = {}) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(({ key, stored, fault }) => {
		const probe = { plays: 0, pauses: 0 };
		Object.assign(window, { __radioChoice: probe });
		HTMLMediaElement.prototype.play = function () {
			probe.plays += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauses += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		if (stored !== undefined) localStorage.setItem(key, stored);
		if (fault === 'getter') {
			Object.defineProperty(window, 'localStorage', {
				configurable: true,
				get() { throw new DOMException('Fixture storage blocked', 'SecurityError'); }
			});
		} else if (fault) {
			// Includes method failures separately from property access failures.
			Storage.prototype[fault === 'read' ? 'getItem' : 'setItem'] = function () {
				throw new DOMException('Fixture storage unavailable', 'QuotaExceededError');
			};
		}
	}, { key: PREFERENCE_KEY, ...options });
	await page.goto('/demo/player?radio-seed=7');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// Existing mount-only probe: avoid treating SSR markup as interactive UI.
	await page.waitForFunction(() => typeof (
		window as unknown as { __levelingProbe?: unknown }
	).__levelingProbe === 'function');
	return errors;
}

function preference(page: Page) {
	return page.getByRole('checkbox', { name: LABEL, exact: true });
}

async function mediaProbe(page: Page): Promise<MediaProbe> {
	return page.evaluate(() => ({ ...(
		window as unknown as { __radioChoice: MediaProbe }
	).__radioChoice }));
}

async function naturalEnd(page: Page) {
	await page.getByTestId('demo-audio').evaluate((audio) => {
		Object.defineProperty(audio, 'paused', { value: true, configurable: true });
		Object.defineProperty(audio, 'ended', { value: true, configurable: true });
		audio.dispatchEvent(new Event('pause'));
		audio.dispatchEvent(new Event('ended'));
	});
}

async function selectTrack(page: Page, id = 'third-light', title = 'Third Light') {
	await page.getByTestId(`queue-select-${id}`).click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('now-playing')).toContainText(title);
}

async function expectStopped(page: Page, plays: number, status = 'ended') {
	// Bounded negative-observation window, not a media/network arrival sleep.
	await page.waitForTimeout(150);
	await expect(page.getByTestId('player-status')).toHaveText(`Status: ${status}`);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	expect((await mediaProbe(page)).plays).toBe(plays);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function storageSnapshot(page: Page) {
	return page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }));
}

test('RC-01 default preference is labeled, tabbable and keyboard usable in the demo host', async ({ page }) => {
	await openPlayer(page);
	const toggle = preference(page);
	await expect(toggle).toBeVisible();
	await expect(toggle).toBeChecked();
	await expect(page.getByRole('heading', { name: /player.*demo|demo.*player/i })).toBeVisible();
	// Real sequential Tab navigation also catches tabindex=-1, unlike focus().
	for (let i = 0; i < 80 && !(await toggle.evaluate((el) => el === document.activeElement)); i += 1) {
		await page.keyboard.press('Tab');
	}
	await expect(toggle).toBeFocused();
	await page.keyboard.press('Space');
	await expect(toggle).not.toBeChecked();
	await page.keyboard.press('Space');
	await expect(toggle).toBeChecked();
});

test('RC-02 control: enabled default preserves the seeded radio pass after the final natural end', async ({ page }) => {
	await openPlayer(page);
	await selectTrack(page, 'demo-track', 'Demo track');
	for (const title of ['Second Signal', 'Third Light']) {
		await naturalEnd(page);
		await expect(page.getByTestId('now-playing')).toContainText(title);
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	}
	const before = (await mediaProbe(page)).plays;
	const order: string[] = [];
	for (let i = 0; i < 3; i += 1) {
		await naturalEnd(page);
		await expect(page.getByTestId('radio-badge')).toBeVisible();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		const text = await page.getByTestId('now-playing').innerText();
		const matches = TITLES.filter((title) => text.includes(title));
		expect(matches).toHaveLength(1);
		order.push(matches[0]!);
	}
	expect([...order].sort()).toEqual([...TITLES].sort());
	expect((await mediaProbe(page)).plays).toBe(before + 3);
	// Seed determinism and rollover remain pinned by radio-continue.spec.ts.
});

test('RC-03 restored false finishes the chosen queue without another play request', async ({ page }) => {
	await openPlayer(page, { stored: 'false' });
	await selectTrack(page, 'demo-track', 'Demo track');
	for (const title of ['Second Signal', 'Third Light']) {
		await naturalEnd(page);
		await expect(page.getByTestId('now-playing')).toContainText(title);
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	}
	const before = (await mediaProbe(page)).plays;
	await naturalEnd(page);
	// Deliberately assert behavior before the new selector: baseline RED must
	// demonstrate unconditional playback, not only a missing checkbox.
	await expectStopped(page, before);
	await expect(preference(page)).not.toBeChecked();
	await naturalEnd(page);
	await expectStopped(page, before);
});

test('RC-04 choice persists only a boolean and reload restores both values without autoplay', async ({ page }) => {
	await openPlayer(page);
	await page.waitForTimeout(150);
	const before = await storageSnapshot(page);
	await preference(page).uncheck();
	await expect.poll(() => page.evaluate((key) => localStorage.getItem(key), PREFERENCE_KEY)).toBe('false');
	const after = await storageSnapshot(page);
	expect(after.session).toEqual(before.session);
	const { [PREFERENCE_KEY]: value, ...unrelated } = after['local'];
	const { [PREFERENCE_KEY]: _previous, ...previousUnrelated } = before['local'];
	expect(value).toBe('false');
	expect(unrelated).toEqual(previousUnrelated);
	await page.reload();
	await expect(preference(page)).not.toBeChecked();
	expect((await mediaProbe(page)).plays).toBe(0);
	await selectTrack(page);
	const beforeEnd = (await mediaProbe(page)).plays;
	await naturalEnd(page);
	await expectStopped(page, beforeEnd);
	await preference(page).check();
	await expect.poll(() => page.evaluate((key) => localStorage.getItem(key), PREFERENCE_KEY)).toBe('true');
	await page.reload();
	await expect(preference(page)).toBeChecked();
	expect((await mediaProbe(page)).plays).toBe(0);
	await selectTrack(page);
	await naturalEnd(page);
	await expect(page.getByTestId('radio-badge')).toBeVisible();
});

for (const [id, stored] of [
	['json-string', '"false"'], ['number', '0'], ['null', 'null'],
	['array', '[false]'], ['object', '{"enabled":false,"queue":["third-light"]}'],
	['broken-json', '{false']
]) {
	test(`RC-05-${id} invalid storage defaults on and still permits an in-session opt-out`, async ({ page }) => {
		const errors = await openPlayer(page, { stored });
		await expect(preference(page)).toBeChecked();
		await selectTrack(page);
		await naturalEnd(page);
		await expect(page.getByTestId('radio-badge')).toBeVisible();
		await page.getByTestId('radio-stop').click();
		await preference(page).uncheck();
		await selectTrack(page);
		const before = (await mediaProbe(page)).plays;
		await naturalEnd(page);
		await expectStopped(page, before);
		expect(errors).toEqual([]);
	});
}

for (const fault of ['getter', 'read', 'write'] as const) {
	test(`RC-06-${fault} unavailable storage preserves default playback and a working in-session preference`, async ({ page }) => {
		const errors = await openPlayer(page, { fault });
		await expect(preference(page)).toBeChecked();
		await selectTrack(page);
		await naturalEnd(page);
		await expect(page.getByTestId('radio-badge')).toBeVisible();
		await page.getByTestId('radio-stop').click();
		await preference(page).uncheck();
		await selectTrack(page);
		const before = (await mediaProbe(page)).plays;
		await naturalEnd(page);
		await expectStopped(page, before);
		await preference(page).check();
		await selectTrack(page);
		await naturalEnd(page);
		await expect(page.getByTestId('radio-badge')).toBeVisible();
		expect(errors).toEqual([]);
	});
}

test('RC-07 toggling while a queued track plays changes only what happens at exhaustion', async ({ page }) => {
	await openPlayer(page);
	await selectTrack(page);
	await page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => { audio.currentTime = 12; });
	const before = await mediaProbe(page);
	await preference(page).uncheck();
	await preference(page).check();
	await preference(page).uncheck();
	await page.waitForTimeout(150);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	expect(await mediaProbe(page)).toEqual(before);
	expect(await page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => audio.currentTime)).toBe(12);
	await naturalEnd(page);
	await expectStopped(page, before.plays);
});

test('RC-08 disabling during radio is not Stop radio and does not interrupt subsequent radio tracks', async ({ page }) => {
	await openPlayer(page);
	await selectTrack(page);
	await naturalEnd(page);
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	const title = await page.getByTestId('now-playing').innerText();
	const before = await mediaProbe(page);
	await preference(page).uncheck();
	await page.waitForTimeout(150);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	await expect(page.getByTestId('now-playing')).toHaveText(title);
	expect(await mediaProbe(page)).toEqual(before);
	await naturalEnd(page);
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect((await mediaProbe(page)).plays).toBe(before.plays + 1);
	await page.getByTestId('radio-stop').click();
	const stopped = (await mediaProbe(page)).plays;
	await preference(page).check();
	await naturalEnd(page);
	await page.waitForTimeout(150);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	expect((await mediaProbe(page)).plays).toBe(stopped);
});

for (const enabled of [true, false]) {
	test(`RC-09-${enabled} explicit Next ends without radio and Previous remains usable`, async ({ page }) => {
		await openPlayer(page, { stored: String(enabled) });
		await selectTrack(page);
		const before = (await mediaProbe(page)).plays;
		await page.getByTestId('player-next').click();
		await expectStopped(page, before);
		await naturalEnd(page);
		await expectStopped(page, before);
		await page.getByTestId('player-prev').click();
		await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	});

	test(`RC-10-${enabled} stale ended and manual pause cannot start radio`, async ({ page }) => {
		await openPlayer(page, { stored: String(enabled) });
		await selectTrack(page);
		const before = (await mediaProbe(page)).plays;
		await page.getByTestId('demo-audio').evaluate((audio) => {
			Object.defineProperty(audio, 'paused', { value: true, configurable: true });
			Object.defineProperty(audio, 'ended', { value: false, configurable: true });
			audio.dispatchEvent(new Event('pause'));
		});
		await expectStopped(page, before, 'paused');
		// Preserve the existing guard: a delayed ended after manual pause
		// must not be mistaken for the natural exhaustion of a playing track.
		await page.getByTestId('demo-audio').dispatchEvent('ended');
		await expectStopped(page, before, 'paused');
	});

	for (const mode of ['end-of-track', 'stop-after', 'duration'] as const) {
		test(`RC-11-${enabled}-${mode} sleep completion wins over the radio preference`, async ({ page }) => {
			await openPlayer(page, { stored: String(enabled) });
			await selectTrack(page);
			if (mode === 'stop-after') await page.getByTestId('sleep-timer-tracks').fill('1');
			if (mode === 'duration') await page.getByTestId('sleep-timer-minutes').fill('1');
			await page.getByTestId(`sleep-timer-set-${mode}`).click();
			await expect(page.getByTestId('sleep-timer-cancel')).toBeEnabled();
			const before = (await mediaProbe(page)).plays;
			if (mode === 'duration') {
				await page.evaluate(() => (
					window as unknown as { __comforts: { advanceSleep(ms: number): void } }
				).__comforts.advanceSleep(61_000));
			}
			await naturalEnd(page);
			await expectStopped(page, before, 'paused');
			await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
		});
	}
}

test('RC-12 disabling radio must not skip clearing the completed track resume position', async ({ page }) => {
	await openPlayer(page, { stored: 'false' });
	await selectTrack(page);
	await page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => {
		audio.currentTime = 42;
		audio.dispatchEvent(new Event('timeupdate'));
	});
	await page.evaluate((key) => localStorage.setItem(key, '{"third-light":42}'), RESUME_KEY);
	await naturalEnd(page);
	await page.getByTestId('player-prev').click();
	await selectTrack(page);
	expect(await page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => audio.currentTime)).toBe(0);
	const saved = await page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? '{}'), RESUME_KEY);
	expect(saved['third-light']).toBeUndefined();
});
