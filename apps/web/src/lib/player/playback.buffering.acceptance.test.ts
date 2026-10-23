import { afterEach, describe, expect, it, vi } from 'vitest';
import { createPlayback, type AudioLike } from './playback.js';

/**
 * Independent buffering contract on the existing createPlayback boundary.
 * Optional dependency: onBufferingChange(buffering: boolean), emitted ONLY
 * on transitions. Event-capable audio uses native EventTarget subscriptions;
 * minimal AudioLike callers must remain supported. No new required exports.
 * The deps variable deliberately permits the optional callback before the
 * builder adds its type: baseline RED is an assertion, not a compile error.
 */
class Media extends EventTarget implements AudioLike {
	paused = true;
	ended = false;
	play = vi.fn((): Promise<void> => Promise.resolve());
	pause = vi.fn(() => { this.paused = true; });
	load = vi.fn();
	getAttribute = vi.fn(() => '/fixture/audio');

	fire(type: string) {
		if (type === 'playing' || type === 'play') this.paused = false;
		if (type === 'pause' || type === 'ended') this.paused = true;
		if (type === 'ended') this.ended = true;
		this.dispatchEvent(new Event(type));
	}
}

function setup() {
	let current: Media | undefined = new Media();
	const onBufferingChange = vi.fn<(buffering: boolean) => void>();
	const onPlaybackStarted = vi.fn();
	const onPlaybackFailure = vi.fn();
	const deps = {
		audio: () => current,
		flush: () => Promise.resolve(),
		onBufferingChange,
		onPlaybackStarted,
		onPlaybackFailure
	};
	return {
		media: current,
		playback: createPlayback(deps),
		onBufferingChange,
		onPlaybackStarted,
		onPlaybackFailure,
		replace: (next: Media | undefined) => { current = next; }
	};
}

function transitions(callback: ReturnType<typeof setup>['onBufferingChange']) {
	return callback.mock.calls.map(([value]) => value);
}

afterEach(() => vi.useRealTimers());

