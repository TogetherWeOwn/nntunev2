import { expect, test, type Page } from '@playwright/test';
import { SOUND_KEY, OTHER_KEY, saved35, frames, volumeControl, muteControl } from './sound-preference/fixtures';
import { ITEMS, openLibrarySound, selectTrack, mediaState, nativeSound, preference, soundEffects } from './library-sound-preference.fixture';

// Independent acceptance bank: only the API response/device boundaries are
// fixtures. The actual library page, native PCM media, controls and storage run.
// Breaks caught: ignored recall, truthy-zero defaults, implicit unmute/play,
// optimistic setter results, per-track persistence, stale listeners, feedback
// loops, dishonest storage claims and unusable keyboard/narrow controls.
// Builder must not edit this file or library-sound-preference.fixture.ts.

test.afterEach(async ({}, info) => {
	const caseId = /LIBSOUND-\d+/.exec(info.title)?.[0];
	if (!caseId) return;
	const passed = info.status === info.expectedStatus;
	const error = info.errors.map((entry) => entry.message ?? '').join('\n');
	const infrastructure = /browserType\.launch|error while loading shared libraries|ECONNREFUSED|webServer|Cannot find module/.test(error);
	console.log(JSON.stringify({
		case_id: caseId,
		expected: info.title.replace(/^LIBSOUND-\d+\s*/, ''),
		actual: passed ? 'All behavior assertions satisfied' : error.replace(/\u001b\[[0-9;]*m/g, '').slice(0, 1600),
		status: passed ? 'GREEN' : infrastructure ? 'INFRA_FAILURE' : 'RED',
		error_code: passed ? null : infrastructure ? 'RUNTIME_UNAVAILABLE' : 'LIBRARY_SOUND_CONTRACT',
		next_action: passed ? 'Retain regression' : infrastructure ? 'Use supported runner; not acceptance RED' : 'Builder implements the library sound contract without changing this bank',
	}));
});

function player(page: Page) {
	return page.getByRole('region', { name: 'Player', exact: true });
}

const systemHint = /(?:use|adjust|change|control).{0,60}(?:system|device).{0,30}(?:sound|volume)|(?:system|device).{0,40}(?:sound|volume).{0,40}(?:control|adjust|change)/i;

async function expectSound(page: Page, volume: number, muted: boolean) {
	await expect.poll(async () => {
		const actual = await mediaState(page);
		return { volume: actual.volume, muted: actual.muted };
	}).toEqual({ volume, muted });
}

async function queue(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => ({
		id: row.getAttribute('data-track-id'), current: row.getAttribute('aria-current'),
	})));
}

async function transport(page: Page) {
	const media = await mediaState(page);
	const effects = await soundEffects(page);
	return { src: media.src, time: media.time, rate: media.rate, paused: media.paused,
		plays: effects.plays.length, pause: effects.pause, load: effects.load, queue: await queue(page) };
}

async function expectHealthy(env: Awaited<ReturnType<typeof openLibrarySound>>) {
	expect(env.errors, 'No unhandled page errors').toEqual([]);
	expect(env.unexpected, 'No provider, telemetry, account mutation or extra API requests').toEqual([]);
}

test('LIBSOUND-01 control: fixture reaches the real library and native audio without provider calls', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	expect(await queue(page)).toEqual([{ id: ITEMS[0].id, current: 'true' }]);
	expect(await mediaState(page)).toMatchObject({ controls: true, volume: 1, muted: false, paused: true });
	expect((await soundEffects(page)).plays).toEqual([]);
	await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => audio.play());
	await expect.poll(async () => (await mediaState(page)).paused).toBe(false);
	await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => audio.pause());
	expect(env.api).toEqual(['GET /api/library']);
	await expectHealthy(env);
});

test('LIBSOUND-02 recall is verified before first play and never transiently saves full volume', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: saved35 });
	await expectSound(page, 0.35, true);
	await expect(volumeControl(page)).toHaveValue('35');
	await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
	expect((await mediaState(page)).paused).toBe(true);
	expect(new URL((await mediaState(page)).src!, page.url()).pathname).toBe(ITEMS[0].streamUrl);
	expect(await queue(page)).toEqual([{ id: ITEMS[0].id, current: 'true' }]);
	expect((await soundEffects(page)).plays).toEqual([]);
	expect((await soundEffects(page)).load).toBe(0);
	for (const raw of (await soundEffects(page)).soundWrites) {
		expect(JSON.parse(raw)).toEqual({ version: 1, volume: 0.35, muted: true });
	}
	await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => audio.play());
	expect((await soundEffects(page)).plays).toEqual([{ volume: 0.35, muted: true }]);
	await expectHealthy(env);
});

