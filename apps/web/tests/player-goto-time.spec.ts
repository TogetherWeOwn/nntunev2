import { expect, test, type Page } from '@playwright/test';

/**
 * PROTECTED GOTO acceptance handoff. Only this new browser spec and
 * src/lib/player/gotoTime.acceptance.test.ts belong to this slice.
 * Builder implements the form without modifying either acceptance file.
 *
 * New /demo/player hooks:
 * - player-goto-time: labeled native text input, name /go to time/i,
 *   associated hint naming seconds, m:ss and h:mm:ss; inside a real form.
 * - player-goto-seek: native submit button named "Seek" in that form.
 * - player-goto-error: actionable inline error with an id associated with
 *   the input by aria-describedby or aria-errormessage and aria-invalid.
 * - player-goto-status: restrained (<160 chars) polite status on success,
 *   mentioning seek/time, never success after a refusal. Clear old error
 *   and success feedback on track change. Do not steal focus into a modal.
 *
 * Reuse parseTimestampParam; independently reject null, nonfinite,
 * negative, unsafe/overflow or out-of-duration results. Duration must be
 * finite/nonnegative and read from the CURRENT connected, matching media
 * at submission, not cached at input/change/loadedmetadata time. Do not
 * clamp an explicit out-of-range request. Preserve transport intent,
 * rate, volume/mute, source, queue order and selection. Never call play
 * to seek. Catch a throwing currentTime setter without claiming success.
 *
 * Only the media boundary is doubled (pending synthetic audio route,
 * play/pause and per-element time/duration); the mounted page, form,
 * event handlers, parser, queue and feedback are real production code.
 * No new production import/stub is needed for RED. Missing UI fails an
 * explicit assertion after the existing page has mounted, not a timeout
 * on clicking an absent control. GOTO-C01/C02 are baseline controls.
 *
 * Breaks caught: wrong seconds/clock arithmetic; silent clamp; finite
 * guard omitted; stale duration/element; unconditional success in catch;
 * accidental play/toggle; shortcut leakage; stale feedback; inaccessible
 * error or overflowing controls. Run using package.json test:e2e script
 * and the repository Playwright config, with media only mocked locally.
 */

test.use({ screenshot: 'only-on-failure' });

type Calls = { play: number; pause: number; load: number };
type Control = Calls & { resolve: () => void };
type InstrumentedAudio = HTMLAudioElement & { __gotoWrites: number[] };

async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.clear();
		localStorage.clear();
		const control: Control = { play: 0, pause: 0, load: 0, resolve: () => {} };
		Object.assign(window, { __gotoMedia: control });
		HTMLMediaElement.prototype.play = function () {
			control.play += 1;
			return new Promise<void>((resolve) => { control.resolve = resolve; });
		};
		HTMLMediaElement.prototype.pause = function () { control.pause += 1; };
		const load = HTMLMediaElement.prototype.load;
		HTMLMediaElement.prototype.load = function () {
			control.load += 1;
			return load.call(this);
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
}

async function mockMedia(page: Page, duration = 120, rejects = false) {
	await page.getByTestId('demo-audio').evaluate((node, seed) => {
		const audio = node as InstrumentedAudio;
		let time = 30;
		audio.__gotoWrites = [];
		Object.defineProperty(audio, 'duration', { value: seed.duration, configurable: true });
		Object.defineProperty(audio, 'currentTime', {
			get: () => time,
			set: (next: number) => {
				audio.__gotoWrites.push(next);
				if (seed.rejects) throw new DOMException('private decoder detail', 'InvalidStateError');
				time = next;
			},
			configurable: true
		});
		audio.dispatchEvent(new Event('loadedmetadata'));
		audio.dispatchEvent(new Event('timeupdate'));
	}, { duration, rejects });
}

async function calls(page: Page): Promise<Calls> {
	return page.evaluate(() => {
		const c = (window as unknown as { __gotoMedia: Control }).__gotoMedia;
		return { play: c.play, pause: c.pause, load: c.load };
	});
}

async function media(page: Page) {
	return page.getByTestId('demo-audio').evaluate((node) => {
		const el = node as InstrumentedAudio;
		return { time: el.currentTime, writes: el.__gotoWrites, rate: el.playbackRate,
			volume: el.volume, muted: el.muted, src: el.getAttribute('src') };
	});
}

