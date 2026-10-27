import { expect, test } from '@playwright/test';
import {
	SOUND_KEY, OTHER_KEY, saved35, openPlayer, media, preference, playCount,
	volumeControl, muteControl, frames, expectSound, pressMute
} from './fixtures';

// Independent Test Author contract. Builder must not edit these tests/fixtures.
// Breaks caught: default-all parser, truthy-zero coercion, implicit unmute/play,
// setup writes that destroy saved state, per-track instead of per-device state,
// stale handlers, write/event feedback, and unguarded platform/storage failures.

for (const vector of [
	{ id: 'SP-PARSE-01', raw: saved35, volume: 0.35, muted: true },
	{ id: 'SP-PARSE-02', raw: '{"version":1,"volume":0,"muted":false}', volume: 0, muted: false },
	{ id: 'SP-PARSE-03', raw: '{"version":1,"volume":0,"muted":true}', volume: 0, muted: true },
	{ id: 'SP-PARSE-04', raw: '{"version":1,"volume":1,"muted":false}', volume: 1, muted: false },
	{ id: 'SP-PARSE-05', raw: '{"version":1,"volume":0.35,"muted":false}', volume: 0.35, muted: false }
]) {
	test(`${vector.id} restores a valid device record before first play`, async ({ page }) => {
		const errors = await openPlayer(page, { saved: vector.raw });
		await expectSound(page, vector.volume, vector.muted);
		expect(await playCount(page)).toBe(0);
		expect((await media(page)).paused).toBe(true);
		await page.getByTestId('queue-select-demo-track').click();
		await expect.poll(() => playCount(page)).toBe(1);
		expect(await page.evaluate(() => window.__soundAcceptance.plays[0])).toEqual({
			volume: vector.volume, muted: vector.muted
		});
		expect(errors).toEqual([]);
	});
}

// Exercise the app's parser through the persisted boundary, not a test-owned
// parser or an import that does not exist yet. Valid and invalid vectors run
// together: ignoring storage entirely cannot pass the matrix.
for (const [index, raw] of [
	undefined, '', '{', 'null', '[]', 'true', '0', '"0.35"',
	'{"version":2,"volume":0.35,"muted":true}',
	'{"volume":0.35,"muted":true}',
	'{"version":"1","volume":0.35,"muted":true}',
	'{"version":1,"muted":true}',
	'{"version":1,"volume":0.35}',
	'{"version":1,"volume":"0.35","muted":true}',
	'{"version":1,"volume":true,"muted":false}',
	'{"version":1,"volume":null,"muted":true}',
	'{"version":1,"volume":{},"muted":true}',
	'{"version":1,"volume":-0.01,"muted":true}',
	'{"version":1,"volume":1.01,"muted":true}',
	'{"version":1,"volume":1e999,"muted":true}',
	'{"version":1,"volume":-1e999,"muted":true}',
	'{"version":1,"volume":NaN,"muted":true}',
	'{"version":1,"volume":0.35,"muted":"false"}',
	'{"version":1,"volume":0.35,"muted":0}',
	'{"version":1,"volume":0.35,"muted":null}'
].entries()) {
	test(`SP-INVALID-${String(index + 1).padStart(2, '0')} defaults safely for ${raw ?? 'missing'}`, async ({ page }) => {
		const errors = await openPlayer(page, { saved: raw });
		await expectSound(page, 1, false);
		expect(await playCount(page)).toBe(0);
		expect(await page.evaluate((key) => window.__soundAcceptance.read(key), OTHER_KEY)).toBe('keep-me');
		// An invalid preference must not prevent transport from working.
		await page.getByTestId('queue-select-demo-track').click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		expect(errors).toEqual([]);
	});
}