test('LIBSOUND-03 an intentionally remembered zero is not replaced by a truthy default', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: '{"version":1,"volume":0,"muted":false}' });
	await expectSound(page, 0, false);
	await expect(volumeControl(page)).toHaveValue('0');
	await muteControl(page).click();
	await expectSound(page, 0, true);
	await muteControl(page).click();
	await expectSound(page, 0, false);
	expect((await soundEffects(page)).plays).toEqual([]);
	await expectHealthy(env);
});

test('LIBSOUND-04 custom controls preserve nonzero and zero volume through mute without transport changes', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => {
		audio.currentTime = 5;
		audio.playbackRate = 1.5;
	});
	await expect.poll(async () => (await mediaState(page)).time).toBe(5);
	await frames(page);
	const before = await transport(page);
	const volume = volumeControl(page);
	const mute = muteControl(page);
	await expect(volume).toBeVisible();
	await expect(volume).toHaveAttribute('min', '0');
	await expect(volume).toHaveAttribute('max', '100');
	await expect(mute).toHaveAttribute('aria-pressed', 'false');
	await volume.fill('35');
	await expectSound(page, 0.35, false);
	await expect(player(page)).toContainText(/35\s*%/);
	await mute.click();
	await expectSound(page, 0.35, true);
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
	await volume.fill('0');
	await expectSound(page, 0, true);
	await mute.click();
	await expectSound(page, 0, false);
	await volume.fill('35');
	await mute.click();
	await mute.click();
	await expectSound(page, 0.35, false);
	expect(await transport(page)).toEqual(before);
	expect((await mediaState(page)).controls).toBe(true);
	expect(env.api).toEqual(['GET /api/library']);
	await expectHealthy(env);
});

test('LIBSOUND-05 native volumechange updates custom state and only the canonical record across reload', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	const before = await transport(page);
	await nativeSound(page, 0.42, true);
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.42, muted: true });
	await expect(volumeControl(page)).toHaveValue('42');
	await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
	expect(await transport(page)).toEqual(before);
	await nativeSound(page, 0.42, false);
	await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'false');
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.42, muted: false });
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await selectTrack(page, 0);
	await expectSound(page, 0.42, false);
	await expect(volumeControl(page)).toHaveValue('42');
	expect((await soundEffects(page)).plays).toEqual([]);
	expect(await page.evaluate((key) => window.__librarySoundProbe.read(key), OTHER_KEY)).toBe('keep-me');
	await expectHealthy(env);
});

test('LIBSOUND-06 keyed track replacement recalls the device level before explicit playback', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await nativeSound(page, 0.27, true);
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.27, muted: true });
	const old = await page.getByTestId('audio-player').elementHandle();
	const writesBefore = (await soundEffects(page)).soundWrites.length;
	await selectTrack(page, 1); // Append first; Next is the explicit selection gesture.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	expect(await old!.evaluate((audio) => audio.isConnected)).toBe(false);
	await expectSound(page, 0.27, true);
	await frames(page);
	for (const raw of (await soundEffects(page)).soundWrites.slice(writesBefore)) {
		expect(JSON.parse(raw)).toEqual({ version: 1, volume: 0.27, muted: true });
	}
	// Any automatic play during selection must already have the same level too.
	for (const played of (await soundEffects(page)).plays) expect(played).toEqual({ volume: 0.27, muted: true });
	await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => audio.play());
	expect((await soundEffects(page)).plays.at(-1)).toEqual({ volume: 0.27, muted: true });
	expect((await queue(page)).map((row) => row.id)).toEqual(ITEMS.map((item) => item.id));
	await expectHealthy(env);
});

