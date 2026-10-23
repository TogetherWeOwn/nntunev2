import { describe, expect, it, vi } from 'vitest';
import { createPlayback, type AudioLike, type Playback } from './playback.js';

class NativeMedia extends EventTarget implements AudioLike {
	paused = false;
	ended = false;
	play = vi.fn(() => Promise.resolve());
	pause = vi.fn(() => { this.paused = true; });
	load = vi.fn();
	getAttribute = () => '/fixture/audio';
}

describe('buffering native element lifecycle', () => {
	it('a page end handler can start the next attempt before the observer sees ended', async () => {
		const media = new NativeMedia();
		let playback: Playback;
		let next: Promise<void> | undefined;
		media.addEventListener('ended', () => { next = playback.play(); });
		const onPlaybackStarted = vi.fn();
		playback = createPlayback({ audio: () => media, flush: () => Promise.resolve(), onPlaybackStarted });
		await playback.play();
		media.dispatchEvent(new Event('playing'));
		media.dispatchEvent(new Event('waiting'));
		media.dispatchEvent(new Event('ended'));
		await next;
		expect(media.play).toHaveBeenCalledTimes(2);
		expect(onPlaybackStarted).toHaveBeenCalledTimes(2);
	});

	it('native controls establish playback intent without calling controller play', () => {
		const media = new NativeMedia();
		const onBufferingChange = vi.fn();
		const playback = createPlayback({ audio: () => media, flush: () => Promise.resolve(), onBufferingChange });
		const release = playback.observe(media);
		for (const type of ['play', 'playing', 'waiting', 'playing']) media.dispatchEvent(new Event(type));
		expect(onBufferingChange.mock.calls).toEqual([[true], [false]]);
		expect(media.play).not.toHaveBeenCalled();
		release();
		for (const type of ['play', 'playing', 'waiting']) media.dispatchEvent(new Event(type));
		expect(onBufferingChange.mock.calls).toEqual([[true], [false]]);
	});

	it('waiting before the first playing event is initial loading, not buffering', async () => {
		const media = new NativeMedia();
		const onBufferingChange = vi.fn();
		const playback = createPlayback({ audio: () => media, flush: () => Promise.resolve(), onBufferingChange });
		await playback.play();
		// Play promise settled and the element unpaused, but no playing event
		// yet: a slow initial load must not be diagnosed as mid-track buffering.
		// The unpaused precondition is what makes this a killing test for the
		// hasPlayed entry guard (paused fixtures mask its removal).
		expect(media.paused).toBe(false);
		media.dispatchEvent(new Event('waiting'));
		expect(onBufferingChange).not.toHaveBeenCalled();
		// Positive control: the same waiting after a real playing event buffers.
		media.dispatchEvent(new Event('playing'));
		media.dispatchEvent(new Event('waiting'));
		expect(onBufferingChange.mock.calls).toEqual([[true]]);
	});

	it('a repeated old cleanup cannot reset a newer subscription on the same element', async () => {
		const media = new NativeMedia();
		const onBufferingChange = vi.fn();
		const playback = createPlayback({ audio: () => media, flush: () => Promise.resolve(), onBufferingChange });
		const release = playback.observe(media);
		release();
		await playback.play();
		media.dispatchEvent(new Event('playing'));
		media.dispatchEvent(new Event('waiting'));
		release();
		expect(onBufferingChange.mock.calls).toEqual([[true]]);
		media.dispatchEvent(new Event('playing'));
		expect(onBufferingChange.mock.calls).toEqual([[true], [false]]);
	});
});
