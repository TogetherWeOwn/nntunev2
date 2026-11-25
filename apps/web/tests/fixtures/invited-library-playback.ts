import { expect, type Page, type Route } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, serveFlac } from '../helpers/private-music-fixture.js';

const listener = 'synthetic-listener';
const binding = 'A'.repeat(43);
const defaultPlayId = `private_browser_${'a'.repeat(22)}`;

type LibraryTrack = { id: string; title: string; duration_ms: number };

export async function mockInvitedLibraryPlayback(page: Page, origin: string, fixture: {
	libraryItems: () => LibraryTrack[];
	playableTrackId: (requestedId: string) => string | undefined;
	playIdFor?: (trackId: string) => string;
	audio: Buffer | ((route: Route) => Promise<void>);
	grantUnavailable?: (trackId: string) => boolean;
	isRevoked?: () => boolean;
}) {
	const context = page.context();
	const isListener = (cookie: string | null) => hasFixtureSession(cookie, listener);
	const playIdFor = fixture.playIdFor ?? (() => defaultPlayId);
	let playback: { trackId: string; sessionId: string } | undefined;
	const mediaPath = (trackId: string) => `/v1/plays/media/${trackId}/${playback?.sessionId}/${playIdFor(trackId)}`;
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
		const input = call.postDataJSON();
		const trackId = typeof input?.track_id === 'string' ? fixture.playableTrackId(input.track_id) : undefined;
		const track = fixture.libraryItems().find(({ id }) => id === trackId);
		if (call.method() !== 'POST' || !trackId || !track || input.track_id !== trackId ||
			await call.headerValue('x-nntune-browser-playback') !== '1' || await call.headerValue('x-nntune-session-binding') !== binding) {
			return route.fulfill({ status: 400 });
		}
		expect(input.session_id).toMatch(/^[0-9a-f-]{36}$/);
		const playId = playIdFor(trackId);
		if (input.play_id !== undefined &&
			(!playback || input.play_id !== playId || trackId !== playback.trackId || input.session_id !== playback.sessionId)) {
			return route.fulfill({ status: 409 });
		}
		if (fixture.grantUnavailable?.(trackId)) return route.fulfill({ status: 503 });
		playback = { trackId, sessionId: input.session_id };
		return route.fulfill({ json: { trackId, playbackSessionId: playback.sessionId, sessionBinding: binding, playId, durationMs: track.duration_ms } });
	});
	await page.route(`${origin}/v1/plays/media/**`, async (route) => {
		const call = route.request();
		if (!isListener(await call.headerValue('cookie')) || fixture.isRevoked?.()) {
			return route.fulfill({ status: 404 });
		}
		const trackId = playback?.trackId;
		if (!trackId || fixture.playableTrackId(trackId) !== trackId || call.url() !== `${origin}${mediaPath(trackId)}`) {
			return route.fulfill({ status: 404 });
		}
		return typeof fixture.audio === 'function' ? fixture.audio(route) : serveFlac(route, fixture.audio);
	});

	return {
		signIn: () => context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]),
		mediaPath
	};
}
