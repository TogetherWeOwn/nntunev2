import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

// Playwright disables Chromium's back/forward cache by default.
test.use({ launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] } });

test('Back after sign-out restores Library offline without private playback', async ({ page, context, origin }, info) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Private history tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	let signedOut = false;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track], playableTrackId: () => track.id, audio: tone.body, isRevoked: () => signedOut
	});
	const logoutUrl = `${origin}/v1/session/logout`;
	await page.route(logoutUrl, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || !hasFixtureSession(await request.headerValue('cookie'), 'synthetic-listener')) {
			return route.fulfill({ status: 400 });
		}
		signedOut = true;
		return route.fulfill({
			headers: { 'set-cookie': `${fixtureCookie}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax` },
			json: { ok: true }
		});
	});

	const playRequests: string[] = [];
	const documents: string[] = [];
	page.on('request', (request) => {
		const path = new URL(request.url()).pathname;
		if (path === player.plays || path.startsWith(`${player.plays}/`)) playRequests.push(path);
		if (request.isNavigationRequest() && request.resourceType() === 'document') documents.push(path);
	});

	const rows = page.getByTestId(library.item);
	const queue = page.getByTestId(player.queueItem);
	const nowPlaying = page.getByTestId('library-now-playing');
	const audio = page.getByTestId(player.audio);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const libraryUrl = origin + library.path;
	await fixture.signIn();
	await page.goto(libraryUrl);
	await rows.filter({ hasText: track.title }).locator(library.select).click();
	await expect(queue).toHaveText(new RegExp(track.title));
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	const mediaPath = fixture.mediaPath(track.id);
	await expect(audio).toHaveAttribute('src', mediaPath);
	await expect(nowPlaying).toHaveText(track.title);
	expect(playRequests).toContain(player.plays);
	expect(playRequests).toContain(mediaPath);

	// This listener and its original decoder only exist in the visited Library document.
	await audio.evaluate((decoder: HTMLAudioElement) => {
		const marker = document.documentElement.dataset;
		const markPlayback = () => { marker.historyPlayback = 'started'; };
		addEventListener('pagehide', () => {
			marker.historyPausedOnExit = String(decoder.paused && !decoder.ended);
			decoder.addEventListener('play', markPlayback);
			document.addEventListener('play', markPlayback, { capture: true });
		});
		addEventListener('pageshow', ({ persisted }) => {
			marker.historyRestored = String(persisted);
			marker.historyOffline = String(!navigator.onLine);
			marker.historyPausedOnReturn = String(decoder.paused && !decoder.ended);
		});
	});

	await page.getByRole('button', { name: 'Account menu', exact: true }).click();
	const logout = page.waitForResponse(logoutUrl);
	await page.getByRole('menuitem', { name: 'Sign out', exact: true }).click();
	expect((await logout).status()).toBe(200);
	await expect(page).toHaveURL(`${origin}/login`);
	expect(await context.cookies(origin)).not.toContainEqual(expect.objectContaining({ name: fixtureCookie }));
	const requestsAtSignOut = playRequests.slice();
	expect(documents).toEqual([library.path, '/login']);

	await context.setOffline(true);
	await page.goBack({ waitUntil: 'commit' });
	await expect(page).toHaveURL(libraryUrl);
	const html = page.locator('html');
	await expect(html, 'Back must restore the same Library document from the browser cache').toHaveAttribute('data-history-restored', 'true');
	await expect(html).toHaveAttribute('data-history-offline', 'true');
	await expect(html).toHaveAttribute('data-history-paused-on-exit', 'true');
	await expect(html).toHaveAttribute('data-history-paused-on-return', 'true');
	await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toHaveAttribute('href', '/login');
	await expect(rows).toHaveCount(0);
	await expect(queue).toHaveCount(0);
	await expect(nowPlaying).toHaveCount(0);
	await expect(transport).toHaveCount(0);
	await expect(page.locator('audio, source')).toHaveCount(0);
	await expect(html).not.toHaveAttribute('data-history-playback', 'started');
	expect(documents).toEqual([library.path, '/login']);
	expect(playRequests).toEqual(requestsAtSignOut);
	await info.attach('offline history restoration', {
		contentType: 'application/json',
		body: JSON.stringify({ persisted: true, offline: true, documentRequests: documents, postLogoutPlaybackRequests: [] })
	});
});
