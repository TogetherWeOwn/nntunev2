import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import {
	fixtureCookie,
	hasFixtureSession,
	expectPlaybackRefusal,
	privateListeningRefusal,
	test
} from './helpers/private-music-fixture.js';

const invited = 'synthetic-listener';
const invitedBinding = 'A'.repeat(43);
const uninvited = 'synthetic-uninvited';
const uninvitedBinding = 'U'.repeat(43);
const track = {
	id: '00000000-0000-4000-8000-000000000001',
	title: 'Synthetic Prelude',
	duration_ms: 14_000
};

test('an invited account selects and plays its Library track while an uninvited account is refused', async ({
	browser,
	page,
	origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac');
	if (!tone) throw new Error('Private music journey requires the checked-in tone-a FLAC');

	const invitedFixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track],
		playableTrackId: (id) => id === track.id ? id : undefined,
		audio: tone.body
	});
	await invitedFixture.signIn();
	await page.goto(origin + library.path);

	const invitedRow = page.getByTestId(library.item).and(page.locator(`[${library.id}="${track.id}"]`));
	await expect(invitedRow).toHaveCount(1);
	await invitedRow.locator(library.select).click();
	await expect(page.getByTestId(player.queueItem)).toHaveAttribute(player.queueId, track.id);
	const grant = page.waitForResponse(origin + player.plays);
	await page.getByRole('region', { name: 'Player', exact: true }).getByRole('button', { name: 'Play', exact: true }).click();
	expect((await grant).status()).toBe(200);
	await expect(page.getByTestId(player.audio)).toHaveAttribute('src', invitedFixture.mediaPath(track.id));
	await expect.poll(() => page.getByTestId(player.audio).evaluate((audio: HTMLAudioElement) =>
		!audio.paused && audio.currentTime > 0.2
	)).toBe(true);

	const deniedContext = await browser.newContext({ serviceWorkers: 'block' });
	try {
		const deniedPage = await deniedContext.newPage();
		await deniedContext.addCookies([{ name: fixtureCookie, value: uninvited, url: origin, httpOnly: true }]);
		await deniedContext.route('**/*', (route) => {
			const request = route.request();
			return new URL(request.url()).origin === origin ? route.continue() : route.abort();
		});
		await deniedPage.route(`${origin}/v1/me/likes/session`, async (route) => {
			if (!hasFixtureSession(await route.request().headerValue('cookie'), uninvited)) {
				return route.fulfill({ status: 401 });
			}
			return route.fulfill({ json: { session_binding: uninvitedBinding } });
		});
		await deniedPage.route(`${origin}${library.api}`, async (route) => {
			if (!hasFixtureSession(await route.request().headerValue('cookie'), uninvited)) {
				return route.fulfill({ status: 401 });
			}
			return route.fulfill({ status: 403, json: privateListeningRefusal });
		});
		await deniedPage.route(`${origin}${player.plays}`, async (route) => {
			const request = route.request();
			const input = request.postDataJSON() as { track_id?: unknown; session_id?: unknown } | null;
			if (!hasFixtureSession(await request.headerValue('cookie'), uninvited) ||
				request.method() !== 'POST' || await request.headerValue('origin') !== origin ||
				await request.headerValue('x-nntune-browser-playback') !== '1' ||
				await request.headerValue('x-nntune-session-binding') !== uninvitedBinding ||
				input?.track_id !== track.id || typeof input.session_id !== 'string' ||
				!/^[0-9a-f-]{36}$/.test(input.session_id)) {
				return route.fulfill({ status: 400 });
			}
			return route.fulfill({ status: 403, json: privateListeningRefusal });
		});
		const refusedListing = deniedPage.waitForResponse(origin + library.api);
		await deniedPage.goto(origin + library.path);
		expect((await refusedListing).status()).toBe(403);
		await expect(deniedPage.getByRole('alert')).toHaveText(library.forbidden);
		await expect(deniedPage.getByTestId(library.item)).toHaveCount(0);
		const refusedGrant = await deniedPage.evaluate(async ({ path, accountBinding }) => {
			const response = await fetch(path, {
				method: 'POST',
				headers: {
					'content-type': 'application/json',
					'x-nntune-browser-playback': '1',
					'x-nntune-session-binding': accountBinding
				},
				body: JSON.stringify({
					track_id: '00000000-0000-4000-8000-000000000001',
					session_id: '00000000-0000-4000-8000-000000000002'
				})
			});
			return { status: response.status, body: await response.json() };
		}, { path: player.plays, accountBinding: uninvitedBinding });
		expect(refusedGrant).toEqual({ status: 403, body: privateListeningRefusal });
	} finally {
		await deniedContext.close();
	}

	const playbackContext = await browser.newContext({ serviceWorkers: 'block' });
	try {
		const playbackPage = await playbackContext.newPage();
		let mediaRequests = 0;
		await playbackContext.addCookies([{ name: fixtureCookie, value: invited, url: origin, httpOnly: true }]);
		await playbackContext.route('**/*', (route) => {
			const request = route.request();
			return new URL(request.url()).origin === origin ? route.continue() : route.abort();
		});
		await playbackPage.route(`${origin}/v1/me/likes/session`, async (route) => {
			if (!hasFixtureSession(await route.request().headerValue('cookie'), invited)) {
				return route.fulfill({ status: 401 });
			}
			return route.fulfill({ json: { session_binding: invitedBinding } });
		});
		await playbackPage.route(`${origin}${library.api}`, async (route) => {
			if (!hasFixtureSession(await route.request().headerValue('cookie'), invited)) {
				return route.fulfill({ status: 401 });
			}
			return route.fulfill({ headers: { 'x-nntune-session-binding': invitedBinding }, json: [track] });
		});
		await playbackPage.route(`${origin}${player.plays}`, async (route) => {
			const request = route.request();
			const input = request.postDataJSON() as { track_id?: unknown; session_id?: unknown } | null;
			if (!hasFixtureSession(await request.headerValue('cookie'), invited) ||
				request.method() !== 'POST' || await request.headerValue('origin') !== origin ||
				await request.headerValue('x-nntune-browser-playback') !== '1' ||
				await request.headerValue('x-nntune-session-binding') !== invitedBinding ||
				input?.track_id !== track.id || typeof input.session_id !== 'string' ||
				!/^[0-9a-f-]{36}$/.test(input.session_id)) {
				return route.fulfill({ status: 400 });
			}
			return route.fulfill({ status: 403, json: privateListeningRefusal });
		});
		await playbackPage.route(`${origin}/v1/plays/media/**`, (route) => {
			mediaRequests += 1;
			return route.fulfill({ status: 404 });
		});

		await playbackPage.goto(origin + library.path);
		const row = playbackPage.getByTestId(library.item).and(playbackPage.locator(`[${library.id}="${track.id}"]`));
		await expect(row).toHaveCount(1);
		await row.locator(library.select).click();
		const refusedGrant = playbackPage.waitForResponse(origin + player.plays);
		await playbackPage.getByRole('region', { name: 'Player', exact: true })
			.getByRole('button', { name: 'Play', exact: true }).click();
		expect((await refusedGrant).status()).toBe(403);
		await expectPlaybackRefusal(playbackPage);
		expect(mediaRequests).toBe(0);
		await expect(playbackPage.locator('audio[src]:not([src=""]), audio source[src]:not([src=""])')).toHaveCount(0);
	} finally {
		await playbackContext.close();
	}
});
