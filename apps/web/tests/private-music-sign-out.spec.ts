import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('sign-out stops active private playback and clears the invited queue', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'First private tone', duration_ms: 14_000 };
	const companion = { id: '00000000-0000-4000-8000-000000000002', title: 'Queued private tone', duration_ms: 14_000 };
	const playId = '00000000-0000-4000-8000-000000000003';
	const firstAccount = 'synthetic-listener';
	const secondAccount = 'synthetic-second-listener';
	const firstBinding = 'A'.repeat(43);
	const secondBinding = 'E'.repeat(43);
	let signedOut = false;
	let mediaPath: string | undefined;
	const playRequests: string[] = [];

	const bindingFor = (cookie: string | null) => {
		if (!signedOut && hasFixtureSession(cookie, firstAccount)) return firstBinding;
		if (hasFixtureSession(cookie, secondAccount)) return secondBinding;
	};
	const seedSession = (value: string) =>
		context.addCookies([{ name: fixtureCookie, value, url: origin, httpOnly: true }]);

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await page.route(`${origin}/v1/me/likes/session`, async (route) => {
		const binding = bindingFor(await route.request().headerValue('cookie'));
		return binding ? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 });
	});
	await page.route(origin + library.api, async (route) => {
		const binding = bindingFor(await route.request().headerValue('cookie'));
		if (!binding) return route.fulfill({ status: 401 });
		// The second invite also sees the played track, so a leaked key would still resolve.
		return route.fulfill({
			headers: { 'x-nntune-session-binding': binding },
			json: binding === firstBinding ? [track, companion] : [track]
		});
	});
	await page.route(origin + player.plays, async (route) => {
		const request = route.request();
		playRequests.push(new URL(request.url()).pathname);
		if (bindingFor(await request.headerValue('cookie')) !== firstBinding) return route.fulfill({ status: 401 });
		if (request.method() !== 'POST') return route.fulfill({ status: 400 });
		const { track_id, session_id, play_id } = request.postDataJSON();
		if (track_id !== track.id || play_id !== undefined ||
			!/^[0-9a-f-]{36}$/.test(session_id) || await request.headerValue('x-nntune-browser-playback') !== '1' ||
			await request.headerValue('x-nntune-session-binding') !== firstBinding) return route.fulfill({ status: 400 });
		mediaPath = `${player.plays}/media/${track.id}/${session_id}/${playId}`;
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: session_id, sessionBinding: firstBinding, playId, durationMs: track.duration_ms
		} });
	});
	await page.route(`${origin}${player.plays}/media/**`, async (route) => {
		const request = route.request();
		playRequests.push(new URL(request.url()).pathname);
		if (request.url() !== origin + mediaPath ||
			bindingFor(await request.headerValue('cookie')) !== firstBinding) return route.fulfill({ status: 404 });
		return serveFlac(route, tone.body);
	});
	const logoutUrl = `${origin}/v1/session/logout`;
	await page.route(logoutUrl, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' ||
			bindingFor(await request.headerValue('cookie')) !== firstBinding) return route.fulfill({ status: 400 });
		signedOut = true;
		return route.fulfill({
			headers: { 'set-cookie': `${fixtureCookie}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax` },
			json: { ok: true }
		});
	});

	const rows = page.getByTestId(library.item);
	const queue = page.getByTestId(player.queueItem);
	const current = page.locator(player.currentQueueItem);
	const nowPlaying = page.getByTestId('library-now-playing');
	const audio = page.getByTestId(player.audio);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const play = transport.getByRole('button', { name: 'Play', exact: true });
	const assertEmptyPlayer = async () => {
		await expect(queue).toHaveCount(0);
		await expect(current).toHaveCount(0);
		await expect(nowPlaying).toHaveCount(0);
		await expect(audio).toHaveCount(0);
		await expect(transport).toHaveCount(0);
	};

	await seedSession(firstAccount);
	await page.goto(origin + library.path);
	for (const item of [track, companion]) await rows.filter({ hasText: item.title }).locator(library.select).click();
	await expect(queue).toHaveText([new RegExp(track.title), new RegExp(companion.title)]);
	await expect(current).toHaveAttribute(player.queueId, track.id);
	await expect(audio).toHaveCount(0);
	await play.click();
	await expect(audio).toHaveAttribute('src', new RegExp(`^${player.plays}/media/${track.id}/`));
	const decoder = (await audio.elementHandle())!;
	await expect.poll(() => decoder.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.currentTime > 0.2)).toBe(true);
	await expect(nowPlaying).toHaveText(track.title);

	// A 204 lets the sign-out redirect finish without unloading the original decoder.
	const loginUrl = `${origin}/login`;
	await page.route(loginUrl, (route) => route.request().isNavigationRequest()
		? route.fulfill({ status: 204 }) : route.fallback(), { times: 1 });
	await page.getByRole('button', { name: 'Account menu', exact: true }).click();
	const [logout, redirect] = await Promise.all([
		page.waitForResponse(logoutUrl),
		page.waitForResponse((response) => response.url() === loginUrl && response.request().isNavigationRequest()),
		page.getByRole('menuitem', { name: 'Sign out', exact: true }).click({ noWaitAfter: true })
	]);
	expect(logout.status()).toBe(200);
	expect(redirect.status()).toBe(204);
	await expect(page).toHaveURL(origin + library.path);
	await expect.poll(() => decoder.evaluate((node: HTMLAudioElement) => ({ paused: node.paused, ended: node.ended })), {
		message: 'Sign-out must pause the original native decoder before the document unloads', timeout: 3_000
	}).toEqual({ paused: true, ended: false });
	await assertEmptyPlayer();
	await expect(rows).toHaveCount(0);
	expect((await context.cookies(origin)).find(({ name }) => name === fixtureCookie)).toBeUndefined();

	const requestsAtSignOut = playRequests.slice();
	// Try the old track's share entry under the signed-out browser session.
	const refused = page.waitForResponse(origin + library.api);
	await page.goto(`${origin}${library.path}?track=${track.id}&t=1`);
	expect((await refused).status()).toBe(401);
	const signIn = page.getByRole('link', { name: 'Sign in', exact: true });
	await expect(signIn).toHaveAttribute('href', '/login');
	await expect(rows).toHaveCount(0);
	await assertEmptyPlayer();
	expect(playRequests).toEqual(requestsAtSignOut);

	await signIn.click();
	await expect(page).toHaveURL(loginUrl);
	await seedSession(secondAccount);
	await page.goto(origin + library.path);
	await expect(rows).toHaveCount(1);
	await expect(rows.first()).toHaveAttribute(library.id, track.id);
	await expect(page.getByTestId('queue-empty-focus')).toHaveText('Queue is empty');
	await assertEmptyPlayer();
	expect(playRequests).toEqual(requestsAtSignOut);
});