describe('buffering lifecycle acceptance', () => {
	it('BUF-U01 playing → waiting → playing reports interruption and recovery', async () => {
		const { media, playback, onBufferingChange } = setup();
		await playback.play();
		media.fire('playing');
		expect(transitions(onBufferingChange)).toEqual([]);
		media.fire('waiting');
		expect(transitions(onBufferingChange)).toEqual([true]);
		media.fire('playing');
		expect(transitions(onBufferingChange)).toEqual([true, false]);
	});

	it('BUF-U02 initial loading and play-promise success are not prior playing events', async () => {
		const { media, playback, onBufferingChange } = setup();
		let resolve = () => {};
		media.play.mockImplementation(() => new Promise<void>((done) => { resolve = done; }));
		const pending = playback.play();
		await Promise.resolve();
		expect(media.play).toHaveBeenCalledTimes(1);
		media.fire('waiting');
		media.fire('stalled');
		expect(transitions(onBufferingChange)).toEqual([]);
		resolve();
		await pending;
		media.fire('waiting');
		expect(transitions(onBufferingChange)).toEqual([]);
		media.fire('playing');
		media.fire('waiting');
		expect(transitions(onBufferingChange)).toEqual([true]);
	});

	it('BUF-U03 duplicate lifecycle events emit once per state transition', async () => {
		const { media, playback, onBufferingChange } = setup();
		await playback.play();
		for (const type of ['playing', 'playing', 'waiting', 'waiting', 'stalled',
			'playing', 'playing', 'waiting', 'waiting', 'playing']) media.fire(type);
		expect(transitions(onBufferingChange)).toEqual([true, false, true, false]);
	});

	it('BUF-U04 stalled alone never means inaudible and cannot recover a waiting state', async () => {
		const { media, playback, onBufferingChange } = setup();
		await playback.play();
		media.fire('playing');
		media.fire('stalled');
		media.fire('stalled');
		expect(transitions(onBufferingChange)).toEqual([]);
		media.fire('waiting');
		media.fire('stalled');
		expect(transitions(onBufferingChange)).toEqual([true]);
	});

	it.each(['pause', 'ended', 'error'])('BUF-U05-%s settles buffering and fences late media events', async (type) => {
		const { media, playback, onBufferingChange } = setup();
		await playback.play();
		media.fire('playing');
		media.fire('waiting');
		expect(transitions(onBufferingChange)).toEqual([true]);
		media.fire(type);
		expect(transitions(onBufferingChange)).toEqual([true, false]);
		// Queued playing does not change the flags or establish fresh intent.
		for (const name of ['waiting', 'playing', 'stalled']) media.dispatchEvent(new Event(name));
		expect(transitions(onBufferingChange)).toEqual([true, false]);
	});

	it('BUF-U06 explicit pause clears buffering even without a native pause event', async () => {
		const { media, playback, onBufferingChange } = setup();
		await playback.play();
		media.fire('playing');
		media.fire('waiting');
		playback.pause();
		media.dispatchEvent(new Event('playing'));
		media.fire('waiting');
		expect(transitions(onBufferingChange)).toEqual([true, false]);
		expect(media.play).toHaveBeenCalledTimes(1);
	});

	it('BUF-U07 unload clears buffering and ignores released-element events', async () => {
		const { media, playback, onBufferingChange, replace } = setup();
		await playback.play();
		media.fire('playing');
		media.fire('waiting');
		await playback.unload();
		replace(undefined);
		for (const name of ['waiting', 'playing', 'stalled']) media.fire(name);
		expect(transitions(onBufferingChange)).toEqual([true, false]);
	});

	it('BUF-U08 replacement/retry starts clean and old elements cannot enter or recover buffering', async () => {
		const { media: old, playback, onBufferingChange, replace } = setup();
		await playback.play();
		old.fire('playing');
		old.fire('waiting');
		const next = new Media();
		replace(next);
		await playback.play();
		expect(transitions(onBufferingChange)).toEqual([true, false]);
		old.fire('waiting');
		next.fire('waiting'); // New source has not entered playing yet.
		expect(transitions(onBufferingChange)).toEqual([true, false]);
		next.fire('playing');
		next.fire('waiting');
		for (const name of ['playing', 'pause', 'ended', 'error', 'stalled']) old.fire(name);
		expect(transitions(onBufferingChange)).toEqual([true, false, true]);
		next.fire('playing');
		expect(transitions(onBufferingChange)).toEqual([true, false, true, false]);
	});

	it('BUF-U09 buffering does not retry, reload, pause, fail, or schedule work', async () => {
		vi.useFakeTimers();
		const { media, playback, onBufferingChange, onPlaybackFailure, onPlaybackStarted } = setup();
		await playback.play();
		media.fire('playing');
		const timers = vi.getTimerCount();
		const started = onPlaybackStarted.mock.calls.length;
		for (const name of ['waiting', 'waiting', 'stalled', 'playing', 'waiting']) media.fire(name);
		expect(media.play).toHaveBeenCalledTimes(1);
		expect(media.pause).not.toHaveBeenCalled();
		expect(media.load).not.toHaveBeenCalled();
		expect(onPlaybackFailure).not.toHaveBeenCalled();
		expect(onPlaybackStarted).toHaveBeenCalledTimes(started);
		expect(vi.getTimerCount()).toBe(timers);
		expect(transitions(onBufferingChange)).toEqual([true, false, true]);
	});

	it('BUF-U11 failed initial play cannot be relabeled buffering by queued media events', async () => {
		const { media, playback, onBufferingChange, onPlaybackFailure } = setup();
		media.play.mockRejectedValue(new Error('fixture play rejection'));
		await playback.play();
		expect(onPlaybackFailure).toHaveBeenCalledTimes(1);
		for (const name of ['playing', 'waiting', 'stalled']) media.fire(name);
		expect(transitions(onBufferingChange)).toEqual([]);
		expect(media.play).toHaveBeenCalledTimes(1);
	});

	it('BUF-U10 a minimal non-event AudioLike keeps the existing start/failure contract', async () => {
		const audio: AudioLike = {
			play: vi.fn(() => Promise.resolve()), pause: vi.fn(), load: vi.fn(),
			getAttribute: () => '/fixture/audio'
		};
		const onBufferingChange = vi.fn();
		const deps = { audio: () => audio, flush: () => Promise.resolve(), onBufferingChange };
		await expect(createPlayback(deps).play()).resolves.toBeUndefined();
		expect(audio.play).toHaveBeenCalledTimes(1);
		expect(onBufferingChange).not.toHaveBeenCalled();
	});
});
