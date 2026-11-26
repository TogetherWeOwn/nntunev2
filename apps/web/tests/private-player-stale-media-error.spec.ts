import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { playQueuedTrack, serveFlac, test } from './helpers/private-music-fixture.js';

type ProbedAudio = HTMLAudioElement & { replayCapturedError?: () => boolean };

test('invited playback ignores a retired audio error and retries a current audio error', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const first = { id: '00000000-0000-4000-8000-000000000001', title: 'First tone', duration_ms: 14_000 };
	const second = { id: '00000000-0000-4000-8000-000000000002', title: 'Second tone', duration_ms: 14_000 };
	const tones = privateStagingAudio();
	const firstTone = tones.find(({ file }) => file === 'tone-a.flac')!;
	const secondTone = tones.find(({ file }) => file === 'tone-b.flac')!;
	let failSecond = false;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [first, second],
		playableTrackId: (id) => [first.id, second.id].find((trackId) => trackId === id),
		audio: async (route) => {
			const isSecond = new URL(route.request().url()).pathname.startsWith(`${player.plays}/media/${second.id}/`);
			if (isSecond && failSecond) return route.fulfill({ status: 503 });
			return serveFlac(route, (isSecond ? secondTone : firstTone).body);
		}
	});
	const grants: Array<{ track_id: string; session_id: string; play_id?: string }> = [];
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) grants.push(request.postDataJSON());
	});
	await page.addInitScript((audioTestId) => {
		const prototype = HTMLAudioElement.prototype;
		const add = prototype.addEventListener;
		prototype.addEventListener = function (this: ProbedAudio, type, listener, options) {
			if (type === 'error' && typeof listener === 'function' && typeof options === 'object' && options.signal
				&& this.dataset.testid === audioTestId) {
				// Keep a callable copy that survives normal listener removal. Dispatch on the
				// retired element so the callback sees its original currentTarget.
				this.replayCapturedError = () => {
					let delivered = false;
					add.call(this, 'error', (event) => {
						delivered = true;
						listener.call(this, event);
					}, { once: true });
					this.dispatchEvent(new Event('error'));
					return delivered;
				};
			}
			add.call(this, type, listener, options);
		};
	}, player.audio);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const status = transport.getByRole('status');
	const seek = transport.getByRole('slider', { name: 'Seek', exact: true });
	const retry = transport.getByRole('button', { name: 'Retry playback', exact: true });
	const expectSecondSelected = async () => {
		await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, second.id);
		await expect(transport.getByTestId('library-now-playing')).toHaveText(second.title);
		await expect(audio).toHaveJSProperty('currentSrc', origin + fixture.mediaPath(second.id));
	};
	const advancing = (after: number) => expect.poll(() => audio.evaluate((node: HTMLAudioElement, time) =>
		!node.paused && !node.ended && node.error === null && node.currentTime > time, after)).toBe(true);

	await fixture.signIn();
	await page.goto(origin + library.path);
	for (const track of [first, second]) {
		await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
	}
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await advancing(0.2);
	await expect(audio).toHaveJSProperty('currentSrc', origin + fixture.mediaPath(first.id));
	const retired = (await audio.elementHandle())!;

	expect((await playQueuedTrack(page, origin, second.title)).ok()).toBe(true);
	await expectSecondSelected();
	await expect(status).toHaveText('Playing');
	await advancing(1);
	const replacement = (await audio.elementHandle())!;
	const interruptions = await replacement.evaluateHandle((node: HTMLAudioElement) => {
		const events: string[] = [];
		for (const type of ['pause', 'seeking', 'loadstart', 'emptied', 'error']) {
			node.addEventListener(type, () => events.push(type));
		}
		return events;
	});
	const before = await replacement.evaluate((node: HTMLAudioElement) => node.currentTime);
	const displayedBefore = Number(await seek.inputValue());
	expect(displayedBefore).toBeGreaterThan(0);
	expect(await retired.evaluate((node: ProbedAudio) => {
		if (node.isConnected) throw new Error('The first audio element is still attached');
		Object.defineProperty(node, 'error', { value: { code: MediaError.MEDIA_ERR_NETWORK } });
		return node.replayCapturedError?.();
	})).toBe(true);
	await advancing(before + 0.2);
	await expectSecondSelected();
	await expect(status).toHaveText('Playing');
	await expect(retry).toHaveCount(0);
	await expect.poll(async () => Number(await seek.inputValue())).toBeGreaterThanOrEqual(displayedBefore);
	expect(await interruptions.jsonValue()).toEqual([]);
	expect(await audio.evaluate((node, original) => node === original, replacement)).toBe(true);
	expect(grants.map(({ track_id, play_id }) => [track_id, play_id])).toEqual([
		[first.id, undefined], [second.id, undefined]
	]);

	// Force an uncached read on the current element and let Chromium report the media failure.
	failSecond = true;
	const failedLoad = page.waitForResponse((response) => response.url() === origin + fixture.mediaPath(second.id));
	await replacement.evaluate((node: HTMLAudioElement) => node.load());
	expect((await failedLoad).status()).toBe(503);
	await expect(status).toHaveText('This audio could not be loaded or played. Retry playback or choose another track.');
	await expect(retry).toBeEnabled();
	await expectSecondSelected();
	expect(await replacement.evaluate((node: HTMLAudioElement) => node.error !== null)).toBe(true);
	expect(grants).toHaveLength(2);

	failSecond = false;
	await retry.press('Enter');
	await expect(status).toHaveText('Playing');
	await advancing(0.2);
	await expectSecondSelected();
	await expect(retry).toHaveCount(0);
	expect(await audio.evaluate((node, original) => node === original, replacement)).toBe(true);
	const secondGrant = grants[1]!;
	expect(grants.slice(2)).toEqual([{ ...secondGrant, play_id: fixture.mediaPath(second.id).split('/').at(-1) }]);
});