test('SP-UI-01 labeled 0–100 volume and pressed mute preserve level and paused intent', async ({ page }) => {
	const errors = await openPlayer(page);
	const volume = volumeControl(page);
	const mute = muteControl(page);
	await expect(volume).toBeVisible();
	await expect(volume).toHaveAttribute('min', '0');
	await expect(volume).toHaveAttribute('max', '100');
	await expect(volume).toHaveValue('100');
	await expect(mute).toHaveAttribute('aria-pressed', 'false');
	expect((await media(page)).controls).toBe(true);

	await volume.fill('35');
	await expectSound(page, 0.35, false);
	await mute.click();
	await expectSound(page, 0.35, true);
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
	// Moving volume while muted must not unmute or start playback.
	await volume.fill('0');
	await expectSound(page, 0, true);
	await volume.fill('35');
	await expectSound(page, 0.35, true);
	await pressMute(page);
	await expectSound(page, 0.35, false);
	await expect(mute).toHaveAttribute('aria-pressed', 'false');
	await pressMute(page);
	await expectSound(page, 0.35, true);
	await mute.click();
	await expectSound(page, 0.35, false);
	expect(await playCount(page)).toBe(0);
	expect((await media(page)).paused).toBe(true);
	expect(errors).toEqual([]);
});

for (const volume of [0, 0.35]) {
	test(`SP-NATIVE-${volume === 0 ? '00' : '35'} native changes serialize only sound and survive a new document`, async ({ page }) => {
		const errors = await openPlayer(page);
		await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement, value) => {
			el.volume = value;
			el.muted = true;
		}, volume);
		await expect.poll(() => preference(page)).toEqual({ version: 1, volume, muted: true });
		await expect(volumeControl(page)).toHaveValue(String(volume * 100));
		await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
		await pressMute(page);
		await expectSound(page, volume, false);
		await expect.poll(() => preference(page)).toEqual({ version: 1, volume, muted: false });
		expect(await playCount(page)).toBe(0);

		// Destroy the player document, retain only device-local storage.
		await page.goto('/');
		await page.goto('/demo/player');
		await expectSound(page, volume, false);
		expect(await playCount(page)).toBe(0);
		await page.reload();
		await expectSound(page, volume, false);
		expect(await playCount(page)).toBe(0);
		expect(await page.evaluate((key) => window.__soundAcceptance.read(key), OTHER_KEY)).toBe('keep-me');
		expect(errors).toEqual([]);
	});
}

test('SP-CUSTOM-RELOAD range and UI mute persist together on ordinary reload', async ({ page }) => {
	await openPlayer(page);
	await expect(volumeControl(page)).toBeVisible();
	await volumeControl(page).fill('35');
	await muteControl(page).click();
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.35, muted: true });
	await page.reload();
	await expectSound(page, 0.35, true);
	await expect(volumeControl(page)).toHaveValue('35');
	await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
	expect(await playCount(page)).toBe(0);
});

for (const volume of [0, 0.35]) {
	test(`SP-TRACK-${volume === 0 ? '00' : '35'} keyed replacements restore volume before their play call`, async ({ page }) => {
		await openPlayer(page);
		await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement, value) => {
			el.volume = value;
			el.muted = true;
		}, volume);
		await frames(page);
		const writesBefore = await page.evaluate(() => window.__soundAcceptance.writes.length);
		const oldAudio = await page.getByTestId('demo-audio').elementHandle();
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
		expect(await oldAudio!.evaluate((el) => el.isConnected)).toBe(false);
		await expectSound(page, volume, true);
		await expect.poll(() => playCount(page)).toBe(1);
		expect(await page.evaluate(() => window.__soundAcceptance.plays[0])).toEqual({ volume, muted: true });
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Third Light');
		await expectSound(page, volume, true);
		await expect.poll(() => playCount(page)).toBe(2);
		expect(await page.evaluate(() => window.__soundAcceptance.plays[1])).toEqual({ volume, muted: true });
		await frames(page);
		expect(await preference(page)).toEqual({ version: 1, volume, muted: true });
		const writes = await page.evaluate(({ key, start }) => window.__soundAcceptance.writes
			.slice(start).filter((write) => write.key === key).map((write) => JSON.parse(write.value)),
			{ key: SOUND_KEY, start: writesBefore });
		for (const value of writes) expect(value).toEqual({ version: 1, volume, muted: true });
	});
}

