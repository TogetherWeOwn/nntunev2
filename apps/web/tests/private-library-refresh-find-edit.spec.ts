import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('editing Find during an invited Library refresh keeps focus and playback', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const playing = { id: '00000000-0000-4000-8000-000000000031', title: 'Harbor Tone', duration_ms: 14_000 };
	const initialMatch = { ...playing, id: '00000000-0000-4000-8000-000000000032', title: 'Field Dawn' };
	const refreshedMatch = { ...initialMatch, id: '00000000-0000-4000-8000-000000000033', title: 'Field Dusk' };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const refreshStarted = Promise.withResolvers<void>();
	const releaseRefresh = Promise.withResolvers<void>();
	let listing = [playing, initialMatch];
	let listingReads = 0;
	let grants = 0;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => listing,
		playableTrackId: (id) => id === playing.id ? id : undefined,
		audio: tone.body
	});
	await page.route(origin + library.api, async (route) => {
		if (++listingReads === 2) {
			refreshStarted.resolve();
			await releaseRefresh.promise;
			listing = [playing, refreshedMatch];
		}
		await route.fallback(); // The shared fixture supplies the session-bound response.
	});
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) grants += 1;
	});

	const main = page.getByRole('main');
	const rows = main.getByTestId(library.item);
	const row = ({ id }: typeof playing) => rows.and(page.locator(`[${library.id}="${id}"]`));
	const find = main.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	const refresh = main.getByRole('button', { name: library.refresh, exact: true });
	const loading = main.getByRole('status').filter({ hasText: 'Loading your library…' });
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);

	await fixture.signIn();
	await page.goto(origin + library.path);
	await expect(rows).toHaveCount(2);
	await find.fill('harbor');
	await expect(rows).toHaveCount(1);
	await row(playing).getByRole('button', { name: playing.title, exact: true }).click();
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.readyState >= node.HAVE_FUTURE_DATA && node.currentTime > 0.2
	)).toBe(true);
	const source = origin + fixture.mediaPath(playing.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	const playback = await audio.evaluateHandle((node: HTMLAudioElement) => {
		node.playbackRate = 0.25; // Keep the short fixture tone running while the reply is held.
		const interruptions: string[] = [];
		for (const event of ['pause', 'waiting', 'seeking', 'emptied', 'loadstart', 'ended', 'error']) {
			node.addEventListener(event, () => interruptions.push(event));
		}
		return { node, interruptions };
	});
	expect(listingReads).toBe(1);

	try {
		await refresh.click();
		await refreshStarted.promise;
		await expect(loading).toBeVisible();
		await find.click();
		await page.keyboard.press('ControlOrMeta+A');
		await page.keyboard.type('field');
		await expect(find).toHaveValue('field');
		await expect(find).toBeFocused();
		await expect(row(initialMatch)).toBeVisible();
		await expect(rows).toHaveCount(1);
		await expect(loading).toBeVisible();
	} finally {
		releaseRefresh.resolve();
	}

	await expect(row(refreshedMatch).getByRole('button', { name: refreshedMatch.title, exact: true })).toBeVisible();
	await expect(rows).toHaveCount(1);
	await expect(loading).toBeHidden();
	await expect(find).toHaveValue('field');
	await expect(find).toBeFocused();
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, playing.id);
	const position = await playback.evaluate(({ node }) => node.currentTime);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, { original, position }) => ({
		sameElement: node === original.node,
		source: node.currentSrc,
		advancing: !node.paused && node.currentTime > position + 0.1,
		interruptions: original.interruptions
	}), { original: playback, position })).toEqual({ sameElement: true, source, advancing: true, interruptions: [] });
	await playback.dispose();
	expect(grants).toBe(1);
	expect(listingReads).toBe(2);
});
