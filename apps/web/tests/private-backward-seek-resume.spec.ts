import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { resumeFloorAfterSeek, seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('private playback resumes from a backward seek made while playing', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Private seek tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track], playableTrackId: () => track.id, audio: tone.body
	});
	const grants: string[] = [];
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) grants.push(request.postDataJSON()?.track_id);
	});
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const play = transport.getByRole('button', { name: 'Play', exact: true });
	const pause = transport.getByRole('button', { name: 'Pause', exact: true });
	const audio = transport.getByTestId(player.audio);

	await fixture.signIn();
	await page.goto(origin + library.path);
	await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
	await play.click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && !node.seeking && node.currentTime > 3.5), {
		message: 'Native playback should advance before the backward seek', timeout: 10_000
	}).toBe(true);
	const source = origin + fixture.mediaPath(track.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	const native = (await audio.elementHandle())!;
	const seek = await native.evaluateHandle((node: HTMLAudioElement) => {
		const observed = { before: node.currentTime, wasPlaying: !node.paused, landed: null as number | null };
		node.addEventListener('seeked', (event) => {
			if (event.isTrusted && !node.paused) observed.landed = node.currentTime;
		}, { once: true });
		return observed;
	});
	const { fraction } = await seekViaPlayerUi(page, audio, 0.12);
	await expect.poll(() => seek.evaluate(({ landed }) => landed), {
		message: 'The native backward seek should finish while playing'
	}).not.toBeNull();
	const { before, wasPlaying, landed } = await seek.jsonValue();
	const duration = track.duration_ms / 1000;
	const target = fraction * duration;
	expect(wasPlaying).toBe(true);
	expect(landed).toBeGreaterThanOrEqual(resumeFloorAfterSeek(target, duration));
	expect(landed).toBeLessThanOrEqual(target + duration * 0.05);
	await pause.click();
	await expect(audio).toHaveJSProperty('paused', true);

	// Observe the first native play event and any timeline changes so a restart
	// cannot catch up to the paused position between polls.
	const resume = await native.evaluateHandle((node: HTMLAudioElement) => {
		const observed = { pausedAt: node.currentTime, startedAt: null as number | null, discontinuities: [] as string[] };
		node.addEventListener('play', () => { observed.startedAt = node.currentTime; }, { once: true });
		for (const event of ['seeking', 'emptied', 'loadstart']) {
			node.addEventListener(event, () => observed.discontinuities.push(event));
		}
		return observed;
	});
	await play.click();
	await expect.poll(() => native.evaluate((node: HTMLAudioElement, { pausedAt, startedAt }) =>
		startedAt !== null && !node.paused && !node.seeking && !node.ended && !node.error &&
		node.currentTime > Math.max(pausedAt, startedAt) + 0.2, resume), {
		message: 'Native playback should advance after resuming from the new position'
	}).toBe(true);
	const { pausedAt, startedAt, discontinuities } = await resume.jsonValue();
	expect(pausedAt, 'Pause should preserve the position reached after the seek').toBeGreaterThanOrEqual(landed! - 0.05);
	expect(pausedAt, 'Pause should capture the timeline after moving backward').toBeLessThan(before);
	expect(startedAt, 'Play should start at the paused position').toBeGreaterThanOrEqual(pausedAt - 0.05);
	expect(startedAt, 'Play should start before the pre-seek position').toBeLessThan(before);
	expect(discontinuities, 'Resume should preserve the native media timeline').toEqual([]);
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.id);
	await expect(transport.getByTestId('library-now-playing')).toHaveText(track.title);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	expect(await audio.evaluate((node, original) => node === original, native)).toBe(true);
	expect(grants, 'The same track should reuse its original playback grant').toEqual([track.id]);
});
