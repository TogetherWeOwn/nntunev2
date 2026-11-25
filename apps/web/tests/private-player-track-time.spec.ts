import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test.use({ viewport: { width: 390, height: 844 } });

test('Library and Now Playing display elapsed time and duration from the private decoder', async ({ page, origin }) => {
	// The catalogue can disagree with the 14-second FLAC.
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Fixture tone', duration_ms: 15_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const { library, player: hooks } = PRIVATE_MUSIC;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track],
		playableTrackId: (id) => id === track.id ? id : undefined,
		audio: tone.body
	});
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const seek = player.getByRole('slider', { name: 'Seek', exact: true });
	const clocks = player.locator('.nt-player-seek > span');
	const audio = player.getByTestId(hooks.audio);
	const mini = page.getByTestId('mini-player');
	const miniLink = mini.getByRole('link', { name: `${track.title} Now Playing`, exact: true });
	const main = page.getByRole('main');
	const expectClock = async (elapsed: string, total: string | null) => {
		await expect(clocks).toHaveText([elapsed, total ?? '—']);
		await expect(clocks.nth(0)).toBeVisible();
		await expect(clocks.nth(1)).toBeVisible();
		await expect(seek).toHaveAttribute('aria-valuetext', `${elapsed} of ${total ?? 'unknown duration'}`);
	};

	await fixture.signIn();
	await page.goto(origin + library.path);
	await main.getByRole('button', { name: track.title, exact: true }).click();
	await expect(miniLink).toBeVisible();

	// Hold the relay bytes so the native element exists without metadata.
	const mediaRoute = `${origin}${hooks.plays}/media/${track.id}/**`;
	let releaseMedia!: () => void;
	const held = new Promise<void>((resolve) => { releaseMedia = resolve; });
	await page.route(mediaRoute, async (route) => {
		await held;
		await route.fallback();
	});
	try {
		await player.getByRole('button', { name: 'Play', exact: true }).click();
		await expect(audio).toHaveJSProperty('readyState', 0);
		await expect(audio).toHaveAttribute('src', fixture.mediaPath(track.id));
		await expectClock('0:00', null);

		await miniLink.click();
		await expect(page).toHaveURL(origin + '/now-playing');
		await expect(main.getByRole('heading', { name: 'Now Playing', exact: true })).toBeVisible();
		await expect(mini).toHaveCount(0);
		await expect(audio).toHaveJSProperty('readyState', 0);
		await expectClock('0:00', null);

		await audio.evaluate((el: HTMLAudioElement) => {
			el.addEventListener('loadedmetadata', (event) => {
				if (event.isTrusted) el.dataset.nativeDuration = String(el.duration);
			});
			el.addEventListener('timeupdate', (event) => {
				if (event.isTrusted && !el.paused && el.currentTime >= 1) el.dataset.nativeClockAdvanced = 'yes';
			});
		});
	} finally {
		releaseMedia();
	}

	await expect(audio).toHaveAttribute('data-native-duration', '14');
	await expect(audio).toHaveAttribute('data-native-clock-advanced', 'yes');
	await expect(seek).toHaveAttribute('aria-valuetext', /^0:(0[1-9]|1[0-3]) of 0:14$/);
	await expect(clocks.first()).toHaveText(/^0:(0[1-9]|1[0-3])$/);
	await expect(audio).toHaveJSProperty('paused', false);
	await player.getByRole('button', { name: 'Pause', exact: true }).click();
	await expect(audio).toHaveJSProperty('paused', true);
	const seconds = await audio.evaluate((el: HTMLAudioElement) => Math.floor(el.currentTime));
	expect(seconds).toBeGreaterThan(0);
	const elapsed = `0:${String(seconds).padStart(2, '0')}`;
	await expectClock(elapsed, '0:14');

	await main.getByRole('button', { name: 'Back', exact: true }).click();
	await expect(page).toHaveURL(origin + library.path);
	await expect(miniLink).toBeVisible();
	await expectClock(elapsed, '0:14');
});