test('SP-STALE detached native volumechange cannot replace the current preference', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.volume = 0.35;
		el.muted = true;
	});
	await frames(page);
	const oldAudio = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect(await oldAudio!.evaluate((el) => el.isConnected)).toBe(false);
	// Establish the new element's state independently so this checks stale
	// events even when keyed restoration itself is broken.
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.volume = 0.35;
		el.muted = true;
	});
	await frames(page);
	await oldAudio!.evaluate((el: HTMLAudioElement) => {
		el.volume = 0.8;
		el.muted = false;
		el.dispatchEvent(new Event('volumechange'));
	});
	await frames(page);
	await expectSound(page, 0.35, true);
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.35, muted: true });
	await pressMute(page);
	await expectSound(page, 0.35, false);
	expect(await playCount(page)).toBe(1);
});

test('SP-SETUP initial native events must not transiently overwrite a restored record', async ({ page }) => {
	await openPlayer(page, { saved: saved35 });
	await expectSound(page, 0.35, true);
	await frames(page);
	const writes = await page.evaluate((key) => window.__soundAcceptance.writes
		.filter((write) => write.key === key).map((write) => JSON.parse(write.value)), SOUND_KEY);
	// Zero writes on setup is fine; every write that does occur must be the
	// restored value, never volume=1/unmuted from a half-initialized element.
	for (const value of writes) expect(value).toEqual({ version: 1, volume: 0.35, muted: true });
	expect(await preference(page)).toEqual({ version: 1, volume: 0.35, muted: true });
});

test('SP-LOOP native events settle without recursive storage writes or extra play', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => { el.volume = 0.35; });
	await expect.poll(() => preference(page)).toEqual({ version: 1, volume: 0.35, muted: false });
	await page.getByTestId('demo-audio').evaluate((el) => {
		for (let i = 0; i < 20; i += 1) el.dispatchEvent(new Event('volumechange'));
	});
	await frames(page);
	const snapshot = () => page.evaluate((key) => ({
		writes: window.__soundAcceptance.writes.filter((write) => write.key === key).length,
		events: window.__soundAcceptance.events
	}), SOUND_KEY);
	const settled = await snapshot();
	// Permit one write per incoming event plus setup; prohibit amplification.
	expect(settled.writes).toBeLessThanOrEqual(24);
	expect(settled.events).toBeLessThanOrEqual(24);
	await frames(page);
	expect(await snapshot()).toEqual(settled);
	expect(await playCount(page)).toBe(0);
});

for (const storage of ['getter', 'read'] as const) {
	test(`SP-STORAGE-${storage} denied storage falls back without breaking seek, rate or transport`, async ({ page }) => {
		const errors = await openPlayer(page, {
			saved: saved35, storage, url: '/demo/player?track=second-signal'
		});
		await expectSound(page, 1, false);
		await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
		await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => { el.currentTime = 5; });
		await expect.poll(async () => (await media(page)).time).toBe(5);
		await page.getByTestId('playback-speed').selectOption('1.5');
		await expect.poll(async () => (await media(page)).rate).toBe(1.5);
		await pressMute(page);
		await expectSound(page, 1, true);
		expect((await media(page)).time).toBe(5);
		expect(await playCount(page)).toBe(0);
		await page.getByTestId('queue-select-second-signal').click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		expect(errors).toEqual([]);
	});
}

test('SP-STORAGE-write quota failure preserves live level, mute, unrelated storage and transport', async ({ page }) => {
	const errors = await openPlayer(page, { storage: 'write' });
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => { el.volume = 0.35; });
	await frames(page);
	await pressMute(page);
	await expectSound(page, 0.35, true);
	await expect(volumeControl(page)).toHaveValue('35');
	await expect(muteControl(page)).toHaveAttribute('aria-pressed', 'true');
	expect(await page.evaluate((key) => window.__soundAcceptance.writes.some((write) => write.key === key), SOUND_KEY)).toBe(true);
	expect(await page.evaluate((key) => window.__soundAcceptance.read(key), OTHER_KEY)).toBe('keep-me');
	expect(await playCount(page)).toBe(0);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expectSound(page, 0.35, true);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(errors).toEqual([]);
});