async function queue(page: Page) {
	return page.getByTestId('queue-item').allTextContents();
}

async function requireForm(page: Page, id: string) {
	expect(await page.getByTestId('player-goto-time').count(),
		`${id}: missing labeled Go to time entry on the real demo player. Next: render the timestamp form.`
	).toBe(1);
	expect(await page.getByTestId('player-goto-seek').count(),
		`${id}: missing explicit Seek submit action. Next: wire the timestamp form submission.`
	).toBe(1);
	return page.getByTestId('player-goto-time');
}

async function submit(page: Page, raw: string) {
	await page.getByTestId('player-goto-time').fill(raw);
	const button = page.getByTestId('player-goto-seek');
	if (await button.isEnabled()) await button.click();
	else {
		// A disabled affordance must still explain unavailable media. Exercise
		// the real form handler too so disabled UI cannot hide a missing guard.
		await page.getByTestId('player-goto-time').evaluate((node) => {
			(node as HTMLInputElement).form!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
		});
	}
}

async function expectError(page: Page, reason: RegExp) {
	const error = page.getByTestId('player-goto-error');
	await expect(error).toBeVisible();
	await expect(error).toContainText(reason);
	const input = page.getByTestId('player-goto-time');
	await expect(input).toHaveAttribute('aria-invalid', 'true');
	const id = await error.getAttribute('id');
	expect(id, 'GOTO: inline correction must have a nonblank id').toBeTruthy();
	const refs = [await input.getAttribute('aria-describedby'), await input.getAttribute('aria-errormessage')]
		.filter(Boolean).join(' ').split(/\s+/);
	expect(refs, 'GOTO: associate the inline correction with the input').toContain(id);
	await expect(page.getByTestId('player-goto-status')).toBeEmpty();
}

async function expectSuccess(page: Page) {
	const status = page.getByTestId('player-goto-status');
	await expect(status).toBeVisible();
	await expect(status).toContainText(/seek|time|jump|moved/i);
	await expect(status).not.toContainText(/cannot|could not|unable|failed|invalid|unavailable/i);
	const text = (await status.innerText()).trim();
	expect(text.length).toBeLessThan(160);
	expect(await status.getAttribute('role') === 'status' || await status.getAttribute('aria-live') === 'polite').toBe(true);
	await expect(page.getByTestId('player-goto-time')).not.toHaveAttribute('aria-invalid', 'true');
	await expect(page.getByTestId('player-goto-error')).not.toBeVisible();
}

