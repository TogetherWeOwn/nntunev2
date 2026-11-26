import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('invited Library keeps its selection title through native pause and restores the prior title on each exit', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Fixture tone', duration_ms: 14_000 };
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track],
		playableTrackId: (id) => id === track.id ? id : undefined,
		audio: privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!.body
	});
	await fixture.signIn();

	const homeTitle = 'Home — NNTune';
	const searchTitle = 'Search — NNTune';
	const selectedTitle = 'Selected: Fixture tone — Library — NNTune';
	const rail = page.getByTestId('shell-rail');
	const libraryLink = rail.getByRole('link', { name: 'Your Library', exact: true });
	const row = page.getByTestId(library.item).locator(library.select);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const play = transport.getByRole('button', { name: 'Play', exact: true });
	const pause = transport.getByRole('button', { name: 'Pause', exact: true });

	await page.goto(origin + '/home');
	await expect(page.locator('html')).toHaveAttribute('data-hydrated', '');
	await expect(page).toHaveTitle(homeTitle);
	await libraryLink.click();
	await expect(page).toHaveURL(origin + library.path);
	await expect(page).toHaveTitle('Library — NNTune');
	await row.click();
	await expect(page).toHaveTitle(selectedTitle);
	await expect(audio).toHaveCount(0);

	// Capture even temporary title writes while keeping the native setter active.
	const writes = await page.evaluateHandle(() => {
		const descriptor = Object.getOwnPropertyDescriptor(Document.prototype, 'title');
		if (!descriptor?.set) throw new Error('Native title setter missing');
		const { set } = descriptor;
		const titles: string[] = [];
		Object.defineProperty(Document.prototype, 'title', {
			...descriptor,
			set(this: Document, title: string) {
				titles.push(title);
				set.call(this, title);
			}
		});
		return titles;
	});

	await play.click();
	const native = (await audio.elementHandle())!;
	await expect.poll(() => native.evaluate((el: HTMLAudioElement) => !el.paused && el.currentTime > 0.2)).toBe(true);
	await expect(audio).toHaveAttribute('src', fixture.mediaPath(track.id));
	await expect(pause).toBeEnabled();
	await expect(page).toHaveTitle(selectedTitle);

	// Interrupt at the native element so the UI has to observe the browser's pause event.
	const pausedAt = await native.evaluate((el: HTMLAudioElement) => {
		el.pause();
		return el.currentTime;
	});
	await expect(play).toBeEnabled();
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(page).toHaveTitle(selectedTitle);
	await play.click();
	await expect.poll(() => native.evaluate((el: HTMLAudioElement, from) =>
		el.isConnected && !el.paused && el.currentTime > from + 0.2, pausedAt)).toBe(true);
	await expect(pause).toBeEnabled();
	await expect(page).toHaveTitle(selectedTitle);
	const playbackTitles = await writes.jsonValue();
	expect(playbackTitles.filter((title) => title !== selectedTitle)).toEqual([]);
	await native.dispose();

	// Each destination has a different title from the one Library captured on entry.
	const leaveLibrary = async (name: string, path: string, priorTitle: string, destinationTitle: string) => {
		await writes.evaluate((titles) => { titles.length = 0; });
		await rail.getByRole('link', { name, exact: true }).click();
		await expect(page).toHaveURL(origin + path);
		await expect(page.getByRole('heading', { name, exact: true, level: 1 })).toBeVisible();
		await expect(page).toHaveTitle(destinationTitle);
		const titles = await writes.jsonValue();
		expect(titles.filter((title) => title === priorTitle)).toEqual([priorTitle]);
		expect(titles.indexOf(priorTitle)).toBeLessThan(titles.indexOf(destinationTitle));
	};

	await leaveLibrary('Search', '/search', homeTitle, searchTitle);
	await libraryLink.click();
	await expect(page).toHaveURL(origin + library.path);
	await expect(row).toBeVisible();
	await expect(page).toHaveTitle(selectedTitle);
	await leaveLibrary('Home', '/home', searchTitle, homeTitle);
	await writes.dispose();
});
