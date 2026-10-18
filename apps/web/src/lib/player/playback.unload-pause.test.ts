import { describe, expect, it, vi } from 'vitest';
import { createPlayback, type AudioLike } from './playback.js';

/**
 * A `pause()` after `unload()` is redundant (unload already paused), not a
 * newer source intent: it must not cancel the pending unload's `load()`
 * release, or the emptied element keeps its old media loaded.
 */
describe('unload then pause', () => {
	it('still load()s after the flush when pause follows unload', async () => {
		const calls: string[] = [];
		const audio: AudioLike = {
			play: vi.fn(() => {
				calls.push('play');
				return Promise.resolve();
			}),
			pause: vi.fn(() => void calls.push('pause')),
			load: vi.fn(() => void calls.push('load')),
			getAttribute: vi.fn(() => null)
		};
		let release: () => void = () => {};
		const gate = new Promise<void>((resolve) => (release = resolve));
		const playback = createPlayback({ audio: () => audio, flush: () => gate });

		const pendingUnload = playback.unload();
		playback.pause();
		expect(calls).toEqual(['pause', 'pause']);

		release();
		await pendingUnload;

		expect(calls).toEqual(['pause', 'pause', 'load']);
		expect(audio.load).toHaveBeenCalledTimes(1);
	});
});