async function reachIntent(page: Page, intent: 'paused' | 'playing') {
	await page.evaluate(() => { (document.activeElement as HTMLElement | null)?.blur(); });
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await page.evaluate(() => { (window as unknown as { __gotoMedia: Control }).__gotoMedia.resolve(); });
	await page.getByTestId('demo-audio').evaluate((node) => {
		Object.defineProperty(node, 'paused', { value: false, configurable: true });
		node.dispatchEvent(new Event('playing'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	if (intent === 'paused') {
		await page.keyboard.press('k');
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	}
}

test('GOTO-C01 existing relative seek operates on the mounted page', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await page.getByTestId('player-seek-forward').click();
	await expect.poll(() => media(page).then((m) => m.time)).toBe(40);
	await page.getByTestId('player-seek-back').click();
	await expect.poll(() => media(page).then((m) => m.time)).toBe(30);
	await expect(page.getByTestId('share-copy')).toBeVisible();
	await expect(page.getByTestId('demo-audio')).toHaveAttribute('controls', '');
});

test('GOTO-C02 the existing player reaches paused and playing intent', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await reachIntent(page, 'paused');
	expect((await calls(page)).play).toBe(1);
});

test('GOTO-01 labeled native form documents all accepted timestamp forms', async ({ page }) => {
	await openPlayer(page);
	const field = await requireForm(page, 'GOTO-01');
	await expect(field).toHaveAccessibleName(/go to time/i);
	await expect(field).toHaveAccessibleDescription(/seconds/i);
	await expect(field).toHaveAccessibleDescription(/m:ss/);
	await expect(field).toHaveAccessibleDescription(/h:mm:ss/);
	expect(await field.evaluate((el) => el.tagName)).toBe('INPUT');
	const button = page.getByTestId('player-goto-seek');
	await expect(button).toHaveAccessibleName(/^seek$/i);
	expect(await button.evaluate((el) => el.tagName)).toBe('BUTTON');
	expect(await button.evaluate((el) => (el as HTMLButtonElement).type)).toBe('submit');
	expect(await field.evaluate((el) => {
		const form = (el as HTMLInputElement).form;
		return form !== null && form === (document.querySelector('[data-testid="player-goto-seek"]') as HTMLButtonElement)?.form;
	})).toBe(true);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(page.getByRole('main')).toContainText(/synthetic|demo/i);
});

for (const [id, raw, seconds, duration] of [
	['GOTO-02', '90.5', 90.5, 120],
	['GOTO-03', ' 1:30 ', 90, 120],
	['GOTO-04', '1:02:03', 3723, 4000],
	['GOTO-05', '0', 0, 120],
	['GOTO-06', '2:00', 120, 120]
] as const) {
	test(`${id} explicitly seeks ${JSON.stringify(raw)} to ${seconds}`, async ({ page }) => {
		await openPlayer(page);
		await mockMedia(page, duration);
		await requireForm(page, id);
		const before = await calls(page);
		await submit(page, raw);
		await expect.poll(() => media(page).then((m) => m.time)).toBe(seconds);
		expect((await media(page)).writes).toEqual([seconds]);
		expect(await calls(page)).toEqual(before);
		await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
		await expectSuccess(page);
	});
}

for (const [id, raw] of [
	['GOTO-07', ''], ['GOTO-08', '   '], ['GOTO-09', 'soon'],
	['GOTO-10', '-1'], ['GOTO-11', 'NaN'], ['GOTO-12', 'Infinity'],
	['GOTO-13', '1:60'], ['GOTO-14', '1e2'], ['GOTO-15', '1:2:3:4'],
	['GOTO-16', '9'.repeat(309)], ['GOTO-17', `${'9'.repeat(309)}:00`],
	['GOTO-18', `${'9'.repeat(309)}:00:00`], ['GOTO-19', '9007199254740992']
] as const) {
	test(`${id} refuses invalid or overflowing entry ${raw.length > 40 ? '(overflow)' : JSON.stringify(raw)}`, async ({ page }) => {
		await openPlayer(page);
		await mockMedia(page, id === 'GOTO-19' ? Number.MAX_VALUE : 120);
		await requireForm(page, id);
		const before = await calls(page);
		await submit(page, raw);
		await expectError(page, /enter|valid|seconds|time|duration|range|between|exceed/i);
		expect((await media(page)).time).toBe(30);
		expect((await media(page)).writes).toEqual([]);
		expect(await calls(page)).toEqual(before);
	});
}

test('GOTO-20 out-of-duration entry is corrected inline, never clamped, then recoverable', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	const field = await requireForm(page, 'GOTO-20');
	const before = await calls(page);
	await submit(page, '120.001');
	await expectError(page, /duration|range|between|exceed|within|before|maximum|long|end/i);
	expect((await media(page)).writes).toEqual([]);
	expect((await media(page)).time).toBe(30);
	await expect(field).toHaveValue('120.001');
	await field.fill('1:00');
	await field.press('Enter');
	await expect.poll(() => media(page).then((m) => m.time)).toBe(60);
	await expectSuccess(page);
	expect(await calls(page)).toEqual(before);
	await expect(field).toBeFocused();
});

for (const [id, duration] of [
	['GOTO-21', Number.NaN], ['GOTO-22', Number.POSITIVE_INFINITY], ['GOTO-23', -1]
] as const) {
	test(`${id} independently rejects unusable live duration`, async ({ page }) => {
		await openPlayer(page);
		await mockMedia(page);
		await requireForm(page, id);
		await page.getByTestId('player-goto-time').fill('60');
		// Change only the live property: no metadata event to refresh a cache.
		await page.getByTestId('demo-audio').evaluate((el, value) => {
			Object.defineProperty(el, 'duration', { value, configurable: true });
		}, duration);
		const before = await calls(page);
		await submit(page, '60');
		await expectError(page, /duration|available|ready|load|known/i);
		expect((await media(page)).writes).toEqual([]);
		expect((await media(page)).time).toBe(30);
		expect(await calls(page)).toEqual(before);
	});
}

for (const intent of ['paused', 'playing'] as const) {
	test(`GOTO-24-${intent} seeks without changing intent, speed, sound or arranged queue`, async ({ page }) => {
		await openPlayer(page);
		await mockMedia(page);
		await reachIntent(page, intent);
		await requireForm(page, `GOTO-24-${intent}`);
		await page.getByTestId('playback-speed').selectOption('1.5');
		await page.getByTestId('queue-move-up-third-light').click();
		await page.getByTestId('queue-move-up-third-light').click();
		expect(await page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id'))))
			.toEqual(['third-light', 'demo-track', 'second-signal']);
		await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => { el.volume = 0.35; el.muted = true; });
		const before = await calls(page);
		const oldMedia = await media(page);
		const order = await queue(page);
		await submit(page, '1:10');
		await expect.poll(() => media(page).then((m) => m.time)).toBe(70);
		expect(await media(page)).toEqual({ ...oldMedia, time: 70, writes: [70] });
		expect(await calls(page)).toEqual(before);
		expect(await queue(page)).toEqual(order);
		await expect(page.getByTestId('now-playing')).toContainText('Demo track');
		await expect(page.getByTestId('player-status')).toHaveText(`Status: ${intent}`);
		await expectSuccess(page);
	});
}

test('GOTO-25 editing excludes global shortcuts; Enter submits only this form', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	const field = await requireForm(page, 'GOTO-25');
	const before = await calls(page);
	const oldMedia = await media(page);
	const order = await queue(page);
	const url = page.url();
	await field.focus();
	for (const key of ['k', 'j', 'l', 'm', 'Space', 'ArrowLeft', 'ArrowRight', 'Home', 'End']) await field.press(key);
	expect(await media(page)).toEqual(oldMedia);
	expect(await calls(page)).toEqual(before);
	expect(await queue(page)).toEqual(order);
	await field.fill('45');
	await field.press('Enter');
	await expect.poll(() => media(page).then((m) => m.time)).toBe(45);
	expect((await media(page)).writes).toEqual([45]);
	expect(await calls(page)).toEqual(before);
	expect(page.url()).toBe(url);
	await expect(field).toBeFocused();
	await expectSuccess(page);
});

