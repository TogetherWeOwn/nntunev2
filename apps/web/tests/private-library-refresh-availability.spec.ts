import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('Refresh reveals a newly playable invited Library match without clearing Find', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const recovered = { id: '00000000-0000-4000-8000-000000000031', title: 'Harbor Dusk', duration_ms: 14_000 };
	const match = { id: '00000000-0000-4000-8000-000000000032', title: 'Harbor Dawn', duration_ms: 14_000 };
	const nonmatch = { id: '00000000-0000-4000-8000-000000000033', title: 'Field Dawn', duration_ms: 14_000 };
	const catalogue = [match, recovered, nonmatch];
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	let sourceReady = false;
	const playableTrackId = (id: string) => catalogue.some((track) =>
		track.id === id && (track.id !== recovered.id || sourceReady)
	) ? id : undefined;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		// The private Library listing admits only tracks with a usable source.
		libraryItems: () => catalogue.filter(({ id }) => playableTrackId(id)),
		playableTrackId,
		audio: tone.body
	});
	const listingRequests: string[] = [];
	const grantRequests: string[] = [];
	const mediaRequests = new Set<string>();
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (url.origin !== origin) return;
		if (url.pathname === library.api) listingRequests.push(`${request.method()} ${url.pathname}${url.search}`);
		if (url.pathname === player.plays) grantRequests.push(request.url());
		if (url.pathname.startsWith(`${player.plays}/media/`)) mediaRequests.add(request.url());
	});

	const main = page.getByRole('main');
	const rows = main.getByTestId(library.item);
	const row = (id: string) => rows.and(main.locator(`[${library.id}="${id}"]`));
	const find = main.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	const scope = main.getByRole('combobox', { name: 'Search in', exact: true });
	const refresh = main.getByRole('button', { name: library.refresh, exact: true });
	const queue = page.getByTestId(player.queueItem);
	const audio = page.getByTestId(player.audio);

	await fixture.signIn();
	await page.goto(origin + library.path);
	await expect(rows).toHaveCount(2);
	await expect(row(match.id)).toBeVisible();
	await expect(row(nonmatch.id)).toBeVisible();
	await expect(row(recovered.id)).toHaveCount(0);
	await scope.selectOption('title');
	await find.fill('harbor');
	await expect(rows).toHaveCount(1);
	await expect(row(match.id)).toBeVisible();
	expect(listingRequests).toEqual([`GET ${library.api}`]);

	sourceReady = true;
	const [listing] = await Promise.all([
		page.waitForResponse(origin + library.api),
		refresh.press('Enter')
	]);
	expect(listing.status()).toBe(200);
	await expect(rows).toHaveCount(2);
	await expect(row(match.id)).toBeVisible();
	await expect(row(nonmatch.id)).toHaveCount(0);
	await expect(find).toHaveValue('harbor');
	await expect(scope).toHaveValue('title');
	await expect(refresh).toBeFocused();
	await expect(queue).toHaveCount(0);
	await expect(audio).toHaveCount(0);
	expect(grantRequests).toEqual([]);

	const actions = row(recovered.id).getByRole('button', { name: `More actions for ${recovered.title}`, exact: true });
	await expect(row(recovered.id).getByRole('button', { name: recovered.title, exact: true })).toBeVisible();
	await expect(actions).toBeVisible();
	// Back-tab from Refresh through the real focus order to the revealed row.
	for (let tabs = 0; tabs < 60 && !(await actions.evaluate((node) => node.matches(':focus'))); tabs++) {
		await page.keyboard.press('Shift+Tab');
	}
	await expect(actions).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByRole('menuitem', { name: 'Play now', exact: true })).toBeFocused();
	const [grant] = await Promise.all([
		page.waitForResponse(origin + player.plays),
		page.keyboard.press('Enter')
	]);
	expect(grant.status()).toBe(200);
	expect(grant.request().method()).toBe('POST');
	expect(grant.request().postDataJSON()).toMatchObject({ track_id: recovered.id });
	await expect(queue).toHaveCount(1);
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, recovered.id);
	const source = origin + fixture.mediaPath(recovered.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	await expect(find).toHaveValue('harbor');
	expect(grantRequests).toEqual([origin + player.plays]);
	expect([...mediaRequests]).toEqual([source]);
	expect(listingRequests).toEqual([`GET ${library.api}`, `GET ${library.api}`]);
});