test('LIBSOUND-07 late detached events cannot replace the current UI, preference or transport', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	const old = await page.getByTestId('audio-player').elementHandle();
	await selectTrack(page, 1);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	expect(await old!.evaluate((audio) => audio.isConnected)).toBe(false);
	await nativeSound(page, 0.31, true); // Independent current-element positive control.
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.31, muted: true });
	const before = await transport(page);
	const writes = (await soundEffects(page)).soundWrites;
	await old!.evaluate((audio: HTMLAudioElement) => {
		audio.volume = 0.9; audio.muted = false;
		audio.dispatchEvent(new Event('volumechange'));
	});
	await frames(page);
	await expectSound(page, 0.31, true);
	await expect(volumeControl(page)).toHaveValue('31');
	await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
	expect(await preference(page)).toEqual({ version: 1, volume: 0.31, muted: true });
	expect((await soundEffects(page)).soundWrites).toEqual(writes);
	expect(await transport(page)).toEqual(before);
	await expectHealthy(env);
});

test('LIBSOUND-08 repeated identical native events settle without write/event or live-message amplification', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await nativeSound(page, 0.35, false);
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.35, muted: false });
	const before = await soundEffects(page);
	await player(page).evaluate((region) => {
		const probe = { changes: 0 };
		(region as HTMLElement & { __soundMutations?: typeof probe }).__soundMutations = probe;
		for (const live of region.querySelectorAll('[role="status"], [role="alert"], [aria-live]')) {
			new MutationObserver((changes) => { probe.changes += changes.length; })
				.observe(live, { childList: true, characterData: true, subtree: true });
		}
	});
	await page.getByTestId('audio-player').evaluate((audio) => {
		for (let i = 0; i < 20; i += 1) audio.dispatchEvent(new Event('volumechange'));
	});
	await frames(page);
	const settled = await soundEffects(page);
	expect(settled.events - before.events).toBe(20);
	expect(settled.writeAttempts - before.writeAttempts).toBeLessThanOrEqual(20);
	expect(settled.volumeWrites).toBe(before.volumeWrites);
	expect(settled.muteWrites).toBe(before.muteWrites);
	await frames(page);
	expect(await soundEffects(page)).toEqual(settled);
	expect(await player(page).evaluate((region) => (region as HTMLElement & { __soundMutations: { changes: number } }).__soundMutations.changes)).toBe(0);
	expect(settled.plays).toEqual(before.plays);
	await expectHealthy(env);
});

for (const [index, storage] of ['getter-blocked', 'read-blocked', 'write-blocked'].entries()) {
	test(`LIBSOUND-${String(9 + index).padStart(2, '0')} ${storage} retains verified session controls without a saved-state claim`, async ({ page }) => {
		const env = await openLibrarySound(page, { raw: storage === 'write-blocked' ? null : saved35,
			storage: storage as 'getter-blocked' | 'read-blocked' | 'write-blocked' });
		await expectSound(page, 1, false);
		await expect(volumeControl(page)).toBeVisible();
		await volumeControl(page).fill('35');
		await muteControl(page).click();
		await expectSound(page, 0.35, true);
		await expect(volumeControl(page)).toHaveValue('35');
		await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
		await expect(player(page).getByText(/^(?:saved|remembered|settings saved)(?:\s|$)/i)).toHaveCount(0);
		expect(await page.evaluate((key) => window.__librarySoundProbe.read(key), SOUND_KEY))
			.toBe(storage === 'write-blocked' ? null : saved35);
		expect(await page.evaluate((key) => window.__librarySoundProbe.read(key), OTHER_KEY)).toBe('keep-me');
		expect((await soundEffects(page)).plays).toEqual([]);
		await selectTrack(page, 1);
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
		await expectSound(page, 0.35, true);
		await expectHealthy(env);
	});
}

for (const [index, raw] of [
	'{', '{"version":2,"volume":0.35,"muted":true}',
	'{"version":1,"volume":1.1,"muted":true}',
	'{"version":1,"volume":0.35,"muted":"false"}',
	'{"version":1,"muted":true}',
].entries()) {
	test(`LIBSOUND-${String(12 + index).padStart(2, '0')} invalid record ${index + 1} defaults atomically without rewriting storage`, async ({ page }) => {
		const env = await openLibrarySound(page, { raw });
		await expectSound(page, 1, false);
		expect(await page.evaluate((key) => window.__librarySoundProbe.read(key), SOUND_KEY)).toBe(raw);
		expect((await soundEffects(page)).soundWrites).toEqual([]);
		expect((await soundEffects(page)).plays).toEqual([]);
		expect(await page.evaluate((key) => window.__librarySoundProbe.read(key), OTHER_KEY)).toBe('keep-me');
		await expectHealthy(env);
	});
}

