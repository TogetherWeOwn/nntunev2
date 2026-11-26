import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { observeMediaSources, test } from './helpers/private-music-fixture.js';

declare global {
	interface Window { __invitedLibraryNext?: MediaSessionActionHandler }
}

test('system Next plays the reordered private successor and stays at the edited tail', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const [first, upcoming, later] = ['First Tone', 'Upcoming Tone', 'Later Tone'].map((title, index) => ({
		id: `00000000-0000-4000-8000-00000000000${index + 1}`, title, duration_ms: 14_000
	}));
	const tracks = [first, upcoming, later];
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks,
		playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
		audio: tone.body
	});
	await page.addInitScript(() => {
		const session = navigator.mediaSession;
		const register = session.setActionHandler.bind(session);
		session.setActionHandler = (action, handler) => {
			register(action, handler);
			if (action === 'nexttrack') window.__invitedLibraryNext = handler ?? undefined;
		};
	});
	const next = () => page.evaluate(() => {
		const handler = window.__invitedLibraryNext;
		if (!handler) throw new Error('Missing Chromium Media Session Next callback');
		handler({ action: 'nexttrack' });
	});
	const grants: unknown[] = [];
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) grants.push(request.postDataJSON());
	});
	await fixture.signIn();
	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	const queue = page.getByTestId(player.queueItem);
	const row = (id: string) => queue.and(page.locator(`[${player.queueId}="${id}"]`));
	const active = page.locator(player.currentQueueItem);
	const order = () => queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), player.queueId);
	const audio = page.getByTestId(player.audio);
	const expectPlaying = (path: string) => expect.poll(() => audio.evaluate((node: HTMLAudioElement, source) =>
		node.currentSrc === source && !node.paused && !node.ended && !node.error && node.currentTime > 0.2,
		origin + path), `Native playback advances on ${path}`).toBe(true);
	const mediaSources = await observeMediaSources(page);
	await expect.poll(order).toEqual(tracks.map(({ id }) => id));
	await expect(active).toHaveAttribute(player.queueId, first.id);
	const [initial] = await Promise.all([
		page.waitForResponse(origin + player.plays),
		page.getByRole('region', { name: 'Player', exact: true }).getByRole('button', { name: 'Play', exact: true }).click()
	]);
	expect(initial.status()).toBe(200);
	const { session_id: sessionId } = initial.request().postDataJSON();
	const initialGrant = { track_id: first.id, session_id: sessionId };
	const firstSource = fixture.mediaPath(first.id);
	await expectPlaying(firstSource);
	// A natural end must not satisfy the explicit Next assertion.
	const completion = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const state = { ended: node.ended };
		node.addEventListener('ended', () => { state.ended = true; }, { once: true });
		return state;
	});

	await row(later.id).getByRole('button', { name: `Move ${later.title} up`, exact: true }).click();
	const reordered = [first.id, later.id, upcoming.id];
	await expect.poll(order).toEqual(reordered);
	await expect(active).toHaveAttribute(player.queueId, first.id);
	await expectPlaying(firstSource);
	expect(grants).toEqual([initialGrant]);
	await next();
	await expect(active).toHaveAttribute(player.queueId, later.id);
	const expectedGrants = [initialGrant, { track_id: later.id, session_id: sessionId }];
	await expect.poll(() => grants, 'System Next requests only the reordered successor').toEqual(expectedGrants);
	const successorSource = fixture.mediaPath(later.id);
	await expectPlaying(successorSource);
	expect(await completion.jsonValue()).toEqual({ ended: false });
	await expect.poll(order).toEqual(reordered);

	await page.getByTestId('player-repeat').selectOption('queue');
	await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: Queue');
	await row(upcoming.id).getByRole('button', { name: `Remove ${upcoming.title} from queue`, exact: true }).click();
	const tail = [first.id, later.id];
	await expect.poll(order).toEqual(tail);
	const successor = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const interruptions: string[] = [];
		for (const type of ['pause', 'emptied', 'loadstart', 'seeking', 'ended']) {
			node.addEventListener(type, () => interruptions.push(type));
		}
		return { node, position: node.currentTime, interruptions };
	});
	await next();
	await expect.poll(() => successor.evaluate(({ node, position, interruptions }, source) => ({
		advancing: node.isConnected && node.currentSrc === source && !node.paused && !node.ended &&
			!node.error && node.currentTime > position + 0.1,
		interruptions
	}), origin + successorSource)).toEqual({ advancing: true, interruptions: [] });
	await expect(active).toHaveAttribute(player.queueId, later.id);
	await expect.poll(order).toEqual(tail);
	await expect(audio).toHaveCount(1);
	expect(grants).toEqual(expectedGrants);
	expect(await mediaSources.evaluate((sources) => [...sources])).toEqual([firstSource, successorSource]);
});
