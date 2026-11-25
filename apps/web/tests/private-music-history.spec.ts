import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

// The headless shell cannot restore this document. Use full Chromium's headless
// mode and remove Playwright's default flag that disables the back/forward cache.
test.use({ channel: 'chromium', launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] } });

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
	await audio.evaluate((decoder: HTMLAudioElement, protectedContent) => {
		const marker = document.documentElement.dataset;
		const markPlayback = (event: Event) => { marker.historyPlayback = event.type; };
		addEventListener('pagehide', () => {
			marker.historyPausedOnExit = String(decoder.paused && !decoder.ended);
			for (const event of ['play', 'loadstart']) {
				decoder.addEventListener(event, markPlayback);
				document.addEventListener(event, markPlayback, { capture: true });
			}
		}, { once: true });
		addEventListener('pageshow', ({ persisted }) => {
			marker.historyRestored = String(persisted);
			marker.historyPausedOnReturn = String(decoder.paused && !decoder.ended);
			marker.historyProtectedOnReturn = String(!!document.querySelector(protectedContent));
		});
	}, `[data-testid="${library.item}"], [data-testid="${player.queueItem}"], audio`);

	await page.getByRole('button', { name: 'Account menu', exact: true }).click();
	const logout = page.waitForResponse(logoutUrl);
	await page.getByRole('menuitem', { name: 'Sign out', exact: true }).click();
	expect((await logout).status()).toBe(200);
	await expect(page).toHaveURL(`${origin}/login`);
	expect(await context.cookies(origin)).not.toContainEqual(expect.objectContaining({ name: fixtureCookie }));
	const requestsBeforeBack = playRequests.length;
	expect(documents).toEqual([library.path, '/login']);

	await context.setOffline(true);
	await page.goBack({ waitUntil: 'commit' });
	await expect(page).toHaveURL(libraryUrl);
	const html = page.locator('html');
	await expect(html, 'Back must restore the same Library document from the browser cache').toHaveAttribute('data-history-restored', 'true');
	// The restored document can retain a stale navigator.onLine value. Check the
	// actual network boundary using an uncached request to the loopback server.
	const [offlineProbe] = await Promise.all([
		page.waitForEvent('requestfailed', { predicate: (request) => request.url() === `${origin}/api/build`, timeout: 5_000 }),
		page.evaluate(() => fetch('/api/build', { cache: 'no-store' }).catch(() => null))
	]);
	const offlineFailure = offlineProbe.failure()?.errorText;
	expect(offlineFailure).toBe('net::ERR_INTERNET_DISCONNECTED');
	await expect(html).toHaveAttribute('data-history-paused-on-exit', 'true');
	await expect(html).toHaveAttribute('data-history-paused-on-return', 'true');
	await expect(html, 'The restored document must have no private rows, queued tracks or audio').toHaveAttribute('data-history-protected-on-return', 'false');
	await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toHaveAttribute('href', '/login');
	await expect(rows).toHaveCount(0);
	await expect(queue).toHaveCount(0);
	await expect(nowPlaying).toHaveCount(0);
	await expect(transport).toHaveCount(0);
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await html.getAttribute('data-history-playback')).toBeNull();
	expect(documents).toEqual([library.path, '/login']);
	const playbackRequestsAfterBack = playRequests.slice(requestsBeforeBack);
	expect(playbackRequestsAfterBack).toEqual([]);
	await info.attach('offline history restoration', {
		contentType: 'application/json',
		body: JSON.stringify({ persisted: true, offlineFailure, documentRequests: documents, playbackRequestsAfterBack })
	});
});
