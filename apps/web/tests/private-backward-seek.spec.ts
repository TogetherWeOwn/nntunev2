import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { resumeFloorAfterSeek, seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('invited playback continues after a backward seek across the first chunk boundary', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Private seek tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	// In the manifest-verified FLAC, byte 65,536 falls inside the frame at byte 65,080
	// (ffprobe: 1,261 bytes, 4.702041s to 4.806531s). Observe outside that frame on each side.
	expect(tone.sha256, 'The chunk boundary timing is pinned to this FLAC digest')
		.toBe('c935d3368a1f2f22ca87b8ccfa6b9facae070e6e07be1e16abff513835a5f259');
	const boundaryFrame = { start: 4.702041, end: 4.806531 };
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track], playableTrackId: () => track.id, audio: tone.body
	});
	const grants: string[] = [];
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) grants.push(request.method());
	});

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	await fixture.signIn();
	await page.goto(origin + library.path);
	await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, end) =>
		!node.paused && !node.seeking && node.currentTime > end, boundaryFrame.end), {
		message: 'Native playback should reach the far side of the boundary', timeout: 10_000
	}).toBe(true);

	const source = origin + fixture.mediaPath(track.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	const native = (await audio.elementHandle())!;
	const observation = await native.evaluateHandle((node: HTMLAudioElement) => {
		const observed = {
			before: { time: node.currentTime, paused: node.paused },
			landed: null as { time: number; paused: boolean; trusted: boolean } | null,
			interruptions: [] as string[]
		};
		node.addEventListener('seeked', (event) => {
			observed.landed = { time: node.currentTime, paused: node.paused, trusted: event.isTrusted };
		}, { once: true });
		for (const event of ['pause', 'play', 'loadstart', 'emptied']) {
			node.addEventListener(event, () => observed.interruptions.push(event));
		}
		return observed;
	});

	const duration = track.duration_ms / 1000;
	const { fraction, surface } = await seekViaPlayerUi(page, audio, 0.2);
	expect(surface, 'The backward seek should use the custom slider').toBe('custom');
	await expect.poll(() => native.evaluate((node: HTMLAudioElement, { observed, boundary }) =>
		observed.landed !== null && !node.paused && !node.seeking &&
		node.currentTime > observed.landed.time + 0.2 && node.currentTime < boundary,
	{ observed: observation, boundary: boundaryFrame.start }), {
		message: 'Native playback should advance from the seek on the earlier side of the boundary'
	}).toBe(true);
	const { before, landed, interruptions } = await observation.jsonValue();
	expect(before.paused).toBe(false);
	expect(before.time).toBeGreaterThan(boundaryFrame.end);
	expect(landed).toMatchObject({ paused: false, trusted: true });
	const target = fraction * duration;
	expect(landed!.time).toBeGreaterThanOrEqual(resumeFloorAfterSeek(target, duration));
	expect(landed!.time).toBeLessThanOrEqual(target + duration * 0.05);
	expect(interruptions).toEqual([]);
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.id);
	await expect(transport.getByTestId('library-now-playing')).toHaveText(track.title);
	await expect(audio).toHaveCount(1);
	expect(await audio.evaluate((node: HTMLAudioElement, original) => ({
		sameNode: node === original, source: node.currentSrc, error: node.error?.code ?? null
	}), native)).toEqual({ sameNode: true, source, error: null });
	expect(grants, 'The backward seek should reuse the original playback grant').toEqual(['POST']);
});
