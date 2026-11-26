import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test.use({ viewport: { width: 390, height: 844 } });

test('reopening Now Playing keeps the private track playing on its original grant', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Private navigation tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track], playableTrackId: () => track.id, audio: tone.body
	});
	const grants: { method: string; trackId: string }[] = [];
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) {
			grants.push({ method: request.method(), trackId: request.postDataJSON()?.track_id });
		}
	});

	const audio = page.getByTestId(player.audio);
	const current = page.locator(player.currentQueueItem);
	const main = page.getByRole('main');
	const mini = page.getByTestId('mini-player').getByRole('link');

	await fixture.signIn();
	await page.goto(origin + library.path);
	await page.getByRole('button', { name: 'Play displayed from the top', exact: true }).click();
	await expect(current).toHaveAttribute(player.queueId, track.id);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.currentTime > 1
	)).toBe(true);
	const source = origin + fixture.mediaPath(track.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);

	const playback = await audio.evaluateHandle((node: HTMLAudioElement) => {
		// Catch brief interruptions even if playback has recovered by the next assertion.
		const interruptions: string[] = [];
		for (const event of ['pause', 'seeking', 'emptied', 'loadstart', 'waiting']) {
			node.addEventListener(event, () => interruptions.push(event));
		}
		return { node, interruptions };
	});
	const position = () => playback.evaluate(({ node }) => node.currentTime);
	const expectContinuedPlayback = async () => {
		// The caller has reached the destination. Only count progress from this point.
		const arrivedAt = await position();
		await expect.poll(position).toBeGreaterThan(arrivedAt + 0.1);
		expect(await audio.evaluate((node: HTMLAudioElement, playback) => ({
			sameNode: node === playback.node,
			source: node.currentSrc,
			playing: !node.paused && !node.ended && !node.error && !node.seeking,
			interruptions: playback.interruptions
		}), playback)).toEqual({ sameNode: true, source, playing: true, interruptions: [] });
		expect(grants).toEqual([{ method: 'POST', trackId: track.id }]);
	};
	const openNowPlaying = async () => {
		await expect(mini).toContainText(track.title);
		await mini.click();
		await expect(page).toHaveURL(origin + '/now-playing');
		await expect(main.getByRole('heading', { name: track.title, exact: true })).toBeVisible();
		await expectContinuedPlayback();
	};

	await openNowPlaying();
	await main.getByRole('button', { name: 'Back', exact: true }).click();
	await expect(page).toHaveURL(origin + library.path);
	await expect(current).toHaveAttribute(player.queueId, track.id);
	await expectContinuedPlayback();
	await openNowPlaying();
	await playback.dispose();
});
