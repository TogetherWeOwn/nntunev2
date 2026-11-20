import { expect, test, vi } from 'vitest';
import { get } from 'svelte/store';
import { createMediaController as createMedia } from './mediaController.js';
import { createQueue, type QueueState } from './queue.js';
import { createQueueController } from './queueController.js';

const createMediaController = (matchesNative?: Parameters<typeof createMedia>[2], initial: QueueState = createQueue([])) =>
	createMedia(createQueueController(initial), undefined, matchesNative);

test('feedback that retires the attempt cannot also bump its live region', async () => {
	const queue = { tracks: [{ id: 'coral', title: 'Coral', artist: '', streamUrl: '' }], currentIndex: 0 };
	const media = createMediaController(undefined, queue);
	const announce = vi.fn(() => media.destroy());
	const bumpAnnouncement = vi.fn();
	const flush = vi.fn(async () => {});
	await media.playSelected(queue, 0, { streamUrl: '', isCurrent: () => true }, '', {
		commit: () => true, flush, announce, bumpAnnouncement
	});
	expect(announce).toHaveBeenCalledExactlyOnceWith('Selected Coral; no stream to play.');
	expect(flush).not.toHaveBeenCalled();
	expect(bumpAnnouncement).not.toHaveBeenCalled();
});

const coral = createQueue([{ id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral' }]);

class SeekableAudio extends EventTarget {
	readonly isConnected = true; readonly paused = true;
	getAttribute() { return '/coral'; }
	get duration() { return 120; }
	get seekable() { return { length: 1, start: () => 0, end: () => 120 }; }
	position = 30;
	get currentTime() { return this.position; }
	set currentTime(value: number) { this.position = value; }
}

test.each(['availability', 'position', 'assignment', 'readback'] as const)(
	'restart suppresses stale work after a replacement attachment during %s', (phase) => {
		const media = createMediaController((_audio, source) => source === '/coral', coral);
		const replacement = new SeekableAudio() as unknown as HTMLAudioElement;
		let position = 30;
		const native = new class extends SeekableAudio {
			play = vi.fn(); pause = vi.fn(); load = vi.fn();
			override get duration() { if (phase === 'availability') media.attach(replacement); return 120; }
			override get currentTime() {
				if (phase === (position === 0 ? 'readback' : 'position')) media.attach(replacement);
				return position;
			}
			override set currentTime(value: number) {
				position = value;
				if (phase === 'assignment') media.attach(replacement);
			}
		}();
		const audio = native as unknown as HTMLAudioElement;
		const staleCleanup = media.attach(audio);
		media.restart();
		staleCleanup();
		expect(media.audio).toBe(replacement);
		expect(position).toBe(phase === 'assignment' || phase === 'readback' ? 0 : 30);
		expect(get(media.seekFeedback).restartStatusVersion).toBe(0);
		for (const method of [native.play, native.pause, native.load]) expect(method).not.toHaveBeenCalled();
	}
);

test('only the newer restart on an unchanged receipt can seek and publish', () => {
	const media = createMediaController(() => true, coral);
	const native = new SeekableAudio();
	const seek = vi.spyOn(native, 'currentTime', 'set');
	media.attach(native as unknown as HTMLAudioElement);
	const receipt = media.attachment;
	vi.spyOn(native, 'duration', 'get').mockImplementationOnce(() => { media.restart(); return 120; });
	media.restart();
	expect(media.attachment).toBe(receipt);
	expect(seek).toHaveBeenCalledExactlyOnceWith(0);
	expect(get(media.seekFeedback)).toMatchObject({ restartStatus: 'Restarted Coral.', restartStatusVersion: 1 });
});

test('retiring a receipt aborts its listener bank without aborting the replacement', () => {
	const media = createMediaController();
	const attach = () => {
		const target = Object.assign(new EventTarget(), { paused: true, pause: vi.fn() });
		const listen = vi.spyOn(target, 'addEventListener');
		const cleanup = media.attach(target as unknown as HTMLAudioElement);
		const signal = (listen.mock.calls[0]?.[2] as AddEventListenerOptions | undefined)?.signal;
		expect(signal?.aborted).toBe(false);
		for (const [, , options] of listen.mock.calls) expect(options).toMatchObject({ signal });
		return { cleanup, signal, target };
	};

	const first = attach();
	const replacement = attach();
	expect(first.signal?.aborted).toBe(true);
	first.cleanup();
	expect(replacement.signal?.aborted).toBe(false);
	replacement.cleanup();
	replacement.cleanup();
	expect(replacement.signal?.aborted).toBe(true);
	expect(media.audio).toBeUndefined();

	const reset = attach();
	reset.target.paused = false;
	reset.target.pause.mockImplementationOnce(() => expect(reset.signal?.aborted).toBe(true));
	media.reset();
	expect(reset.target.pause).toHaveBeenCalledOnce();
	expect(reset.signal?.aborted).toBe(true);
	const destroyed = attach();
	media.destroy();
	expect(destroyed.signal?.aborted).toBe(true);
});

test('an observed mount cannot be reused, even by a new controller', () => {
	const media = createMediaController();
	const audio = Object.assign(new EventTarget(), { paused: true }) as unknown as HTMLAudioElement;
	media.attach(audio);
	media.destroy();
	const replacement = createMediaController();
	expect(() => replacement.attach(audio)).toThrow(/fresh audio element/);
	expect(replacement.attachment).toBeUndefined();
});

test('destroy retires the active node and refuses later mounts', () => {
	const media = createMediaController();
	const audio = Object.assign(new EventTarget(), { paused: false, pause: vi.fn() }) as unknown as HTMLAudioElement;
	const cleanup = media.attach(audio);
	media.destroy();
	media.attach(audio);
	cleanup();
	expect(media.audio).toBeUndefined();
	expect(audio.pause).toHaveBeenCalledOnce();
});
