import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { SOUND_PREFERENCE_STORAGE_KEY } from '../src/lib/player/soundPreference.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test.use({ viewport: { width: 390, height: 844 } });

test('keyboard volume follows private playback between Library and Now Playing', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Private volume tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track], playableTrackId: (id) => id === track.id ? id : undefined, audio: tone.body
	});
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio).and(page.locator('audio'));
	const volume = transport.getByRole('slider', { name: /^Volume / });
	const mini = page.getByTestId('mini-player').getByRole('link', { name: /Now Playing/ });
	const main = page.getByRole('main');

	const expectVolume = async (percent: number) => {
		await expect(volume).toBeVisible();
		await expect(volume).toBeEnabled();
		await expect(volume).toHaveValue(String(percent));
		await expect(volume).toHaveAccessibleName(`Volume ${percent}%`);
		await expect(audio).toHaveJSProperty('volume', percent / 100);
	};
	const savedVolume = () => page.evaluate((key) =>
		JSON.parse(localStorage.getItem(key) ?? 'null')?.volume, SOUND_PREFERENCE_STORAGE_KEY);
	const openNowPlaying = async () => {
		await expect(mini).toBeVisible();
		await mini.press('Enter');
		await expect(page).toHaveURL(origin + '/now-playing');
		await expect(main.getByRole('heading', { name: track.title, exact: true })).toBeVisible();
	};

	await fixture.signIn();
	await page.goto(origin + library.path);
	await page.getByRole('button', { name: 'Play displayed from the top', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => node.readyState >= node.HAVE_CURRENT_DATA)).toBe(true);
	await expect(audio).toHaveJSProperty('currentSrc', origin + fixture.mediaPath(track.id));
	await expectVolume(100);
	await volume.press('Home');
	await volume.press('ArrowRight');
	await expectVolume(1);
	await expect.poll(savedVolume).toBe(0.01);

	await openNowPlaying();
	await expectVolume(1);
	await volume.press('ArrowRight');
	await expectVolume(2);
	await expect.poll(savedVolume).toBe(0.02);

	await main.getByRole('button', { name: 'Back', exact: true }).click();
	await expect(page).toHaveURL(origin + library.path);
	await expectVolume(2);
	await openNowPlaying();
	await expectVolume(2);
});
