import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

const track = { id: '00000000-0000-4000-8000-000000001103', title: 'Interrupted seek audio', duration_ms: 14_000 };
const duration = track.duration_ms / 1000;
const sampleRate = 48_000;
const bytesPerFrame = 4; // Two 16-bit PCM channels.
const bytesPerSecond = sampleRate * bytesPerFrame;
const prefixBytes = 512 * 1024;

function silentWav() {
	const dataBytes = duration * bytesPerSecond;
	const wav = Buffer.alloc(44 + dataBytes);
	wav.write('RIFF', 0);
	wav.writeUInt32LE(wav.length - 8, 4);
	wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); // PCM format chunk length.
	wav.writeUInt16LE(1, 20); // PCM format code.
	wav.writeUInt16LE(2, 22);
	wav.writeUInt32LE(sampleRate, 24);
	wav.writeUInt32LE(bytesPerSecond, 28);
	wav.writeUInt16LE(bytesPerFrame, 32);
	wav.writeUInt16LE(16, 34);
	wav.write('data', 36);
	wav.writeUInt32LE(dataBytes, 40);
	return wav;
}

test('private playback retries an interrupted seek from the requested position', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const playsUrl = origin + player.plays;
	const body = silentWav();
	const plays: unknown[] = [];
	const mediaUrls = new Set<string>();
	const releaseInterruption = Promise.withResolvers<void>();
	let receiptConfirmed = false;
	let interrupted: Request | undefined;
	let refused: Request | undefined;
	let recovered: Request | undefined;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track],
		playableTrackId: (requested) => requested === track.id ? track.id : undefined,
		audio: async (route) => {
			const request = route.request();
			const range = /^bytes=(\d+)-$/.exec(request.headers()['range'] ?? '');
			const start = range ? Number(range[1]) : NaN;
			if (!Number.isSafeInteger(start) || start >= body.length) return route.fulfill({ status: 416 });

			// Let metadata and the opening seconds decode. Keep the sequential
			// prefetch pending so the slider has to issue a distant byte read.
			if (start > 0 && start <= prefixBytes) return;
			if (start > prefixBytes && !receiptConfirmed) {
				if (!interrupted) {
					interrupted = request;
					await releaseInterruption.promise;
					return route.abort('connectionreset');
				}
				// Chromium reissues a reset range. Keep the relay unavailable until
				// the user can exercise the existing explicit Retry control.
				refused ??= request;
				return route.fulfill({ status: 503 });
			}
			if (start > prefixBytes) recovered ??= request;
			const end = start === 0 ? prefixBytes - 1 : body.length - 1;
			return route.fulfill({
				status: 206, contentType: 'audio/wav', body: body.subarray(start, end + 1),
				headers: { 'accept-ranges': 'bytes', 'cache-control': 'no-store', 'content-range': `bytes ${start}-${end}/${body.length}` }
			});
		}
	});
	page.on('request', (request) => {
		if (request.url() === playsUrl) plays.push(request.postDataJSON());
		if (request.url().startsWith(`${playsUrl}/media/`)) mediaUrls.add(request.url());
	});

	await fixture.signIn();
	await page.goto(origin + library.path);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const status = transport.getByRole('status');
	const retry = transport.getByRole('button', { name: 'Retry playback', exact: true });
	const currentTrack = page.locator(player.currentQueueItem);
	await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
	await expect(audio).toHaveCount(0);
	expect(plays).toEqual([]);
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.currentTime > 0.2)).toBe(true);
	const source = origin + fixture.mediaPath(track.id);
	const [sessionId, playId] = source.split('/').slice(-2);
	const admission = { track_id: track.id, session_id: sessionId };
	const continuation = { ...admission, play_id: playId };
	page.on('response', (response) => {
		if (response.url() === playsUrl && response.ok() && response.request().postDataJSON()?.play_id === playId) {
			receiptConfirmed = true;
		}
	});
	await expect(audio).toHaveJSProperty('currentSrc', source);
	const native = (await audio.elementHandle())!;
	let target: number;
	try {
		const { surface, fraction } = await seekViaPlayerUi(page, audio, 0.8);
		expect(surface).toBe('custom');
		await expect.poll(() => interrupted, { message: 'Seeking should request audio beyond the opening prefix' }).toBeDefined();
		const pending = await native.evaluate((node: HTMLAudioElement) => ({
			position: node.currentTime, seeking: node.seeking, paused: node.paused,
			bufferedEnd: node.buffered.end(node.buffered.length - 1)
		}));
		expect(pending).toMatchObject({ seeking: true, paused: false });
		expect(pending.position).toBeGreaterThan(pending.bufferedEnd);
		expect(Math.abs(pending.position - duration * fraction)).toBeLessThan(duration * 0.05);
		target = pending.position;
	} finally {
		releaseInterruption.resolve();
	}
	await expect.poll(() => interrupted?.failure()?.errorText).toBe('net::ERR_CONNECTION_RESET');
	await expect(retry).toBeEnabled();
	await expect(status).toContainText('Retry playback');
	expect(await (await refused?.response())?.status()).toBe(503);
	expect(refused?.headers()['range']).toBe(interrupted?.headers()['range']);
	await expect(currentTrack).toHaveAttribute(player.queueId, track.id);
	expect(plays).toEqual([admission]);

	const resumed = await native.evaluateHandle((node: HTMLAudioElement) => {
		const observation = { position: null as number | null, trusted: false };
		node.addEventListener('playing', (event) => {
			observation.position = node.currentTime;
			observation.trusted = event.isTrusted;
		}, { once: true });
		return observation;
	});
	await retry.click();
	await expect(status).toHaveText('Playing');
	await expect.poll(() => native.evaluate((node: HTMLAudioElement, observation) =>
		observation.position !== null && !node.paused && !node.seeking && node.error === null &&
			node.currentTime > observation.position + 0.2, resumed)).toBe(true);
	const firstPlaying = await resumed.jsonValue();
	expect(firstPlaying.trusted).toBe(true);
	expect(firstPlaying.position).not.toBeNull();
	expect(Math.abs(firstPlaying.position! - target)).toBeLessThan(0.1);
	await expect(retry).toHaveCount(0);
	await expect(currentTrack).toHaveAttribute(player.queueId, track.id);
	expect(await audio.evaluate((node, original) => node === original, native)).toBe(true);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	expect(await (await recovered?.response())?.status()).toBe(206);
	expect(recovered?.headers()['range']).toBe(interrupted?.headers()['range']);
	expect(mediaUrls).toEqual(new Set([source]));
	expect(plays, 'Retry should confirm the original receipt without another admission').toEqual([admission, continuation]);
});
