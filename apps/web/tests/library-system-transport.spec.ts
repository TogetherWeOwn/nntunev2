import { expect, test, type Page } from '@playwright/test';
import { expectedSyntheticBody } from '../../../e2e/helpers/demo-audio.js';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { playQueuedTrack, serveRangedAudio, test as signedInTest } from './helpers/private-music-fixture.js';

type Probe = {
	handlers: Record<string, (() => void) | null>;
	retired: Record<string, (() => void) | null>;
	retiredPlays: number;
};
declare global {
	interface Window { __librarySystemTransport: Probe; }
}

const coral = {
	id: 'system-coral', title: 'System Coral', artist: 'Fixture Artist', release: 'Fixture Album',
	streamUrl: '/api/demo/audio?track=coral'
};

async function recordSystemControls(page: Page) {
	await page.addInitScript(() => {
		const handlers: Probe['handlers'] = {};
		window.__librarySystemTransport = { handlers, retired: {}, retiredPlays: 0 };
		Object.defineProperty(navigator, 'mediaSession', {
			value: {
				metadata: null,
				setActionHandler(action: string, handler: (() => void) | null) { handlers[action] = handler; }
			}, configurable: true
		});
	});
}

async function rememberSystemControls(page: Page) {
	await page.evaluate(() => {
		const probe = window.__librarySystemTransport;
		probe.retired = { ...probe.handlers };
	});
}

async function openWithSystemControls(page: Page, items = [coral]) {
	await recordSystemControls(page);
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	await page.goto(`/library?track=${coral.id}&t=2`);
	await expect(page.getByTestId('audio-player')).toBeVisible();
	await expect.poll(() => page.evaluate(() => {
		const metadata = navigator.mediaSession.metadata;
		return [metadata?.title, metadata?.artist, metadata?.album];
	})).toEqual([coral.title, coral.artist, coral.release]);
	await rememberSystemControls(page);
}

const systemAction = (page: Page, action: 'play' | 'pause' | 'nexttrack' | 'previoustrack') =>
	page.evaluate((key) => window.__librarySystemTransport.handlers[key]!(), action);

test('system Play and Pause control the same native decoder after leaving Library', async ({ page }) => {
	await openWithSystemControls(page);
	const native = page.locator('audio[controls][data-testid="audio-player"]');
	const audio = (await native.elementHandle())!;
	const time = () => audio.evaluate((el: HTMLAudioElement) => el.currentTime);
	await expect.poll(time).toBeCloseTo(2);
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	await expect(native).toBeVisible();
	expect(await audio.evaluate((el: HTMLAudioElement) => el === document.querySelector('audio') && el.controls && el.paused)).toBe(true);
	const pausedAt = await time();
	expect(pausedAt).toBeCloseTo(2);
	const playedAt = await audio.evaluate((el: HTMLAudioElement) => new Promise<number>((resolve) => {
		el.addEventListener('play', () => resolve(el.currentTime), { once: true });
		window.__librarySystemTransport.handlers.play!();
	}));
	expect(Math.abs(playedAt - pausedAt)).toBeLessThan(0.25);
	await expect(player.getByRole('status')).toHaveText('Playing');
	await expect.poll(time).toBeGreaterThan(pausedAt + 0.1);
	await systemAction(page, 'pause');
	await expect(player.getByRole('status')).toHaveText('Paused');
	expect(await audio.evaluate((el: HTMLAudioElement) => el === document.querySelector('audio[controls]') && el.paused)).toBe(true);
	await expect(page.locator('audio')).toHaveCount(1);

	// Exercise the native media path independently of both sets of app actions.
	await audio.evaluate((el: HTMLAudioElement) => el.play());
	await expect(player.getByRole('status')).toHaveText('Playing');
	await audio.evaluate((el: HTMLAudioElement) => el.pause());
	await expect(player.getByRole('status')).toHaveText('Paused');

	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
	await expect(native).toHaveCount(0);
	expect(await page.evaluate(() => {
		const { play, pause } = window.__librarySystemTransport.handlers;
		return [navigator.mediaSession.metadata, play, pause];
	})).toEqual([null, null, null]);
});

