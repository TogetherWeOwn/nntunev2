import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { observeMediaSources, test } from './helpers/private-music-fixture.js';

declare global {
	interface Window { __privatePausedNext?: MediaSessionActionHandler }
}

const first = { id: '00000000-0000-4000-8000-000000000001', title: 'First Tone', duration_ms: 14_000 };
const successor = { id: '00000000-0000-4000-8000-000000000002', title: 'Successor Tone', duration_ms: 14_000 };
const tracks = [first, successor];

for (const entry of ['in-app Next', 'Media Session Next Track'] as const) {
	test(`${entry} selects the invited successor without playing and does nothing at the paused tail`, async ({ page, origin }) => {
		const { library, player } = PRIVATE_MUSIC;
		const [tone] = privateStagingAudio();
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
				if (action === 'nexttrack') window.__privatePausedNext = handler ?? undefined;
			};
		});

		const grants: unknown[] = [];
		const mediaRequests: string[] = [];
		const grantUrl = origin + player.plays;
		page.on('request', (request) => {
			if (request.url() === grantUrl) grants.push(request.postDataJSON());
			if (request.url().startsWith(`${grantUrl}/media/`)) mediaRequests.push(request.url());
		});
		const region = page.getByRole('region', { name: 'Player', exact: true });
		const play = region.getByRole('button', { name: 'Play', exact: true });
		const pause = region.getByRole('button', { name: 'Pause', exact: true });
		const status = region.getByRole('status');
		const audio = region.getByTestId(player.audio);
		const active = page.locator(player.currentQueueItem);
		const nowPlaying = region.getByTestId('library-now-playing');
		const queue = page.getByTestId(player.queueItem);
		const order = () => queue.evaluateAll((rows, attr) => rows.map((row) => row.getAttribute(attr)), player.queueId);
		const next = entry === 'in-app Next'
			? () => page.getByTestId('player-next').click()
			: async () => {
				await expect.poll(() => page.evaluate(() => typeof window.__privatePausedNext)).toBe('function');
				await page.evaluate(() => window.__privatePausedNext!({ action: 'nexttrack' }));
			};
		const playThenPause = async (track: typeof first) => {
			const [grant] = await Promise.all([page.waitForResponse(grantUrl), play.click()]);
			expect(grant.status()).toBe(200);
			const path = fixture.mediaPath(track.id);
			await expect.poll(() => audio.evaluate((node: HTMLAudioElement, url) =>
				node.currentSrc === url && !node.paused && !node.ended && !node.error && node.currentTime > 0.2,
				origin + path)).toBe(true);
			await pause.click();
			await expect(status).toHaveText('Paused');
			await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
				node.paused && !node.ended && !node.error && node.networkState === node.NETWORK_IDLE
			)).toBe(true);
			return { path, sessionId: grant.request().postDataJSON().session_id as string };
		};

		await fixture.signIn();
		await page.goto(origin + library.path);
		for (const { id } of tracks) {
			await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
		}
		const sources = await observeMediaSources(page);
		const expectedOrder = tracks.map(({ id }) => id);
		await expect.poll(order).toEqual(expectedOrder);
		await expect(active).toHaveAttribute(player.queueId, first.id);
		const initial = await playThenPause(first);
		const initialGrant = { track_id: first.id, session_id: initial.sessionId };
		const initialMedia = [...mediaRequests];
		expect([...new Set(initialMedia)]).toEqual([origin + initial.path]);
		expect(grants).toEqual([initialGrant]);

		await next();
		await expect(active).toHaveAttribute(player.queueId, successor.id);
		await expect(nowPlaying).toHaveText(successor.title);
		await expect.poll(order).toEqual(expectedOrder);
		await expect(status).toHaveText('Paused');
		await expect(play).toBeEnabled();
		await expect(audio).toHaveCount(0);
		expect(grants).toEqual([initialGrant]);
		expect(mediaRequests).toEqual(initialMedia);
		expect(await sources.evaluate((seen) => [...seen])).toEqual([initial.path]);

		// Explicit Play proves the new selection is playable and starts it in the same tab session.
		const selected = await playThenPause(successor);
		const expectedGrants = [initialGrant, { track_id: successor.id, session_id: initial.sessionId }];
		const tailMedia = [...mediaRequests];
		expect(grants).toEqual(expectedGrants);
		expect([...new Set(tailMedia)]).toEqual([origin + initial.path, origin + selected.path]);
		const tail = await audio.evaluateHandle((node: HTMLAudioElement) => ({ node, position: node.currentTime }));

		await next();
		await expect(active).toHaveAttribute(player.queueId, successor.id);
		await expect(nowPlaying).toHaveText(successor.title);
		await expect.poll(order).toEqual(expectedOrder);
		await expect(status).toHaveText('Paused');
		expect(await audio.evaluate((node: HTMLAudioElement, tail) =>
			node === tail.node && node.paused && node.currentTime === tail.position, tail)).toBe(true);
		await expect(audio).toHaveJSProperty('currentSrc', origin + selected.path);
		expect(grants).toEqual(expectedGrants);
		expect(mediaRequests).toEqual(tailMedia);
		expect(await sources.evaluate((seen) => [...seen])).toEqual([initial.path, selected.path]);
		await tail.dispose();
		await sources.dispose();
	});
}
