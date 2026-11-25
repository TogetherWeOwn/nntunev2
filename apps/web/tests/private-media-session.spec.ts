import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('private playback replaces Media Session metadata on natural advance', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const first = {
		id: '00000000-0000-4000-8000-000000000001', title: 'First Tone', artist: 'First Artist',
		release: 'First Album', duration_ms: 14_000
	};
	const second = {
		id: '00000000-0000-4000-8000-000000000002', title: 'Second Tone', artist: 'Second Artist',
		release: 'Second Album', duration_ms: 14_000
	};
	const tracks = [first, second];
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks,
		playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
		audio: tone.body
	});
	await fixture.signIn();
	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`))
			.locator(library.select).click();
	}

	const audio = page.getByTestId(player.audio);
	const active = page.locator(player.currentQueueItem);
	const sourceFor = (id: string) => new RegExp(`^${player.plays}/media/${id}/`);
	const expectMetadata = (track: typeof first) => expect.poll(() => page.evaluate(() => {
		const metadata = navigator.mediaSession?.metadata;
		return metadata && { title: metadata.title, artist: metadata.artist, album: metadata.album };
	}), `Browser Media Session metadata for ${track.title}`).toEqual({
		title: track.title, artist: track.artist, album: track.release
	});

	await expect(active).toHaveAttribute(player.queueId, first.id);
	await page.getByRole('region', { name: 'Player', exact: true })
		.getByRole('button', { name: 'Play', exact: true }).click();
	await expect(audio).toHaveAttribute('src', sourceFor(first.id));
	await expect(audio).toHaveJSProperty('paused', false);
	await expectMetadata(first);

	// Let the native FLAC finish and advance the queue without a transport action.
	await expect(active).toHaveAttribute(player.queueId, second.id, { timeout: 18_000 });
	await expect(audio).toHaveAttribute('src', sourceFor(second.id));
	await expect(audio).toHaveJSProperty('paused', false);
	await expectMetadata(second);
});
