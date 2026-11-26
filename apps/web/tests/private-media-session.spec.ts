import { expect, type Page } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { playQueuedTrack, test } from './helpers/private-music-fixture.js';

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

declare global {
	interface Window { __invitedLibraryPrevious?: MediaSessionActionHandler }
}

async function openInvitedLibrary(page: Page, origin: string) {
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
	return fixture;
}

test('private playback replaces Media Session metadata on natural advance', async ({ page, origin }) => {
	await openInvitedLibrary(page, origin);
	const audio = page.getByTestId(player.audio);
	const active = page.locator(player.currentQueueItem);
	// Capture startup before clicking Play so the successor can start between assertions.
	const startedSources = await page.evaluateHandle(() => {
		const sources = new WeakMap<HTMLAudioElement, string>();
		document.addEventListener('playing', (event) => {
			if (event.isTrusted && event.target instanceof HTMLAudioElement) {
				sources.set(event.target, event.target.currentSrc);
			}
		}, true);
		return sources;
	});
	const expectPlaying = (track: typeof first) => expect.poll(() => audio.evaluate(
		(node: HTMLAudioElement, { sources, prefix }) =>
			!node.paused && node.currentSrc.startsWith(prefix) && sources.get(node) === node.currentSrc,
		{ sources: startedSources, prefix: `${origin}${player.plays}/media/${track.id}/` }
	), `Native playback started for ${track.title}`).toBe(true);
	const expectMetadata = (track: typeof first) => expect.poll(() => page.evaluate(() => {
		const metadata = navigator.mediaSession?.metadata;
		return metadata && { title: metadata.title, artist: metadata.artist, album: metadata.album };
	}), `Browser Media Session metadata for ${track.title}`).toEqual({
		title: track.title, artist: track.artist, album: track.release
	});

	await expect(active).toHaveAttribute(player.queueId, first.id);
	await page.getByRole('region', { name: 'Player', exact: true })
		.getByRole('button', { name: 'Play', exact: true }).click();
	await expectPlaying(first);
	await expectMetadata(first);

	// Let the native FLAC finish and advance the queue without a transport action.
	await expect(active).toHaveAttribute(player.queueId, second.id, { timeout: 18_000 });
	await expectPlaying(second);
	await expectMetadata(second);
});

test('system Previous switches from the predecessor to restart at three seconds', async ({ page, origin }) => {
	await page.addInitScript(() => {
		const session = navigator.mediaSession;
		const register = session.setActionHandler.bind(session);
		session.setActionHandler = (action, handler) => {
			register(action, handler);
			if (action === 'previoustrack') window.__invitedLibraryPrevious = handler ?? undefined;
		};
	});
	const grants: unknown[] = [];
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) grants.push(request.postDataJSON());
	});
	const fixture = await openInvitedLibrary(page, origin);
	const initial = await playQueuedTrack(page, origin, second.title);
	expect(initial.status()).toBe(200);
	const { session_id: sessionId } = initial.request().postDataJSON();
	const initialGrant = { track_id: second.id, session_id: sessionId };
	const currentSource = origin + fixture.mediaPath(second.id);
	const active = page.locator(player.currentQueueItem);
	const audio = page.getByTestId(player.audio);
	const queueOrder = () => page.getByTestId(player.queueItem).evaluateAll(
		(items, id) => items.map((item) => item.getAttribute(id)), player.queueId
	);
	const expectedOrder = tracks.map(({ id }) => id);
	await expect.poll(queueOrder).toEqual(expectedOrder);
	await expect(active).toHaveAttribute(player.queueId, second.id);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, source) =>
		node.currentSrc === source && !node.paused && node.currentTime > 0.2, currentSource)).toBe(true);
	const original = await audio.elementHandle();
	const previousAt = (target: number) => original.evaluate(async (node: HTMLAudioElement, { target, source }) => {
		// Dispatch in the trusted seeked event's turn to keep playback from crossing the cutoff.
		await new Promise<void>((resolve) => {
			node.addEventListener('seeked', (event) => {
				if (event.isTrusted) resolve();
			}, { once: true });
			node.currentTime = target;
		});
		if (!node.isConnected || node.currentSrc !== source || node.paused || node.ended || node.error) {
			throw new Error('Expected the original source to be playing before system Previous');
		}
		const previous = window.__invitedLibraryPrevious;
		if (!previous) throw new Error('Missing Chromium Media Session Previous callback');
		const before = node.currentTime;
		previous({ action: 'previoustrack' });
		return { before, after: node.currentTime };
	}, { target, source: currentSource });

	const restart = await previousAt(3.25);
	expect(restart.before).toBeGreaterThan(3);
	expect(restart.before).toBeLessThan(3.5);
	expect(restart.after).toBeCloseTo(0, 2);
	await expect.poll(() => original.evaluate((node: HTMLAudioElement, source) =>
		node.isConnected && node.currentSrc === source && !node.paused && !node.ended &&
		!node.error && !node.seeking && node.currentTime > 0.2, currentSource)).toBe(true);
	await expect(active).toHaveAttribute(player.queueId, second.id);
	await expect.poll(queueOrder).toEqual(expectedOrder);
	expect(grants).toEqual([initialGrant]);

	const move = await previousAt(2.75);
	expect(move.before).toBeGreaterThan(2.5);
	expect(move.before).toBeLessThan(3);
	await expect(active).toHaveAttribute(player.queueId, first.id);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, source) =>
		node.currentSrc === source && !node.paused && !node.ended && !node.error && node.currentTime > 0.2,
		origin + fixture.mediaPath(first.id))).toBe(true);
	await expect(audio).toHaveCount(1);
	await expect.poll(queueOrder).toEqual(expectedOrder);
	expect(grants).toEqual([initialGrant, { track_id: first.id, session_id: sessionId }]);
});
