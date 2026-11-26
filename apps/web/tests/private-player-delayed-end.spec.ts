import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { playQueuedTrack, test } from './helpers/private-music-fixture.js';

type DelayedEndAudio = HTMLAudioElement & {
	releaseDelayedEnd?: () => Promise<{ retired: boolean; aborted: boolean; delivered: number }>;
};

test('a delayed private track end leaves the selected replacement playing until its own end', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const tracks = ['First Tone', 'Intervening Tone', 'Selected Tone', 'Successor Tone'].map((title, index) => ({
		id: `00000000-0000-4000-8000-00000000000${index + 1}`, title, duration_ms: 14_000
	}));
	const [first, , selected, successor] = tracks;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks,
		playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
		audio: privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!.body
	});
	const grantUrl = origin + player.plays;
	const grants: Array<{ track_id: string; session_id: string; play_id?: string }> = [];
	page.on('request', (request) => {
		if (request.url() === grantUrl) grants.push(request.postDataJSON());
	});
	await page.addInitScript((audioTestId) => {
		const prototype = HTMLAudioElement.prototype;
		const add = prototype.addEventListener;
		const pending: Array<{ listener: EventListener; signal: AbortSignal }> = [];
		let firstAudio: DelayedEndAudio | undefined;
		let nativeEnd: Event | undefined;
		prototype.addEventListener = function (this: DelayedEndAudio, type: string,
			listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions) {
			const signal = typeof options === 'object' && options.signal;
			if (type === 'ended' && typeof listener === 'function' && signal && this.dataset.testid === audioTestId) {
				firstAudio ??= this;
				if (this === firstAudio) {
					add.call(this, type, (event) => {
						nativeEnd = event;
						pending.push({ listener, signal });
					}, options);
					this.releaseDelayedEnd ??= async () => {
						if (!nativeEnd?.isTrusted || pending.length === 0) throw new Error('Missing delayed native completion');
						const callbacks = pending.splice(0);
						const retired = !this.isConnected;
						const aborted = callbacks.every(({ signal }) => signal.aborted);
						let settlement: Promise<unknown[]> | undefined;
						// Redispatch on the retired element so each callback sees its original currentTarget.
						add.call(this, 'ended', (event) => {
							settlement = Promise.all(callbacks.map(({ listener }) => listener.call(this, event)));
						}, { once: true });
						this.dispatchEvent(nativeEnd);
						return { retired, aborted, delivered: (await settlement)?.length ?? 0 };
					};
					return;
				}
			}
			add.call(this, type, listener, options);
		};
	}, player.audio);

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const active = page.locator(player.currentQueueItem);
	const advancing = (after: number) => expect.poll(() => audio.evaluate((node: HTMLAudioElement, time) =>
		!node.paused && !node.ended && !node.error && node.currentTime > time, after)).toBe(true);
	const expectPlaying = async (track: typeof first) => {
		await expect(active).toHaveAttribute(player.queueId, track.id);
		await expect(transport.getByTestId('library-now-playing')).toHaveText(track.title);
		await expect(audio).toHaveJSProperty('currentSrc', origin + fixture.mediaPath(track.id));
		await advancing(0.2);
	};
	const finish = async () => {
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
			Number.isFinite(node.duration) && node.duration > 2 && node.seekable.length > 0)).toBe(true);
		return audio.evaluate((node: HTMLAudioElement) => new Promise<{ trusted: boolean; source: string; at: number }>((resolve) => {
			node.addEventListener('ended', (event) => resolve({
				trusted: event.isTrusted, source: node.currentSrc, at: event.timeStamp
			}), { once: true });
			// Leave two seconds for the decoder to play through to its native end.
			node.currentTime = node.duration - 2;
		}));
	};

	await fixture.signIn();
	await page.goto(origin + library.path);
	for (const track of tracks) {
		await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
	}
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expectPlaying(first);
	const retired = await audio.elementHandle();
	const session = grants[0].session_id;
	const freshGrant = (track: typeof first) => ({ track_id: track.id, session_id: session });
	expect(await finish()).toMatchObject({ trusted: true, source: origin + fixture.mediaPath(first.id) });
	await expect(active).toHaveAttribute(player.queueId, first.id);
	expect(grants).toEqual([freshGrant(first)]);

	const selection = await playQueuedTrack(page, origin, selected.title);
	expect(selection.ok()).toBe(true);
	expect(selection.request().postDataJSON()).toEqual(freshGrant(selected));
	await expectPlaying(selected);
	const replacement = await audio.elementHandle();
	const continuity = await replacement.evaluateHandle((node: HTMLAudioElement) => {
		const interruptions: string[] = [];
		for (const type of ['pause', 'seeking', 'emptied']) {
			node.addEventListener(type, () => interruptions.push(type));
		}
		return { before: node.currentTime, interruptions };
	});
	const before = await continuity.evaluate((probe) => probe.before);
	const released = await retired.evaluate((node: DelayedEndAudio) => node.releaseDelayedEnd?.());
	expect(released).toMatchObject({ retired: true, aborted: true });
	expect(released?.delivered).toBeGreaterThan(0);
	await advancing(before + 0.2);
	await expectPlaying(selected);
	expect(await audio.evaluate((node, original) => node === original, replacement)).toBe(true);
	expect(await continuity.evaluate((probe) => probe.interruptions)).toEqual([]);
	expect(grants).toEqual([freshGrant(first), freshGrant(selected)]);

	const completed = await finish();
	expect(completed).toMatchObject({ trusted: true, source: origin + fixture.mediaPath(selected.id) });
	await expectPlaying(successor);
	expect(grants).toEqual([freshGrant(first), freshGrant(selected), freshGrant(successor)]);
	const starts = await page.evaluate((url) =>
		performance.getEntriesByName(url, 'resource').map((entry) => entry.startTime), grantUrl);
	expect(starts).toHaveLength(3);
	expect(starts[2]).toBeGreaterThanOrEqual(completed.at);
});