test('GOTO-26 throwing setter reports refusal, never success or a decoder-detail leak', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page, 120, true);
	await requireForm(page, 'GOTO-26');
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const before = await calls(page);
	await submit(page, '60');
	await expectError(page, /cannot|could not|unable|failed|try|unavailable/i);
	expect((await media(page)).time).toBe(30);
	expect((await media(page)).writes).toEqual([60]);
	expect(await calls(page)).toEqual(before);
	expect(errors).toEqual([]);
	await expect(page.getByTestId('player-goto-error')).not.toContainText('private decoder detail');
});

test('GOTO-27 detached audio is unavailable, never mutated through a stale reference', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await requireForm(page, 'GOTO-27');
	const stale = await page.getByTestId('demo-audio').elementHandle();
	expect(stale).not.toBeNull();
	await stale!.evaluate((el) => el.remove());
	const before = await calls(page);
	await submit(page, '60');
	await expectError(page, /media|track|available|ready|load/i);
	expect(await stale!.evaluate((el) => (el as InstrumentedAudio).__gotoWrites)).toEqual([]);
	expect(await calls(page)).toEqual(before);
});

test('GOTO-28 track replacement clears feedback and targets only the new live audio', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await requireForm(page, 'GOTO-28');
	await submit(page, '60');
	await expectSuccess(page);
	const stale = await page.getByTestId('demo-audio').elementHandle();
	expect(stale).not.toBeNull();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect(await stale!.evaluate((el) => el.isConnected)).toBe(false);
	await expect(page.getByTestId('player-goto-status')).not.toContainText(/seek|time|jump|moved/i);
	await expect(page.getByTestId('player-goto-error')).not.toBeVisible();
	await mockMedia(page, 200);
	const before = await calls(page);
	await submit(page, '2:30');
	await expect.poll(() => media(page).then((m) => m.time)).toBe(150);
	expect(await stale!.evaluate((el) => (el as InstrumentedAudio).__gotoWrites)).toEqual([60]);
	await stale!.evaluate((el) => { el.dispatchEvent(new Event('error')); el.dispatchEvent(new Event('loadedmetadata')); });
	await expectSuccess(page);
	expect(await calls(page)).toEqual(before);
});

