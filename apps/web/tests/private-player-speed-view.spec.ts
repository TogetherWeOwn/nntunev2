import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test.use({ viewport: { width: 390, height: 844 } });

test('playback speed follows the same private track when Now Playing closes and reopens', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Private speed tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track], playableTrackId: (id) => id === track.id ? id : undefined, audio: tone.body
	});
	const main = page.getByRole('main');
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio).and(page.locator('audio'));
	const speed = page.getByRole('combobox', { name: 'Playback speed', exact: true });
	const mini = page.getByTestId('mini-player').getByRole('link', { name: `${track.title} Now Playing`, exact: true });

	await fixture.signIn();
	await page.goto(origin + library.path);
	await main.getByRole('button', { name: 'Play displayed from the top', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0)).toBe(true);
	const source = origin + fixture.mediaPath(track.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	const decoder = await audio.elementHandle();

	// Pause the decoded FLAC so natural completion cannot end the track during navigation.
	await transport.getByRole('button', { name: 'Pause', exact: true }).click();
	await expect(audio).toHaveJSProperty('paused', true);

	const expectSpeed = async (rate: number) => {
		await expect(speed).toBeVisible();
		await expect(speed).toBeEnabled();
		await expect(speed).toHaveValue(String(rate));
		await expect(audio).toHaveJSProperty('playbackRate', rate);
		await expect(audio).toHaveJSProperty('currentSrc', source);
		expect(await audio.evaluate((node, original) => node === original, decoder)).toBe(true);
	};
	const openNowPlaying = async () => {
		await expect(mini).toBeVisible();
		await mini.press('Enter');
		await expect(page).toHaveURL(`${origin}/now-playing`);
		await expect(main.getByRole('heading', { name: track.title, exact: true })).toBeVisible();
	};

	await speed.selectOption({ label: '1.25x' });
	await expectSpeed(1.25);
	await openNowPlaying();
	await expectSpeed(1.25);

	await speed.selectOption({ label: '1.5x' });
	await expectSpeed(1.5);
	await main.getByRole('button', { name: 'Back', exact: true }).click();
	await expect(page).toHaveURL(origin + library.path);
	await expectSpeed(1.5);
	await openNowPlaying();
	await expectSpeed(1.5);
});
