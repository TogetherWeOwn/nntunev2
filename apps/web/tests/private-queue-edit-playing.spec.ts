import { expect, type Page, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

const { library, player } = PRIVATE_MUSIC;
const [earlier, playing, upcoming, later] = [
	{ id: '00000000-0000-4000-8000-000000000011', title: 'Earlier Tone' },
	{ id: '00000000-0000-4000-8000-000000000012', title: 'Playing Tone' },
	{ id: '00000000-0000-4000-8000-000000000013', title: 'Upcoming Tone' },
	{ id: '00000000-0000-4000-8000-000000000014', title: 'Later Tone' }
] as const;
const tracks = [earlier, playing, upcoming, later].map((track) => ({ ...track, duration_ms: 14_000 }));

async function openInvitedQueue(page: Page, origin: string) {
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks, playableTrackId: () => playing.id, audio: tone.body
	});
	const grants: Request[] = [];
	const mediaRequests = new Set<string>();
	page.on('request', (request) => {
		const { pathname } = new URL(request.url());
		if (pathname === player.plays) grants.push(request);
		if (pathname.startsWith(`${player.plays}/media/`)) mediaRequests.add(request.url());
	});
	const rows = page.getByTestId(library.item);
	const queue = page.getByTestId(player.queueItem);
	const order = () => queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), player.queueId);
	const current = page.locator(player.currentQueueItem);
	const audio = page.getByTestId(player.audio);
	const transport = page.getByRole('region', { name: 'Player', exact: true });

	await fixture.signIn();
	await page.goto(origin + library.path);
	await expect(rows).toHaveCount(tracks.length);

	async function startPlayback() {
		for (const { id } of tracks) {
			await rows.and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
		}
		await expect.poll(order).toEqual(tracks.map(({ id }) => id));
		await queue.getByRole('button', { name: `Select ${playing.title}`, exact: true }).click();
		await expect(current).toHaveAttribute(player.queueId, playing.id);
		const [grant] = await Promise.all([
			page.waitForResponse(origin + player.plays),
			transport.getByRole('button', { name: 'Play', exact: true }).click()
		]);
		expect(grant.status()).toBe(200);
		const receipt = fixture.mediaPath(playing.id);
		await expect(audio).toHaveAttribute('src', receipt);
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);

		const playback = await audio.evaluateHandle((node: HTMLAudioElement) => {
			const interruptions: string[] = [];
			for (const type of ['pause', 'emptied', 'loadstart', 'seeking', 'ended', 'error']) {
				node.addEventListener(type, () => interruptions.push(type));
			}
			return { node, position: node.currentTime, interruptions };
		});
		const tab = grant.request().postDataJSON().session_id;

		return async () => {
			await expect(current).toHaveAttribute(player.queueId, playing.id);
			await expect(transport.getByTestId('library-now-playing')).toHaveText(playing.title);
			await expect(audio).toHaveAttribute('src', receipt);
			await expect.poll(() => playback.evaluate(({ node, position, interruptions }) => ({
				connected: node.isConnected,
				source: node.currentSrc,
				advancing: !node.paused && !node.ended && !node.error && node.currentTime > position + 0.1,
				interruptions
			})), 'the original private audio keeps advancing without interruption').toEqual({
				connected: true, source: origin + receipt, advancing: true, interruptions: []
			});
			expect(grants.map((request) => request.postDataJSON())).toEqual([{ track_id: playing.id, session_id: tab }]);
			expect([...mediaRequests]).toEqual([origin + receipt]);
			await playback.dispose();
		};
	}

	return { queue, order, audio, grants, mediaRequests, startPlayback };
}

test('shuffling upcoming private tracks preserves active playback', async ({ page, origin }) => {
	const { order, startPlayback } = await openInvitedQueue(page, origin);
	const expectPlaybackContinues = await startPlayback();

	// Force the two upcoming rows to swap in this test's page.
	await page.evaluate(() => { Math.random = () => 0; });
	await page.getByRole('button', { name: 'Shuffle upcoming', exact: true }).click();

	await expect.poll(order).toEqual([earlier.id, playing.id, later.id, upcoming.id]);
	await expectPlaybackContinues();
});

test('clearing upcoming private tracks preserves active playback', async ({ page, origin }) => {
	const { queue, order, audio, grants, mediaRequests, startPlayback } = await openInvitedQueue(page, origin);
	const clearUpcoming = page.getByRole('button', { name: 'Clear upcoming', exact: true });
	await expect(queue).toHaveCount(0);
	await expect(clearUpcoming).toBeDisabled();
	await expect(audio).toHaveCount(0);
	expect(grants).toHaveLength(0);
	expect(mediaRequests.size).toBe(0);

	const expectPlaybackContinues = await startPlayback();
	await clearUpcoming.click();
	await page.getByTestId('queue-clear-confirm').getByRole('button', { name: 'Yes, clear upcoming', exact: true }).click();

	await expect.poll(order).toEqual([earlier.id, playing.id]);
	await expectPlaybackContinues();
});
