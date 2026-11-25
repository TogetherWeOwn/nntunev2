import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

type RelativeAction = 'seekforward' | 'seekbackward';
declare global {
	interface Window {
		__privateRelativeSeekHandlers: Partial<Record<MediaSessionAction, MediaSessionActionHandler | null>>;
	}
}

const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Private seek tone', duration_ms: 14_000 };

for (const input of ['keyboard', 'Media Session'] as const) {
	test(`private relative seek with ${input} uses the retained paused player at both bounds`, async ({ page, origin }) => {
		const { library, player } = PRIVATE_MUSIC;
		const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
		const fixture = await mockInvitedLibraryPlayback(page, origin, {
			libraryItems: () => [track], playableTrackId: () => track.id, audio: tone.body
		});
		if (input === 'Media Session') {
			// Capture the browser's system callbacks. The app still owns the native decoder and seeks.
			await page.addInitScript(() => {
				const handlers: Window['__privateRelativeSeekHandlers'] = {};
				window.__privateRelativeSeekHandlers = handlers;
				Object.defineProperty(navigator, 'mediaSession', {
					configurable: true,
					value: {
						metadata: null,
						setActionHandler(action: MediaSessionAction, handler: MediaSessionActionHandler | null) {
							handlers[action] = handler;
						}
					}
				});
			});
		}

		const grantRequests: string[] = [];
		page.on('request', (request) => {
			if (request.url() === origin + player.plays) grantRequests.push(request.method());
		});
		const rows = page.getByTestId(library.item);
		const transport = page.getByRole('region', { name: 'Player', exact: true });
		const audio = transport.getByTestId(player.audio);
		const seek = transport.getByRole('slider', { name: 'Seek', exact: true });
		const feedback = transport.getByTestId('player-seek-feedback');
		await fixture.signIn();
		await page.goto(origin + library.path);
		await rows.filter({ hasText: track.title }).locator(library.select).click();
		await transport.getByRole('button', { name: 'Play', exact: true }).click();
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
		const native = (await audio.elementHandle())!;
		const originalSource = origin + fixture.mediaPath(track.id);
		await expect(audio).toHaveJSProperty('currentSrc', originalSource);
		const lifecycle = await native.evaluateHandle((node: HTMLAudioElement) => {
			const events: string[] = [];
			for (const type of ['play', 'loadstart', 'emptied']) node.addEventListener(type, () => events.push(type));
			return events;
		});

		const assertPosition = async (seconds: number, clock: string) => {
			await expect(seek).toHaveAttribute('aria-valuetext', `${clock} of 0:14`);
			await expect.poll(() => native.evaluate((node: HTMLAudioElement) => node.seeking)).toBe(false);
			await expect(audio).toHaveCount(1);
			expect(await audio.evaluate((node: HTMLAudioElement, original) => ({
				sameNode: node === original, paused: node.paused, source: node.currentSrc
			}), native)).toEqual({ sameNode: true, paused: true, source: originalSource });
			expect(await native.evaluate((node: HTMLAudioElement) => node.currentTime)).toBeCloseTo(seconds, 2);
			await expect(transport.getByTestId('library-now-playing')).toHaveText(track.title);
		};
		const jump = async (action: RelativeAction, message: string, seconds: number, clock: string) => {
			if (input === 'keyboard') {
				await page.getByTestId('shortcuts-button').press(action === 'seekforward' ? 'l' : 'j');
			} else {
				await page.evaluate((action) => {
					const handler = window.__privateRelativeSeekHandlers[action];
					if (!handler) throw new Error(`Missing Media Session ${action} handler`);
					handler({ action });
				}, action);
			}
			await expect(feedback).toHaveText(message);
			await assertPosition(seconds, clock);
		};

		await transport.getByRole('button', { name: 'Pause', exact: true }).click();
		await expect(audio).toHaveJSProperty('paused', true);
		await expect(seek).toBeEnabled();
		await seek.press('Home');
		for (const clock of ['0:01', '0:02', '0:04']) {
			await seek.press('PageUp');
			await expect(seek).toHaveAttribute('aria-valuetext', `${clock} of 0:14`);
		}
		await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
		await expect(page).toHaveURL(origin + '/home');
		await expect(rows).toHaveCount(0);
		await assertPosition(4.2, '0:04');
		await expect(feedback).toHaveAttribute('aria-live', 'polite');
		await jump('seekforward', 'Forward to the end, 0:14.', 14, '0:14');
		await jump('seekforward', 'Already at the end.', 14, '0:14');

		await page.goBack();
		await expect(page).toHaveURL(origin + library.path);
		await expect(rows).toHaveCount(1);
		await jump('seekbackward', 'Back 10 seconds, now at 0:04.', 4, '0:04');
		await jump('seekbackward', 'Back to the start, 0:00.', 0, '0:00');
		await jump('seekbackward', 'Already at the start.', 0, '0:00');
		expect(await lifecycle.jsonValue(), 'seeks should leave the original decoder paused without reloading').toEqual([]);
		expect(grantRequests, 'seeks should reuse the original playback grant').toEqual(['POST']);
	});
}
