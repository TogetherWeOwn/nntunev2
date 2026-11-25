import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('Play replays the ended final private track with repeat off', async ({ page, origin }) => {
	test.setTimeout(45_000);
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Final Tone', duration_ms: 14_000 };
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
	const audio = page.getByTestId(player.audio);

	await fixture.signIn();
	await page.goto(origin + library.path);
	await page.getByTestId(library.item).locator(library.select).click();
	await expect(page.getByLabel('Repeat', { exact: true })).toHaveValue('off');
	await play.click();
	// Wait for Chromium to finish the FLAC at its normal speed.
	await expect(audio).toHaveJSProperty('ended', true, { timeout: 20_000 });
	const source = origin + fixture.mediaPath(track.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);

	// Sample the native start and observe any reload of the attached decoder.
	const replay = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const observed = { node, startedAt: null as number | null, reloadEvents: [] as string[] };
		node.addEventListener('play', () => { observed.startedAt = node.currentTime; }, { once: true });
		for (const event of ['emptied', 'loadstart']) {
			node.addEventListener(event, () => observed.reloadEvents.push(event));
		}
		return observed;
	});
	await play.click();
	await expect.poll(() => replay.evaluate(({ startedAt }) => startedAt ?? Infinity), {
		message: 'Replay should restart the authorized audio near zero'
	}).toBeLessThan(0.25);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && !node.ended && !node.error && node.currentTime > 0.5), {
		message: 'Native audio should advance after replay'
	}).toBe(true);
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.id);
	await expect(transport.getByTestId('library-now-playing')).toHaveText(track.title);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	expect(await audio.evaluate((node, { node: original }) => node === original, replay)).toBe(true);
	expect(await replay.evaluate(({ reloadEvents }) => reloadEvents), 'Replay should keep the attached decoder loaded').toEqual([]);
	// Play on an attached source reuses its grant, including after native completion.
	expect(grants).toEqual([track.id]);
	await replay.dispose();
});