for (const [index, volume] of ['rejected', 'fixed', 'clamped'].entries()) {
	test(`LIBSOUND-${String(17 + index).padStart(2, '0')} ${volume} setter shows verified actual volume and actionable system controls`, async ({ page }) => {
		const env = await openLibrarySound(page, { raw: '{"version":1,"volume":0.6,"muted":false}',
			volume: volume as 'rejected' | 'fixed' | 'clamped' });
		const actual = volume === 'clamped' ? 0.25 : 1;
		await expectSound(page, actual, false);
		await expect(volumeControl(page)).toHaveValue(String(actual * 100));
		await expect(player(page).getByText(systemHint).first()).toBeVisible();
		expect((await soundEffects(page)).volumeWrites).toBeGreaterThan(0);
		if (await volumeControl(page).isEnabled()) await volumeControl(page).fill('60');
		await expectSound(page, actual, false);
		await expect(volumeControl(page)).toHaveValue(String(actual * 100));
		for (const raw of (await soundEffects(page)).soundWrites) {
			expect(JSON.parse(raw)).toEqual({ version: 1, volume: actual, muted: false });
		}
		await muteControl(page).click();
		await expectSound(page, actual, true);
		await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
		expect((await soundEffects(page)).plays).toEqual([]);
		await expectHealthy(env);
	});
}

test('LIBSOUND-20 a mid-session clamp reports the actual result rather than the requested percent', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await expect(volumeControl(page)).toBeVisible();
	await volumeControl(page).fill('20');
	await expectSound(page, 0.2, false);
	await page.evaluate(() => { window.__librarySoundProbe.volumeMode = 'clamped'; });
	const before = await transport(page);
	await volumeControl(page).fill('80');
	await expectSound(page, 0.25, false);
	await expect(volumeControl(page)).toHaveValue('25');
	await expect(player(page)).toContainText(/25\s*%/);
	await expect(player(page).getByText(systemHint).first()).toBeVisible();
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.25, muted: false });
	expect(await transport(page)).toEqual(before);
	await expectHealthy(env);
});

test('LIBSOUND-21 clearing the queue drops stale element/results but keeps the device preference', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await nativeSound(page, 0.25, true);
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.25, muted: true });
	await page.evaluate(() => { window.__librarySoundProbe.volumeMode = 'clamped'; });
	await volumeControl(page).fill('80');
	await expect(player(page).getByText(systemHint).first()).toBeVisible();
	const old = await page.getByTestId('audio-player').elementHandle();
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(player(page).getByText(systemHint)).toHaveCount(0);
	const writes = (await soundEffects(page)).soundWrites;
	await old!.evaluate((audio: HTMLAudioElement) => {
		audio.volume = 0.1; audio.muted = false; audio.dispatchEvent(new Event('volumechange'));
	});
	await frames(page);
	expect((await soundEffects(page)).soundWrites).toEqual(writes);
	expect(await preference(page)).toEqual({ version: 1, volume: 0.25, muted: true });
	await page.evaluate(() => { window.__librarySoundProbe.volumeMode = 'normal'; });
	await selectTrack(page, 0);
	await expectSound(page, 0.25, true);
	await expect(volumeControl(page)).toBeEnabled();
	await volumeControl(page).fill('60');
	await expectSound(page, 0.6, true);
	await expect(player(page).getByText(systemHint)).toHaveCount(0);
	await expectHealthy(env);
});

test('LIBSOUND-22 a 401 clears account media while retaining only device sound across recovery', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await nativeSound(page, 0.35, true);
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.35, muted: true });
	const old = await page.getByTestId('audio-player').elementHandle();
	env.setSignedOut(true);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	const writes = (await soundEffects(page)).soundWrites;
	await old!.evaluate((audio: HTMLAudioElement) => {
		audio.volume = 0.9; audio.muted = false; audio.dispatchEvent(new Event('volumechange'));
	});
	await frames(page);
	expect((await soundEffects(page)).soundWrites).toEqual(writes);
	expect(await preference(page)).toEqual({ version: 1, volume: 0.35, muted: true });
	env.setSignedOut(false);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await selectTrack(page, 0);
	await expectSound(page, 0.35, true);
	expect(await preference(page)).toEqual({ version: 1, volume: 0.35, muted: true });
	expect((await soundEffects(page)).plays).toEqual([]);
	await expectHealthy(env);
});