test('system Previous and Next navigate the persistent queue without autoplay', async ({ page }) => {
	const second = { ...coral, id: 'system-second', title: 'System Second', streamUrl: '/api/demo/audio?track=second-signal' };
	await openWithSystemControls(page, [coral, second]);
	await page.getByTestId('library-item').and(page.locator(`[data-id="${second.id}"]`)).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('library-list')).toHaveCount(0);

	const native = page.getByTestId('audio-player');
	const firstAudio = (await native.elementHandle())!;
	await systemAction(page, 'previoustrack');
	expect(await firstAudio.evaluate((el) => el.isConnected && (el as HTMLAudioElement).paused)).toBe(true);

	await systemAction(page, 'nexttrack');
	await expect(native).toHaveAttribute('src', second.streamUrl);
	await expect(native).toHaveJSProperty('paused', true);
	const title = () => page.evaluate(() => navigator.mediaSession.metadata?.title);
	await expect.poll(title).toBe(second.title);
	const secondAudio = (await native.elementHandle())!;
	await page.evaluate(() => window.__librarySystemTransport.retired.previoustrack!());
	await expect(native).toHaveAttribute('src', second.streamUrl);

	await systemAction(page, 'play');
	const status = page.getByRole('region', { name: 'Player', exact: true }).getByRole('status');
	await expect(status).toHaveText('Playing');
	await systemAction(page, 'nexttrack');
	expect(await secondAudio.evaluate((el) => el.isConnected && !(el as HTMLAudioElement).paused)).toBe(true);

	await systemAction(page, 'previoustrack');
	await expect(native).toHaveAttribute('src', coral.streamUrl);
	await expect(status).toHaveText('Paused');
	await expect(native).toHaveJSProperty('paused', true);
	expect(await secondAudio.evaluate((el) => !el.isConnected && (el as HTMLAudioElement).paused)).toBe(true);
	await expect.poll(title).toBe(coral.title);
	await expect(page.locator('audio')).toHaveCount(1);

	const afterPurge = await page.evaluate(() => {
		const { handlers } = window.__librarySystemTransport;
		const { nexttrack, previoustrack } = handlers;
		window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' }));
		nexttrack!();
		previoustrack!();
		return [navigator.mediaSession.metadata, handlers.nexttrack, handlers.previoustrack];
	});
	expect(afterPurge).toEqual([null, null, null]);
	await expect(native).toHaveCount(0);
});

test('system Next resolves a bulk-added source and keeps the live decoder for an unavailable slot', async ({ page }) => {
	const second = { ...coral, id: 'system-second', title: 'System Second', streamUrl: '/api/demo/audio?track=second-signal' };
	const unavailable = { ...coral, id: 'system-unavailable', title: 'System Unavailable', streamUrl: '' };
	await openWithSystemControls(page, [coral, second, unavailable]);
	await page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true }).click();
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.map((row) => row.getAttribute('data-track-id'))
	)).toEqual([coral.id, second.id, unavailable.id]);
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('library-list')).toHaveCount(0);

	const native = page.getByTestId('audio-player');
	await systemAction(page, 'nexttrack');
	await expect(native).toHaveAttribute('src', second.streamUrl);
	await expect(native).toHaveJSProperty('paused', true);
	const title = () => page.evaluate(() => navigator.mediaSession.metadata?.title);
	await expect.poll(title).toBe(second.title);

	const secondAudio = (await native.elementHandle())!;
	await systemAction(page, 'nexttrack');
	expect(await secondAudio.evaluate((el) => el.isConnected && (el as HTMLAudioElement).paused)).toBe(true);
	expect(await title()).toBe(second.title);
	await systemAction(page, 'previoustrack');
	await expect(native).toHaveAttribute('src', coral.streamUrl);
	await expect(native).toHaveJSProperty('paused', true);
	await expect.poll(title).toBe(coral.title);
});

async function navigateInApp(page: Page, path: string) {
	await page.evaluate((href) => {
		const link = Object.assign(document.createElement('a'), { href });
		document.body.append(link);
		link.click();
		link.remove();
	}, path);
}

const systemSnapshot = (page: Page) => page.evaluate(() => {
	const { metadata } = navigator.mediaSession;
	const { handlers } = window.__librarySystemTransport;
	return {
		metadata: metadata && { title: metadata.title, artist: metadata.artist, album: metadata.album, artwork: metadata.artwork },
		active: Object.keys(handlers).filter((action) => handlers[action]).sort()
	};
});
const cleared = { metadata: null, active: [] };

const retiredAction = (page: Page, action: 'play' | 'pause') =>
	page.evaluate((key) => window.__librarySystemTransport.retired[key]!(), action);

test("navigation to /pricing retires the app's system controls", async ({ page }) => {
	await openWithSystemControls(page);
	const audio = (await page.getByTestId('audio-player').elementHandle())!;
	await audio.evaluate((el: HTMLAudioElement) => {
		el.play = async () => { window.__librarySystemTransport.retiredPlays += 1; };
	});
	await navigateInApp(page, '/pricing');
	await expect(page).toHaveURL(/\/pricing$/);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect.poll(() => systemSnapshot(page)).toEqual(cleared);
	await retiredAction(page, 'play');
	expect(await systemSnapshot(page)).toEqual(cleared);
	await retiredAction(page, 'pause');
	expect(await systemSnapshot(page)).toEqual(cleared);
	expect(await page.evaluate(() => window.__librarySystemTransport.retiredPlays)).toBe(0);
});

