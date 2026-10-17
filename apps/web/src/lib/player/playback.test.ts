import { describe, expect, it, vi } from 'vitest';
import { createPlayback, endedForCurrent, type AudioLike } from './playback.js';

function fakeAudio(overrides: Partial<AudioLike> = {}) {
	const calls: string[] = [];
	const audio: AudioLike = {
		play: vi.fn(() => {
			calls.push('play');
			return Promise.resolve();
		}),
		pause: vi.fn(() => void calls.push('pause')),
		load: vi.fn(() => void calls.push('load')),
		getAttribute: vi.fn(() => null),
		...overrides
	};
	return { audio, calls };
}

describe('createPlayback', () => {
	it('plays only after the pending src update has been flushed', async () => {
		// Models the race: play() before the flush hits the *old* source.
		let srcApplied = false;
		const seenAtPlay: boolean[] = [];
		const { audio } = fakeAudio({
			play: vi.fn(() => {
				seenAtPlay.push(srcApplied);
				return Promise.resolve();
			})
		});
		const playback = createPlayback({
			audio: () => audio,
			flush: async () => {
				await Promise.resolve();
				srcApplied = true;
			}
		});

		await playback.play();

		expect(seenAtPlay).toEqual([true]);
	});

	it('swallows a rejected or throwing play() so navigation never breaks', async () => {
		const rejecting = fakeAudio({ play: () => Promise.reject(new Error('NotSupportedError')) });
		const throwing = fakeAudio({
			play: () => {
				throw new Error('sync failure');
			}
		});
		for (const { audio } of [rejecting, throwing]) {
			const playback = createPlayback({ audio: () => audio, flush: () => Promise.resolve() });
			await expect(playback.play()).resolves.toBeUndefined();
		}
	});

	it('pauses immediately', () => {
		const { audio, calls } = fakeAudio();
		createPlayback({ audio: () => audio, flush: () => Promise.resolve() }).pause();
		expect(calls).toEqual(['pause']);
	});

	it('unload pauses at once, then load()s only after the flush', async () => {
		const { audio, calls } = fakeAudio();
		let release: () => void = () => {};
		const gate = new Promise<void>((resolve) => (release = resolve));
		const playback = createPlayback({
			audio: () => audio,
			flush: async () => {
				calls.push('flush');
				await gate;
			}
		});

		const pending = playback.unload();
		expect(calls).toEqual(['pause', 'flush']);
		release();
		await pending;
		expect(calls).toEqual(['pause', 'flush', 'load']);
	});

	it('is a no-op while the audio element is not mounted', async () => {
		const playback = createPlayback({ audio: () => undefined, flush: () => Promise.resolve() });
		playback.pause();
		await expect(playback.play()).resolves.toBeUndefined();
		await expect(playback.unload()).resolves.toBeUndefined();
	});
});

describe('endedForCurrent', () => {
	it('accepts an end for the source the queue still points at', () => {
		const { audio } = fakeAudio({ getAttribute: () => '/api/demo/audio?track=second-signal' });
		expect(endedForCurrent(audio, '/api/demo/audio?track=second-signal')).toBe(true);
	});

	it('ignores a late end from a superseded track', () => {
		// Queue already moved to the third track; the element still carries
		// the second track's src until Svelte flushes.
		const { audio } = fakeAudio({ getAttribute: () => '/api/demo/audio?track=second-signal' });
		expect(endedForCurrent(audio, '/api/demo/audio?track=third-light')).toBe(false);
	});

	it('ignores an end once the queue is empty', () => {
		const { audio } = fakeAudio({ getAttribute: () => '/api/demo/audio' });
		expect(endedForCurrent(audio, undefined)).toBe(false);
	});
});
