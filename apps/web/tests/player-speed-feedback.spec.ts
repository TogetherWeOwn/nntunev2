import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('keeps failure feedback through its matching native ratechange', async ({ page, origin }) => {
	const track = {
		id: '00000000-0000-4000-8000-000000000001',
		title: 'Fixture tone',
		duration_ms: 14_000
	};
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track],
		playableTrackId: () => track.id,
		audio: privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!.body
	});
	await fixture.signIn();
	await page.goto(`${origin}${PRIVATE_MUSIC.library.path}`);
	await page.getByRole('main').getByRole('button', { name: track.title, exact: true }).click();

	const player = page.getByRole('region', { name: 'Player', exact: true });
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	const audio = player.getByTestId(PRIVATE_MUSIC.player.audio);
	await expect(audio).toHaveAttribute('src', fixture.mediaPath(track.id));
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0);
	await player.getByRole('button', { name: 'Pause', exact: true }).click();
	await expect(audio).toHaveJSProperty('paused', true);
	const speed = page.getByRole('combobox', { name: 'Playback speed', exact: true });
	await expect(speed).toHaveValue('1');
	const feedback = page.getByTestId('library-speed-status');
	await expect(feedback).toBeEmpty();

	await audio.evaluate((el: HTMLAudioElement) => {
		const nativeRate = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'playbackRate')!;
		Object.defineProperty(el, 'playbackRate', {
			get: nativeRate.get,
			set: (value: number) => nativeRate.set!.call(el, Math.min(value, 1.25))
		});
		el.addEventListener('ratechange', () => {
			if (el.playbackRate === 1.25) el.dataset.clampedRatechange = 'seen';
		});
	});

	await speed.selectOption('1.5');
	await expect(feedback).toHaveText('Speed 1.5x not applied — still 1.25x');

	// The event emitted by the clamped write must not immediately erase why
	// the request failed. A later, distinct native change clears that stale note.
	await expect(audio).toHaveAttribute('data-clamped-ratechange', 'seen');
	await expect(feedback).toHaveText('Speed 1.5x not applied — still 1.25x');
	await audio.evaluate((el: HTMLAudioElement) => { el.playbackRate = 1; });
	await expect(feedback).toBeEmpty();
	await expect(speed).toHaveValue('1');
});
