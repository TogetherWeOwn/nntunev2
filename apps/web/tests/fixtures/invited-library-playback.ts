import { expect, type Page } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, serveFlac } from '../helpers/private-music-fixture.js';

const listener = 'synthetic-listener';
const binding = 'A'.repeat(43);
const playId = '00000000-0000-4000-8000-000000000003';

type LibraryTrack = { id: string; title: string; duration_ms: number };

export async function mockInvitedLibraryPlayback(page: Page, origin: string, fixture: {
	libraryItems: () => LibraryTrack[];
	playableTrackId: () => string | undefined;
	audio: Buffer;
	isRevoked?: () => boolean;
}) {
	const context = page.context();
	const isListener = (cookie: string | null) => hasFixtureSession(cookie, listener);
	let playbackSession: string | undefined;
	const mediaPath = (trackId: string) => `/v1/plays/media/${trackId}/${playbackSession}/${playId}`;
	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await page.route(`${origin}/v1/me/likes/session`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		if (fixture.isRevoked?.()) return route.fulfill({ status: 403 });
		return route.fulfill({ json: { session_binding: binding } });
	});
	await page.route(`${origin}${PRIVATE_MUSIC.library.api}`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		if (fixture.isRevoked?.()) return route.fulfill({ status: 403 });
		return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: fixture.libraryItems() });
	});
	await page.route(`${origin}${PRIVATE_MUSIC.player.plays}`, async (route) => {
		const call = route.request();
		if (!isListener(await call.headerValue('cookie')) || fixture.isRevoked?.()) return route.fulfill({ status: 403 });
		const trackId = fixture.playableTrackId();
		const input = call.postDataJSON();
		if (call.method() !== 'POST' || !trackId || input.track_id !== trackId ||
			await call.headerValue('x-nntune-browser-playback') !== '1' || await call.headerValue('x-nntune-session-binding') !== binding) {
			return route.fulfill({ status: 400 });
		}
		expect(input.session_id).toMatch(/^[0-9a-f-]{36}$/);
		playbackSession = input.session_id;
		return route.fulfill({ json: { trackId, playbackSessionId: playbackSession, sessionBinding: binding, playId, durationMs: 14_000 } });
	});
	await page.route(`${origin}/v1/plays/media/**`, async (route) => {
		const call = route.request();
		const trackId = fixture.playableTrackId();
		if (!isListener(await call.headerValue('cookie')) || fixture.isRevoked?.() || !trackId || !playbackSession ||
			call.url() !== `${origin}${mediaPath(trackId)}`) {
			return route.fulfill({ status: 404 });
		}
		return serveFlac(route, fixture.audio);
	});

	return {
		signIn: () => context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]),
		mediaPath
	};
}