for (const [index, colorScheme] of ['light', 'dark'].entries()) {
	test(`LIBSOUND-${23 + index} ${colorScheme} narrow controls support keyboard focus and touch without overflow`, async ({ browser, baseURL }) => {
		const context = await browser.newContext({ baseURL, viewport: { width: 360, height: 800 }, hasTouch: true,
			colorScheme: colorScheme as 'light' | 'dark', reducedMotion: 'reduce' });
		const page = await context.newPage();
		try {
			const env = await openLibrarySound(page, { raw: null });
			const volume = volumeControl(page);
			const mute = muteControl(page);
			await expect(volume).toBeVisible();
			await expect(mute).toBeVisible();
			const reached = new Set<string>();
			for (let i = 0; i < 80 && reached.size < 2; i += 1) {
				await page.keyboard.press('Tab');
				for (const [name, control] of [['volume', volume], ['mute', mute]] as const) {
					if (!await control.evaluate((node) => node === document.activeElement)) continue;
					reached.add(name);
					expect(await control.evaluate((node) => {
						const css = getComputedStyle(node);
						return node.matches(':focus-visible') && (parseFloat(css.outlineWidth) > 0 || css.boxShadow !== 'none');
					}), `${name} needs visible keyboard focus`).toBe(true);
				}
			}
			expect([...reached].sort()).toEqual(['mute', 'volume']);
			await mute.focus();
			await page.keyboard.press('Space');
			await expect(mute).toHaveAttribute('aria-pressed', 'true');
			await volume.focus();
			await page.keyboard.press('End');
			await page.keyboard.press('ArrowLeft');
			const percent = Number(await volume.inputValue());
			expect(percent).toBeLessThan(100);
			await expectSound(page, percent / 100, true);
			await mute.tap();
			await expectSound(page, percent / 100, false);
			for (const control of [volume, mute]) {
				const box = await control.boundingBox();
				expect(box).not.toBeNull();
				expect(box!.x).toBeGreaterThanOrEqual(0);
				expect(box!.x + box!.width).toBeLessThanOrEqual(360);
				expect(box!.width).toBeGreaterThan(0);
				expect(box!.height).toBeGreaterThan(0);
			}
			const sliderBox = await volume.boundingBox();
			await volume.tap({ position: { x: sliderBox!.width / 2, y: sliderBox!.height / 2 } });
			const touchedPercent = Number(await volume.inputValue());
			expect(touchedPercent).toBeGreaterThan(0);
			expect(touchedPercent).toBeLessThan(100);
			await expectSound(page, touchedPercent / 100, false);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
			expect((await mediaState(page)).controls).toBe(true);
			expect((await soundEffects(page)).plays).toEqual([]);
			await expectHealthy(env);
		} finally { await context.close(); }
	});
}

test('LIBSOUND-25 changing sound during real synthetic playback neither restarts nor pauses it', async ({ page }) => {
	const env = await openLibrarySound(page, { raw: null });
	await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => audio.play());
	await expect.poll(async () => (await mediaState(page)).paused).toBe(false);
	const before = await soundEffects(page);
	const beforeQueue = await queue(page);
	const beforeSrc = (await mediaState(page)).src;
	await expect(volumeControl(page)).toBeVisible();
	await volumeControl(page).fill('35');
	await muteControl(page).click();
	await expectSound(page, 0.35, true);
	expect((await mediaState(page)).paused).toBe(false);
	expect((await mediaState(page)).src).toBe(beforeSrc);
	expect(await queue(page)).toEqual(beforeQueue);
	const after = await soundEffects(page);
	expect({ plays: after.plays, pause: after.pause, load: after.load })
		.toEqual({ plays: before.plays, pause: before.pause, load: before.load });
	expect(env.api).toEqual(['GET /api/library']);
	await expectHealthy(env);
});
