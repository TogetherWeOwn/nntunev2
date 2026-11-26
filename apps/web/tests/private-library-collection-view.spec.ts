import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('keyboard collection view changes preserve focus and invited Library playback', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const [playing, outsideQuery, outsideSelection] = ['Harbor Tone', 'Meadow Tone', 'Harbor Bell'].map((title, index) => ({
		id: `00000000-0000-4000-8000-00000000000${index + 1}`, title, duration_ms: 14_000
	}));
	const albums = ['Field East', 'Field West', 'Valley'].map((title, index) => ({
		mbid: `10000000-0000-4000-8000-00000000000${index + 1}`, title, release_type: 'Album'
	}));
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	let listingReads = 0;
	let collectionReads = 0;
	let grants = 0;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => { listingReads++; return [playing, outsideQuery, outsideSelection]; },
		playableTrackId: (id) => id === playing.id ? id : undefined,
		audio: tone.body
	});
	await page.route(`${origin}/v1/releases`, async (route) => {
		const request = route.request();
		if (request.method() !== 'GET' || !hasFixtureSession(await request.headerValue('cookie'), 'synthetic-listener')) {
			return route.fulfill({ status: 403 });
		}
		collectionReads++;
		return route.fulfill({ json: albums });
	});
	page.on('request', (request) => { if (request.url() === origin + player.plays) grants++; });

	const rows = page.getByTestId(library.item);
	const row = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`));
	const checkbox = ({ id, title }: typeof playing) => row(id).getByRole('checkbox', { name: `Select ${title}`, exact: true });
	const find = page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	const show = page.getByRole('combobox', { name: 'Show', exact: true });
	const types = page.getByRole('group', { name: 'Library type', exact: true });
	const albumsButton = types.getByRole('button', { name: 'Albums', exact: true });
	const collections = page.getByRole('region', { name: 'Library collections', exact: true });
	const collectionSearch = collections.getByRole('searchbox', { name: 'Search in Your Library', exact: true });
	const reverse = collections.getByRole('button', { name: 'Reverse order', exact: true });
	const view = collections.getByRole('combobox', { name: 'Collection view', exact: true });
	const list = collections.getByRole('list');
	const current = page.locator(player.currentQueueItem);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);

	await fixture.signIn();
	await page.goto(origin + library.path);
	await expect(rows).toHaveCount(3);
	await checkbox(playing).check();
	await checkbox(outsideQuery).check();
	await show.selectOption('selected');
	await find.fill('harbor');
	await expect(rows).toHaveCount(1);
	await row(playing.id).getByRole('button', { name: playing.title, exact: true }).click();
	const [grant] = await Promise.all([
		page.waitForResponse(origin + player.plays),
		transport.getByRole('button', { name: 'Play', exact: true }).click()
	]);
	expect(grant.status()).toBe(200);
	const source = origin + fixture.mediaPath(playing.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	// Give the short synthetic tone time to play through the keyboard round trip.
	await audio.evaluate((node: HTMLAudioElement) => { node.playbackRate = 0.25; });
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.readyState >= node.HAVE_FUTURE_DATA && node.currentTime > 0.2
	)).toBe(true);
	await expect(current).toHaveAttribute(player.queueId, playing.id);
	const playback = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const interruptions: string[] = [];
		for (const event of ['pause', 'waiting', 'seeking', 'emptied', 'loadstart', 'ended', 'error']) {
			node.addEventListener(event, () => interruptions.push(event));
		}
		return { node, interruptions };
	});

	await albumsButton.press('Enter');
	await expect(list.getByRole('listitem')).toHaveCount(3);
	await expect(row(playing.id)).toBeHidden();
	await collectionSearch.fill('field');
	await reverse.press('Enter');
	await page.keyboard.press('Tab');
	await expect(view).toBeFocused();
	await expect(view).toHaveValue('list');
	await expect(list).toHaveCSS('display', 'block');

	for (const { key, mode, display } of [
		{ key: 'ArrowDown', mode: 'grid', display: 'grid' },
		{ key: 'ArrowUp', mode: 'list', display: 'block' }
	]) {
		await page.keyboard.press(key);
		await expect(view).toHaveValue(mode);
		await expect(list).toHaveCSS('display', display);
		await expect(view).toBeFocused();
		await page.keyboard.press('Shift+Tab');
		await expect(reverse).toBeFocused();
		await page.keyboard.press('Tab');
		await expect(view).toBeFocused();
		await expect(albumsButton).toHaveAttribute('aria-pressed', 'true');
		await expect(collectionSearch).toHaveValue('field');
		await expect(reverse).toHaveAttribute('aria-pressed', 'true');
		await expect(list.getByRole('listitem')).toHaveText([/Field West/, /Field East/]);
		await expect(current).toHaveAttribute(player.queueId, playing.id);
		await page.keyboard.press('Tab');
		await expect(view).not.toBeFocused();
		await page.keyboard.press('Shift+Tab');
		await expect(view).toBeFocused();
	}

	await types.getByRole('button', { name: 'Tracks', exact: true }).press('Enter');
	await expect(show).toHaveValue('selected');
	await expect(find).toHaveValue('harbor');
	await expect(rows).toHaveCount(1);
	await expect(checkbox(playing)).toBeChecked();
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await expect(current).toHaveAttribute(player.queueId, playing.id);
	const position = await playback.evaluate(({ node }) => node.currentTime);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, { original, position }) => ({
		sameElement: node === original.node,
		source: node.currentSrc,
		advancing: !node.paused && node.currentTime > position + 0.1,
		interruptions: original.interruptions
	}), { original: playback, position })).toEqual({ sameElement: true, source, advancing: true, interruptions: [] });
	await playback.dispose();
	expect({ listingReads, collectionReads, grants }).toEqual({ listingReads: 1, collectionReads: 1, grants: 1 });
});
