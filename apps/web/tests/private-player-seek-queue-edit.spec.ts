import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

const duration = 14;
const tracks = ['Current tone', 'Removed successor', 'New successor'].map((title, index) => ({
	id: `00000000-0000-4000-8000-00000000110${index + 4}`, title, duration_ms: duration * 1000
}));
const [current, removed, successor] = tracks;
const prefixBytes = 512 * 1024;

function silentWav() {
	const sampleRate = 48_000;
	const bytesPerFrame = 4; // Stereo 16-bit PCM.
	const bytesPerSecond = sampleRate * bytesPerFrame;
	const dataBytes = duration * bytesPerSecond;
	const wav = Buffer.alloc(44 + dataBytes);
	wav.write('RIFF', 0);
	wav.writeUInt32LE(wav.length - 8, 4);
	wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16);
	wav.writeUInt16LE(1, 20);
	wav.writeUInt16LE(2, 22);
	wav.writeUInt32LE(sampleRate, 24);
	wav.writeUInt32LE(bytesPerSecond, 28);
	wav.writeUInt16LE(bytesPerFrame, 32);
	wav.writeUInt16LE(16, 34);
	wav.write('data', 36);
	wav.writeUInt32LE(dataBytes, 40);
	return wav;
}

for (const paused of [false, true]) {
	test(`removing the upcoming track preserves a pending ${paused ? 'paused' : 'playing'} seek and updates Next`, async ({ page, origin }) => {
		const { library, player } = PRIVATE_MUSIC;
		const wav = silentWav();
		const releaseSeek = Promise.withResolvers<void>();
		let seekRead: Request | undefined;
		const grants: unknown[] = [];
		const mediaRequests = new Set<string>();
		const playsUrl = origin + player.plays;
		const fixture = await mockInvitedLibraryPlayback(page, origin, {
			libraryItems: () => tracks,
			playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
			audio: async (route) => {
				const request = route.request();
				const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers()['range'] ?? '');
				const start = range ? Number(range[1]) : 0;
				let end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
				if (start > end) return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${wav.length}` } });
				if (request.url().startsWith(`${playsUrl}/media/${current.id}/`)) {
					if (!range) return route.fulfill({ status: 416 });
					// Hold sequential prefetch so a distant seek must open its own Range request.
					if (start > 0 && start <= prefixBytes) return;
					if (start === 0) end = Math.min(end, prefixBytes - 1);
					else {
						seekRead ??= request;
						await releaseSeek.promise;
					}
				}
				return route.fulfill({ status: range ? 206 : 200, contentType: 'audio/wav', body: wav.subarray(start, end + 1),
					headers: { 'accept-ranges': 'bytes', 'cache-control': 'no-store',
						...(range && { 'content-range': `bytes ${start}-${end}/${wav.length}` }) } });
			}
		});
		page.on('request', (request) => {
			if (request.url() === playsUrl) grants.push(request.postDataJSON());
			if (request.url().startsWith(`${playsUrl}/media/`)) mediaRequests.add(request.url());
		});

		await fixture.signIn();
		await page.goto(origin + library.path);
		for (const { id } of tracks) {
			await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
		}
		const rows = page.getByTestId(player.queueItem);
		const queueOrder = () => rows.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), player.queueId);
		const active = page.locator(player.currentQueueItem);
		const transport = page.getByRole('region', { name: 'Player', exact: true });
		const audio = transport.getByTestId(player.audio);
		const play = transport.getByRole('button', { name: 'Play', exact: true });
		const slider = transport.getByRole('slider', { name: 'Seek', exact: true });
		const status = transport.getByRole('status');
		await expect.poll(queueOrder).toEqual(tracks.map(({ id }) => id));
		await expect(active).toHaveAttribute(player.queueId, current.id);
		const [grant] = await Promise.all([page.waitForResponse(playsUrl), play.click()]);
		expect(grant.status()).toBe(200);
		const { session_id } = grant.request().postDataJSON();
		const initialGrant = { track_id: current.id, session_id };
		const currentSource = origin + fixture.mediaPath(current.id);
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
			!node.paused && node.currentTime > 0.2)).toBe(true);
		await expect(audio).toHaveJSProperty('currentSrc', currentSource);
		const pausedEvent = { type: 'pause', position: expect.any(Number), paused: true, trusted: true, at: expect.any(Number) };
		const native = await audio.elementHandle();
		const events = await native.evaluateHandle((node: HTMLAudioElement) => {
			const changes: { type: string; position: number; paused: boolean; trusted: boolean; at: number }[] = [];
			for (const type of ['seeking', 'seeked', 'play', 'pause', 'emptied', 'loadstart', 'ended']) {
				node.addEventListener(type, (event) => changes.push({ type, position: node.currentTime,
					paused: node.paused, trusted: event.isTrusted, at: performance.now() }));
			}
			return changes;
		});
		if (paused) {
			await transport.getByRole('button', { name: 'Pause', exact: true }).click();
			await expect(status).toHaveText('Paused');
			// The native pause event is queued and can trail the visible status.
			await expect.poll(() => events.jsonValue()).toEqual([pausedEvent]);
		}
		const editedOrder = [current.id, successor.id];
		let target: number;
		let releasedAt: number;
		try {
			expect(seekRead).toBeUndefined();
			await expect(slider).toBeEnabled();
			const { surface, fraction } = await seekViaPlayerUi(page, audio, 0.65);
			expect(surface).toBe('custom');
			await expect.poll(() => seekRead?.url(), 'The seek needs a held read outside the opening prefix').toBe(currentSource);
			const pending = await native.evaluate((node: HTMLAudioElement) => ({
				position: node.currentTime, seeking: node.seeking, paused: node.paused,
				bufferedEnd: node.buffered.end(node.buffered.length - 1)
			}));
			expect(pending).toMatchObject({ seeking: true, paused });
			expect(pending.position).toBeGreaterThan(pending.bufferedEnd);
			expect(Math.abs(pending.position - duration * fraction)).toBeLessThan(duration * 0.05);
			target = pending.position;
			await expect(slider).toHaveAttribute('aria-valuetext', /^Seeking to /);

			await rows.getByRole('button', { name: `Remove ${removed.title} from queue`, exact: true }).click();
			await expect.poll(queueOrder).toEqual(editedOrder);
			await expect(active).toHaveAttribute(player.queueId, current.id);
			expect(await audio.evaluate((node, original) => node === original, native)).toBe(true);
			await expect(audio).toHaveJSProperty('currentSrc', currentSource);
			await expect(audio).toHaveJSProperty('seeking', true);
			await expect(audio).toHaveJSProperty('paused', paused);
			await expect(audio).toHaveJSProperty('currentTime', target);
			await expect(slider).toHaveAttribute('aria-valuetext', /^Seeking to /);
			expect(grants).toEqual([initialGrant]);
			releasedAt = await page.evaluate(() => performance.now());
		} finally {
			releaseSeek.resolve();
		}
		expect((await seekRead!.response())?.status()).toBe(206);
		await expect.poll(() => events.jsonValue()).toEqual([
			...(paused ? [pausedEvent] : []),
			{ type: 'seeking', position: expect.closeTo(target, 1), paused, trusted: true, at: expect.any(Number) },
			{ type: 'seeked', position: paused ? expect.closeTo(target, 1) : expect.any(Number), paused, trusted: true,
				at: expect.any(Number) }
		]);
		// The playing clock can advance before the queued seeked event is delivered, but
		// never further than real time since the held read was released.
		const completion = (await events.jsonValue()).at(-1)!;
		const maxAdvance = (completion.at - releasedAt!) / 1000 + 0.25;
		expect(completion.position).toBeGreaterThanOrEqual(target - 0.05);
		expect(completion.position).toBeLessThanOrEqual(target + (paused ? 0.05 : maxAdvance));
		await expect(slider).not.toHaveAttribute('aria-valuetext', /^Seeking to /);
		await expect(status).toHaveText(paused ? 'Paused' : 'Playing');
		await expect.poll(() => native.evaluate((node: HTMLAudioElement, { target, paused, releasedAt }) =>
			node.isConnected && node.paused === paused && !node.seeking && !node.ended && !node.error &&
			(paused ? Math.abs(node.currentTime - target) < 0.05 : node.currentTime > target + 0.2 &&
				node.currentTime <= target + (performance.now() - releasedAt) / 1000 + 0.25),
		{ target, paused, releasedAt: releasedAt! })).toBe(true);
		await expect(active).toHaveAttribute(player.queueId, current.id);
		await expect(audio).toHaveJSProperty('currentSrc', currentSource);
		expect(grants).toEqual([initialGrant]);
		expect([...mediaRequests]).toEqual([currentSource]);

		await page.getByTestId('player-next').click();
		await expect(active).toHaveAttribute(player.queueId, successor.id);
		await expect.poll(queueOrder).toEqual(editedOrder);
		expect((await events.jsonValue()).some(({ type }) => type === 'ended')).toBe(false);
		if (paused) {
			await expect(status).toHaveText('Paused');
			await expect(audio).toHaveCount(0);
			expect(grants).toEqual([initialGrant]);
			expect([...mediaRequests]).toEqual([currentSource]);
			await play.click();
		}
		const successorSource = origin + fixture.mediaPath(successor.id);
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement, source) =>
			node.currentSrc === source && !node.paused && !node.error && node.currentTime > 0.2, successorSource)).toBe(true);
		expect(grants).toEqual([initialGrant, { track_id: successor.id, session_id }]);
		expect([...mediaRequests]).toEqual([currentSource, successorSource]);
	});
}