test('SP-INDEPENDENT preference changes leave gain, seek, rate and paused intent alone', async ({ page }) => {
	const errors = await openPlayer(page);
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => { el.currentTime = 5; });
	await page.getByTestId('playback-speed').selectOption('1.5');
	await expect.poll(async () => (await media(page)).time).toBe(5);
	const gain = () => page.evaluate(() => (
		window as unknown as { __levelingProbe: () => { enabled: boolean; linear: number } }
	).__levelingProbe());
	expect((await gain()).enabled).toBe(true);
	expect((await gain()).linear).toBeCloseTo(0.7079457843841379, 9);
	await expect(volumeControl(page)).toBeVisible();
	await volumeControl(page).fill('35');
	await muteControl(page).click();
	await pressMute(page);
	await expectSound(page, 0.35, false);
	expect(await media(page)).toMatchObject({ time: 5, rate: 1.5, paused: true });
	expect((await gain()).linear).toBeCloseTo(0.7079457843841379, 9);
	expect(await playCount(page)).toBe(0);
	// Also protect an already-playing intent: sound changes must not pause it
	// or invoke play again. No fake play promise or paused property here.
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await volumeControl(page).fill('0');
	await muteControl(page).click();
	await frames(page);
	expect((await media(page)).paused).toBe(false);
	expect((await media(page)).rate).toBe(1.5);
	expect(await playCount(page)).toBe(1);
	expect((await gain()).linear).toBeCloseTo(0.7079457843841379, 9);
	expect(errors).toEqual([]);
});

for (const volumeSetter of ['throw', 'ignore'] as const) {
	test(`SP-SETTER-${volumeSetter} restricted setter stays truthful and usable (simulation only)`, async ({ page }) => {
		const errors = await openPlayer(page, { saved: saved35, volumeSetter });
		expect((await media(page)).volume).toBe(1);
		const volume = volumeControl(page);
		await expect(volume).toBeVisible();
		// Disabled with an explanation or an operable slider reflecting the
		// actual value are both allowed. Neither may claim the requested 35%.
		if (await volume.isEnabled()) await volume.fill('35');
		else await expect(page.getByText(/(?:device|system|platform).{0,50}volume|volume.{0,50}(?:unavailable|unsupported|device|system)/i).first()).toBeVisible();
		await frames(page);
		expect((await media(page)).volume).toBe(1);
		await expect(volume).toHaveValue('100');
		expect(await page.evaluate(() => window.__soundAcceptance.setterAttempts)).toBeGreaterThan(0);
		await page.getByTestId('queue-select-demo-track').click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		expect(await playCount(page)).toBe(1);
		expect(errors).toEqual([]);
	});
}

test('SP-A11Y keyboard focus and state are visible; arrows never play or unmute', async ({ page }) => {
	await openPlayer(page);
	const volume = volumeControl(page);
	const mute = muteControl(page);
	await expect(volume).toBeVisible();
	await expect(mute).toHaveAttribute('aria-pressed', 'false');
	const seen = new Set<string>();
	for (let i = 0; i < 70 && seen.size < 2; i += 1) {
		await page.keyboard.press('Tab');
		for (const [name, control] of [['volume', volume], ['mute', mute]] as const) {
			if (!await control.evaluate((el) => el === document.activeElement)) continue;
			seen.add(name);
			const visible = await control.evaluate((el) => {
				const style = getComputedStyle(el);
				return el.matches(':focus-visible') &&
					(parseFloat(style.outlineWidth) > 0 || style.boxShadow !== 'none');
			});
			expect(visible, `${name} focus indicator`).toBe(true);
		}
	}
	expect([...seen].sort()).toEqual(['mute', 'volume']);
	await mute.focus();
	await page.keyboard.press('Space');
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
	await volume.focus();
	await page.keyboard.press('End');
	await page.keyboard.press('ArrowLeft');
	const percent = Number(await volume.inputValue());
	expect(percent).toBeLessThan(100);
	expect(percent).toBeGreaterThanOrEqual(0);
	await expectSound(page, percent / 100, true);
	expect(await playCount(page)).toBe(0);
});
