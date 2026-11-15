import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { expect, test, type Locator } from '@playwright/test';
// EL TWO-5795 (demo/player retirement): this spec is hosted on the retired
// `/demo/player` surface and the deleted `/api/demo/*` synthetic-media endpoints.
// Vectors and case IDs are preserved; migration to the product surface is
// deferred per fallback (b) — see the PR body disposition manifest.
test.skip(true, 'EL TWO-5795: demo/player retirement — migration deferred, vectors preserved');

import { cc0Track } from '../../../e2e/helpers/cc0-fixture.js';

/**
 * Browser pin: real 24-bit/48 kHz FLAC decode and seek in Chromium against
 * the CC0 fixture (Holizna, "Busted Guitar (JAZZ)", track 1).
 *
 * The demo audio route is intercepted and answers with the real FLAC bytes,
 * honouring Range requests. Skips (never fails) when the R2 fixture store is
 * unconfigured, which is the normal case in CI. Fixture audio is CC0-only per
 * the AGENTS.md rule; see docs/fixture-audio-licences.md.
 */

const fixture = cc0Track(1);
test.skip(fixture.status === 'skipped', fixture.status === 'skipped' ? fixture.reason : '');

const manifest = JSON.parse(
	readFileSync(
		fileURLToPath(
			new URL(
				'../../../fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593/manifest.json',
				import.meta.url
			)
		),
		'utf-8'
	)
) as { tracks: { position: number; duration_s: number; bytes: number }[] };
const track = manifest.tracks.find((entry) => entry.position === 1)!;

async function snapshot(audio: Locator) {
	return audio.evaluate((element) => {
		const player = element as HTMLAudioElement;
		return {
			currentTime: player.currentTime,
			duration: player.duration,
			paused: player.paused,
			errorCode: player.error ? player.error.code : null
		};
	});
}

test('real CC0 FLAC decodes, plays, and seeks mid-track and near the end', async ({ page }) => {
	test.setTimeout(120_000);
	const body = readFileSync((fixture as { path: string }).path);
	expect(body.length, 'fetched fixture size must match the manifest').toBe(track.bytes);

	// Ranged streaming is the behavior under test: count the 206 responses so
	// a silent fall-back to one full download cannot pass this spec.
	let partialResponses = 0;

	await page.route('**/api/demo/audio**', async (route) => {
		const range = route.request().headers()['range'];
		if (!range) {
			await route.fulfill({
				status: 200,
				contentType: 'audio/flac',
				headers: { 'accept-ranges': 'bytes' },
				body
			});
			return;
		}
		const match = /^bytes=(\d+)-(\d*)$/.exec(range);
		const start = match ? Number(match[1]) : Number.NaN;
		const end = match ? (match[2] === '' ? body.length - 1 : Math.min(Number(match[2]), body.length - 1)) : Number.NaN;
		if (!match || start >= body.length || end < start) {
			await route.fulfill({
				status: 416,
				headers: { 'content-range': `bytes */${body.length}` },
				body: ''
			});
			return;
		}
		partialResponses += 1;
		await route.fulfill({
			status: 206,
			contentType: 'audio/flac',
			headers: {
				'accept-ranges': 'bytes',
				'content-range': `bytes ${start}-${end}/${body.length}`
			},
			body: body.subarray(start, end + 1)
		});
	});

	await page.goto('/demo/player');
	await page.waitForFunction(
		() => typeof (window as unknown as { __seekStorm?: unknown }).__seekStorm === 'function'
	);
	const audio = page.getByTestId('demo-audio');
	await expect(audio).toBeVisible();
	await page.getByTestId('queue-select-demo-track').click();

	// Decodes: no error and the duration matches the manifest.
	await expect
		.poll(
			async () => {
				const state = await snapshot(audio);
				if (state.errorCode !== null) return `error:${state.errorCode}`;
				return Number.isFinite(state.duration) &&
					Math.abs(state.duration - track.duration_s) <= 1.0
					? 'ok'
					: `duration=${state.duration}`;
			},
			{ timeout: 30_000 }
		)
		.toBe('ok');

	// Plays: the clock advances while unpaused.
	await audio.evaluate((element) => void (element as HTMLAudioElement).play());
	await expect
		.poll(async () => (await snapshot(audio)).currentTime, { timeout: 30_000 })
		.toBeGreaterThan(0.3);
	const started = await snapshot(audio);
	expect(started.paused).toBe(false);
	expect(started.errorCode).toBeNull();

	for (const target of [60, track.duration_s - 5]) {
		await audio.evaluate((element, time) => {
			const player = element as HTMLAudioElement;
			(window as unknown as { __seeked?: boolean }).__seeked = false;
			player.addEventListener(
				'seeked',
				() => ((window as unknown as { __seeked?: boolean }).__seeked = true),
				{ once: true }
			);
			player.currentTime = time;
		}, target);
		await expect
			.poll(
				() => page.evaluate(() => (window as unknown as { __seeked?: boolean }).__seeked),
				{ timeout: 30_000 }
			)
			.toBe(true);
		const landed = await snapshot(audio);
		expect(landed.errorCode).toBeNull();
		expect(Math.abs(landed.currentTime - target)).toBeLessThanOrEqual(2);

		// Playback must keep advancing after every seek, near-end included:
		// a stall would leave currentTime parked inside the ±2 s window above.
		const after = landed.currentTime;
		await expect
			.poll(async () => (await snapshot(audio)).currentTime, { timeout: 30_000 })
			.toBeGreaterThan(after + 0.3);
		expect((await snapshot(audio)).errorCode).toBeNull();
	}

	expect(partialResponses, 'the player must issue at least one ranged request').toBeGreaterThan(0);
});