test('GOTO-29 a track change also clears stale validation feedback', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await requireForm(page, 'GOTO-29');
	await submit(page, 'soon');
	await expectError(page, /time|enter|valid|seconds/i);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-goto-error')).not.toBeVisible();
	await expect(page.getByTestId('player-goto-time')).not.toHaveAttribute('aria-invalid', 'true');
});

test('GOTO-30 empty queue explains unavailable media with no playback or stale mutation', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await requireForm(page, 'GOTO-30');
	const stale = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	const before = await calls(page);
	await submit(page, '60');
	await expectError(page, /media|track|queue|available|select|load/i);
	expect(await stale!.evaluate((el) => (el as InstrumentedAudio).__gotoWrites)).toEqual([]);
	expect(await calls(page)).toEqual(before);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('GOTO-32 submission rechecks a finite duration that shrank after editing', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await requireForm(page, 'GOTO-32');
	await page.getByTestId('player-goto-time').fill('90');
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'duration', { value: 60, configurable: true });
	});
	const before = await calls(page);
	await page.getByTestId('player-goto-time').press('Enter');
	await expectError(page, /duration|range|between|exceed|within|before|maximum|long|end/i);
	expect((await media(page)).writes).toEqual([]);
	expect(await calls(page)).toEqual(before);
});

test('GOTO-33 a source-mismatched bound element is unavailable, never sought', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await requireForm(page, 'GOTO-33');
	await page.getByTestId('demo-audio').evaluate((el) => {
		(el as HTMLAudioElement).src = '/api/demo/audio?track=third-light';
	});
	const before = await calls(page);
	await submit(page, '60');
	await expectError(page, /media|track|available|ready|load/i);
	expect((await media(page)).writes).toEqual([]);
	expect(await calls(page)).toEqual(before);
});

test('GOTO-34 a later refused submission removes an earlier success announcement', async ({ page }) => {
	await openPlayer(page);
	await mockMedia(page);
	await requireForm(page, 'GOTO-34');
	await submit(page, '60');
	await expectSuccess(page);
	const before = await calls(page);
	await submit(page, '121');
	await expectError(page, /duration|range|between|exceed|within|before|maximum|long|end/i);
	expect((await media(page)).time).toBe(60);
	expect((await media(page)).writes).toEqual([60]);
	expect(await calls(page)).toEqual(before);
});

test.describe('GOTO narrow touch and keyboard surface', () => {
	test.use({ viewport: { width: 360, height: 800 }, hasTouch: true });
	for (const colorScheme of ['light', 'dark'] as const) {
		test(`GOTO-31-${colorScheme} controls fit 360px, accept touch and show keyboard focus`, async ({ page }) => {
			await page.emulateMedia({ colorScheme });
			await openPlayer(page);
			await mockMedia(page);
			const field = await requireForm(page, `GOTO-31-${colorScheme}`);
			const button = page.getByTestId('player-goto-seek');
			for (const control of [field, button]) {
				await expect(control).toBeVisible();
				const box = await control.boundingBox();
				expect(box).not.toBeNull();
				expect(box!.x).toBeGreaterThanOrEqual(0);
				expect(box!.x + box!.width).toBeLessThanOrEqual(360);
			}
			await field.tap();
			await field.fill('1:30');
			await button.tap();
			await expect.poll(() => media(page).then((m) => m.time)).toBe(90);
			await expectSuccess(page);
			await field.focus();
			await field.press('Tab');
			await expect(button).toBeFocused();
			const ring = await button.evaluate((el) => {
				const style = getComputedStyle(el);
				return el.matches(':focus-visible') &&
					((style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== 'none');
			});
			expect(ring, 'GOTO: keyboard focus must have a visible indicator').toBe(true);
			await expect(page.getByRole('dialog')).toHaveCount(0);
		});
	}
});
