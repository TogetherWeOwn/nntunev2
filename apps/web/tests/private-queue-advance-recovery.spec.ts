import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('a refused end-of-track grant leaves the queue paused and the successor selectable', async ({ page, origin }) => {
	test.setTimeout(45_000);
	const { library, player } = PRIVATE_MUSIC;
	const [first, second, third] = ['First Tone', 'Second Tone', 'Third Tone'].map((title, index) => ({
		id: `00000000-0000-4000-8000-00000000000${index + 1}`, title, duration_ms: 14_000
	}));
	const tracks = [first, second, third];
	let successorUnavailable = true;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks,
		playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
		audio: privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!.body,
		grantUnavailable: (id) => successorUnavailable && id === second.id
	});
	const grantUrl = origin + player.plays;
	const grants: unknown[] = [];
	const mediaUrls = new Set<string>();
	page.on('request', (request) => {
		if (request.url() === grantUrl) grants.push(request.postDataJSON());
		if (request.url().startsWith(`${grantUrl}/media/`)) mediaUrls.add(request.url());
	});

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const play = transport.getByRole('button', { name: 'Play', exact: true });
	const status = transport.getByRole('status');
	const audio = page.getByTestId(player.audio);
	const rows = page.getByTestId(player.queueItem);
	const active = page.locator(player.currentQueueItem);
	const nowPlaying = transport.getByTestId('library-now-playing');
	const announcement = page.getByTestId('queue-announcement');
	const order = () => rows.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), player.queueId);
	const nativeProgress = () => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && !node.ended && node.error === null && node.currentTime > 0.2);

	await fixture.signIn();
	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`))
			.locator(library.select).click();
	}
	await expect.poll(order).toEqual(tracks.map(({ id }) => id));
	await expect(active).toHaveAttribute(player.queueId, first.id);
	await expect(page.getByLabel('Repeat', { exact: true })).toHaveValue('off');
	await expect(audio).toHaveCount(0);
	expect(grants).toEqual([]);

	// Capture the trusted event before Play. Its timestamp shares the resource timing clock.
	const nativeEnd = await page.evaluateHandle(() => {
		const observed: { at?: number } = {};
		document.addEventListener('ended', (event) => {
			if (event.isTrusted && event.target instanceof HTMLAudioElement && observed.at === undefined) {
				observed.at = event.timeStamp;
			}
		}, true);
		return observed;
	});
	const firstGrant = page.waitForResponse(grantUrl);
	const nextGrant = page.waitForResponse((response) => response.url() === grantUrl &&
		response.request().postDataJSON()?.track_id === second.id, { timeout: 25_000 });
	await play.press('Enter');
	const admitted = await firstGrant;
	expect(admitted.status()).toBe(200);
	const { session_id: tab } = admitted.request().postDataJSON();
	const firstUrl = origin + fixture.mediaPath(first.id);
	await expect(audio).toHaveJSProperty('currentSrc', firstUrl);
	await expect.poll(nativeProgress).toBe(true);

	const refusal = await nextGrant;
	expect(refusal.status()).toBe(503);
	await expect(announcement).toHaveText('Could not start the next track. Choose a track to continue.');
	const { at } = await nativeEnd.jsonValue();
	expect(at).toEqual(expect.any(Number));
	await expect.poll(() => page.evaluate((url) =>
		performance.getEntriesByName(url, 'resource')[1]?.startTime ?? -1, grantUrl
	)).toBeGreaterThanOrEqual(at!);
	await expect(audio).toHaveJSProperty('currentSrc', firstUrl);
	await expect(audio).toHaveJSProperty('ended', true);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(status).toHaveText('Paused');
	await expect(play).toBeEnabled();
	await expect(active).toHaveAttribute(player.queueId, first.id);
	await expect(nowPlaying).toHaveText(first.title);
	await expect.poll(order).toEqual(tracks.map(({ id }) => id));
	const firstRequest = { track_id: first.id, session_id: tab };
	const secondRequest = { track_id: second.id, session_id: tab };
	expect(grants).toEqual([firstRequest, secondRequest]);
	expect([...mediaUrls]).toEqual([firstUrl]);

	// The next successful request must come from an explicit queue choice.
	successorUnavailable = false;
	await rows.and(page.locator(`[${player.queueId}="${second.id}"]`))
		.getByRole('button', { name: `More actions for ${second.title}`, exact: true }).press('Enter');
	await page.getByRole('menuitem', { name: 'Play now', exact: true }).press('Enter');
	const secondUrl = origin + fixture.mediaPath(second.id);
	await expect(audio).toHaveJSProperty('currentSrc', secondUrl);
	await expect.poll(nativeProgress).toBe(true);
	await expect(active).toHaveAttribute(player.queueId, second.id);
	await expect(nowPlaying).toHaveText(second.title);
	await expect(status).toHaveText('Playing');
	await expect(announcement).toHaveText(`Playing ${second.title}.`);
	await expect.poll(order).toEqual(tracks.map(({ id }) => id));
	expect(grants).toEqual([firstRequest, secondRequest, secondRequest]);
	expect([...mediaUrls]).toEqual([firstUrl, secondUrl]);
	await nativeEnd.dispose();
});
