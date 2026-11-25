import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('muting and unmuting a private track keeps native playback moving', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Listening Tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track], playableTrackId: () => track.id, audio: tone.body
	});
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	// Both actions use the stable Mute label. The pressed state distinguishes them.
	const mute = transport.getByRole('button', { name: 'Mute', exact: true, pressed: false });
	const unmute = transport.getByRole('button', { name: 'Mute', exact: true, pressed: true });

	await fixture.signIn();
	await page.goto(origin + library.path);
	await page.getByTestId(library.item).locator(library.select).click();
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && !node.muted && node.currentTime > 0.5)).toBe(true);
	const source = origin + fixture.mediaPath(track.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);

	const playback = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const interruptions: string[] = [];
		for (const type of ['pause', 'play', 'seeking', 'emptied', 'loadstart']) {
			node.addEventListener(type, () => interruptions.push(type));
		}
		return { node, interruptions };
	});
	const expectStillPlaying = async (muted: boolean) => {
		await expect(audio).toHaveJSProperty('muted', muted);
		// Measure in the new mute state so time before the click cannot count.
		const position = await playback.evaluate(({ node }) => node.currentTime);
		await expect.poll(() => playback.evaluate(({ node }, start) => ({
			connected: node.isConnected, source: node.currentSrc, muted: node.muted,
			soundEnabled: !node.muted && node.volume > 0,
			advancing: !node.paused && !node.ended && !node.error && node.currentTime > start + 0.15
		}), position)).toEqual({ connected: true, source, muted, soundEnabled: !muted, advancing: true });
		expect(await playback.evaluate(({ interruptions }) => interruptions)).toEqual([]);
		await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.id);
		await expect(transport.getByTestId('library-now-playing')).toHaveText(track.title);
	};

	await mute.click();
	await expect(unmute).toBeEnabled();
	await expectStillPlaying(true);

	await unmute.click();
	await expect(mute).toBeEnabled();
	await expectStillPlaying(false);
	await playback.dispose();
});