test('direct Library-to-demo navigation leaves the demo system controls usable', async ({ page }) => {
	// Both players update the same Media Session during this navigation. Keep this
	// check while the demo runs, since the remount below waits for a public route.
	const wav = expectedSyntheticBody();
	await page.route('**/api/demo/audio**', (route) => serveRangedAudio(route, wav, 'audio/wav'));
	await openWithSystemControls(page);
	const session = await page.evaluateHandle(() => navigator.mediaSession);
	await navigateInApp(page, '/demo/player');
	await expect(page).toHaveURL(/\/demo\/player$/);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	expect(await session.evaluate((original) => original === navigator.mediaSession)).toBe(true);
	const demo = page.getByTestId('demo-audio');
	await expect(demo).toBeVisible();
	await expect.poll(() => systemSnapshot(page)).toMatchObject({
		metadata: { title: expect.stringMatching(/\S/) },
		active: expect.arrayContaining(['play', 'pause', 'seekto'])
	});
	const current = await systemSnapshot(page);
	expect(current.metadata?.title).not.toBe(coral.title);
	await expect(demo).toHaveJSProperty('paused', true);
	const time = () => demo.evaluate((el: HTMLAudioElement) => el.currentTime);
	const pausedAt = await time();
	await systemAction(page, 'play');
	await expect(demo).toHaveJSProperty('paused', false);
	await expect.poll(time).toBeGreaterThan(pausedAt + 0.1);
	await systemAction(page, 'pause');
	await expect(demo).toHaveJSProperty('paused', true);
	expect(await systemSnapshot(page)).toEqual(current);
});

signedInTest('a remounted Library player owns system Play and Pause', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const first = {
		id: '00000000-0000-4000-8000-000000000001', title: 'First Tone', artist: 'First Artist',
		release: 'First Album', duration_ms: 16_381
	};
	const second = {
		...first, id: '00000000-0000-4000-8000-000000000002', title: 'Second Tone',
		artist: 'Second Artist', release: 'Second Album'
	};
	const tracks = [first, second];
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks,
		playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
		audio: expectedSyntheticBody(), audioContentType: 'audio/wav'
	});
	await recordSystemControls(page);
	await fixture.signIn();
	await page.goto(origin + library.path);

	const native = page.getByTestId(player.audio);
	const time = () => native.evaluate((el: HTMLAudioElement) => el.currentTime);
	const snapshot = () => systemSnapshot(page);
	const expectTrack = (track: typeof first) => expect.poll(snapshot).toMatchObject({
		metadata: { title: track.title, artist: track.artist, album: track.release },
		active: expect.arrayContaining(['play', 'pause', 'seekto'])
	});
	const playTrack = async (track: typeof first) => {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${track.id}"]`))
			.locator(library.select).click();
		expect((await playQueuedTrack(page, origin, track.title)).status()).toBe(200);
		await expect(native).toHaveAttribute('src', fixture.mediaPath(track.id));
		await expect.poll(time).toBeGreaterThan(0.1);
		await expectTrack(track);
	};
	await playTrack(first);
	await rememberSystemControls(page);
	const oldAudio = (await native.elementHandle())!;
	await oldAudio.evaluate((el: HTMLAudioElement) => {
		el.play = async () => { window.__librarySystemTransport.retiredPlays += 1; };
	});

	// The public route destroys the app player while keeping the recorded callbacks in this document.
	await navigateInApp(page, '/');
	await expect(page).toHaveURL(origin + '/');
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(native).toHaveCount(0);
	expect(await oldAudio.evaluate((el: HTMLAudioElement) => !el.isConnected && el.paused)).toBe(true);
	await expect.poll(snapshot).toEqual(cleared);
	await page.goBack();
	await expect(page).toHaveURL(origin + library.path);
	await playTrack(second);
	const freshAudio = (await native.elementHandle())!;
	expect(await oldAudio.evaluate((old, fresh) => old !== fresh && !old.isConnected && fresh.isConnected, freshAudio)).toBe(true);
	expect(await page.evaluate(() => {
		const { handlers, retired } = window.__librarySystemTransport;
		return ['play', 'pause'].every((action) => retired[action] && handlers[action] !== retired[action]);
	})).toBe(true);

	// Count calls while preserving native behavior so a stale call cannot hide behind a later valid action.
	const calls = await freshAudio.evaluateHandle((el: HTMLAudioElement) => {
		const calls = { play: 0, pause: 0 };
		const { play, pause } = el;
		el.play = () => { calls.play += 1; return play.call(el); };
		el.pause = () => { calls.pause += 1; pause.call(el); };
		return calls;
	});
	const current = await snapshot();
	await retiredAction(page, 'pause');
	expect(await freshAudio.evaluate((el: HTMLAudioElement) => el.paused)).toBe(false);
	expect(await snapshot()).toEqual(current);
	await systemAction(page, 'pause');
	await expect(native).toHaveJSProperty('paused', true);
	const pausedAt = await time();
	await retiredAction(page, 'play');
	expect(await freshAudio.evaluate((el: HTMLAudioElement) => el.paused)).toBe(true);
	expect(await snapshot()).toEqual(current);
	await systemAction(page, 'play');
	await expect(native).toHaveJSProperty('paused', false);
	await expect.poll(time).toBeGreaterThan(pausedAt + 0.1);

	expect(await calls.jsonValue()).toEqual({ play: 1, pause: 1 });
	expect(await snapshot()).toEqual(current);
	expect(await page.evaluate(() => window.__librarySystemTransport.retiredPlays)).toBe(0);
});
