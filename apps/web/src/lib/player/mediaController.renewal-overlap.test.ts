import { expect, test, vi } from 'vitest';
import { get } from 'svelte/store';
import { createMediaController, type MediaSelection } from './mediaController.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';

const track = { id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral' };
const INTERRUPTED_SEEK = 84;

// A private decoder whose distant seek read failed: the position holds the requested target.
class InterruptedAudio extends EventTarget {
	readonly isConnected = true;
	paused = true;
	ended = false;
	seeking = false;
	error: { code: number } | null = { code: 2 };
	durationValue = 120;
	position = INTERRUPTED_SEEK;
	readonly writes: number[] = [];
	getAttribute() { return track.streamUrl; }
	get duration() { return this.durationValue; }
	get seekable() { return { length: 1, start: () => 0, end: () => this.durationValue }; }
	get currentTime() { return this.position; }
	set currentTime(value: number) { this.writes.push(value); this.position = value; }
	// A reload clears the error and starts again from zero until metadata arrives.
	load = vi.fn(() => { this.error = null; this.position = 0; this.durationValue = NaN; this.paused = true; });
	play = vi.fn(async () => { this.paused = false; });
	pause = vi.fn(() => { this.paused = true; });
	emit(type: string) { this.dispatchEvent(new Event(type)); }
	get element() { return this as unknown as HTMLAudioElement; }
}

function interruptedPrivatePlayback() {
	const queue = createQueueController(createQueue([track]));
	const renewals: { resolve: () => void; settled: Promise<boolean>; retry?: boolean }[] = [];
	const prepare: NonNullable<MediaSelection['prepare']> = (trackId, isCurrent, publish, options) => {
		const response = Promise.withResolvers<void>();
		// Like the browser grant, publish a verified receipt only while its request is still current.
		const settled = response.promise.then(() => {
			if (!isCurrent()) return false;
			publish({ streamUrl: track.streamUrl, trackId, playId: 'private_browser_play', isCurrent: () => true });
			return true;
		});
		renewals.push({ resolve: response.resolve, settled, retry: options?.retry });
		return settled;
	};
	const media = createMediaController(queue, {
		commit: () => true, flush: async () => {}, browseCopy: { invalidate: () => {} }, announce: () => {}, prepare
	}, (audio, source) => audio.getAttribute('src') === source);
	const audio = new InterruptedAudio();
	media.attach(audio.element);
	audio.emit('error');
	return { media, audio, renewals, status: () => get(media.playbackStatus) };
}

async function settle(renewal: { settled: Promise<boolean> }) {
	await renewal.settled;
	for (let turn = 0; turn < 5; turn++) await Promise.resolve();
}

// One uncontested Retry: the restore write(s) the player makes for a single renewal.
async function singleRenewalRestore() {
	const { media, audio, renewals } = interruptedPrivatePlayback();
	const retry = media.retryCurrent();
	renewals[0].resolve();
	await settle(renewals[0]);
	audio.durationValue = 120;
	audio.emit('loadedmetadata');
	await retry;
	return audio.writes;
}

test.each(['older first', 'newer first'] as const)(
	'overlapping Retry renewals restore the interrupted seek once (%s)', async (order) => {
		const restore = await singleRenewalRestore();
		expect(restore.length).toBeGreaterThan(0);
		expect(new Set(restore)).toEqual(new Set([INTERRUPTED_SEEK]));
		const { media, audio, renewals, status } = interruptedPrivatePlayback();
		expect(status()).toBe('error');

		const first = media.retryCurrent();
		const second = media.retryCurrent();
		expect(renewals.map(({ retry }) => retry)).toEqual([true, true]);
		const [older, newer] = renewals;
		// Neither renewal has touched the decoder yet.
		expect(audio.load).not.toHaveBeenCalled();
		expect(audio.position).toBe(INTERRUPTED_SEEK);

		if (order === 'older first') {
			older.resolve();
			await settle(older);
			// The superseded receipt leaves the failed decoder and its requested position alone.
			expect(await older.settled).toBe(false);
			expect(audio.load).not.toHaveBeenCalled();
			expect(audio.writes).toEqual([]);
			expect({ paused: audio.paused, position: audio.position, status: status() })
				.toEqual({ paused: true, position: INTERRUPTED_SEEK, status: 'error' });
		}

		newer.resolve();
		await settle(newer);
		expect(await newer.settled).toBe(true);
		expect(audio.load).toHaveBeenCalledOnce();
		audio.durationValue = 120;
		audio.emit('loadedmetadata');
		expect(audio.writes).toEqual(restore);
		expect(audio.play).toHaveBeenCalledOnce();
		expect(status()).toBe('playing');

		if (order === 'newer first') {
			// The older receipt arrives after recovery and must not reload, seek or pause it.
			older.resolve();
			await settle(older);
			expect(await older.settled).toBe(false);
			expect(audio.writes).toEqual(restore);
			expect({ paused: audio.paused, position: audio.position, status: status() })
				.toEqual({ paused: false, position: INTERRUPTED_SEEK, status: 'playing' });
		}
		await Promise.all([first, second]);
		// A later request remains the only position change after the single restore.
		media.seek(1, 100);
		expect(audio.load).toHaveBeenCalledOnce();
		expect(audio.play).toHaveBeenCalledOnce();
		expect(audio.pause).not.toHaveBeenCalled();
		expect(audio.writes).toEqual([...restore, 100]);
		expect({ paused: audio.paused, position: audio.position, status: status() })
			.toEqual({ paused: false, position: 100, status: 'playing' });
	});
