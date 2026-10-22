import { describe, expect, it, vi } from 'vitest';
import { createPlayback, type AudioLike } from './playback.js';

/**
 * Acceptance contract: a failed `play()` must become visible to the caller
 * instead of being silently swallowed, so the demo player can show a
 * recoverable error state. The contract these tests define for the
 * implementation:
 *
 * - `createPlayback` accepts an optional `onPlaybackFailure` callback.
 * - It fires exactly once when the *current* (latest-intent) play attempt
 *   fails: the element's `play()` rejects or throws synchronously.
 * - It never fires for a superseded attempt: a play that was overtaken by a
 *   newer play, a pause, or an unload before its failure landed must not
 *   flag an error over the newer state.
 * - `playback.play()` itself still resolves (never rejects) so queue
 *   navigation cannot break, and each `playback.play()` call reaches the
 *   element at most once — no automatic retry loop.
 * - After a failure, a fresh explicit `play()` (the retry action) reaches
 *   the element again and can succeed without re-firing the callback.
 *
 * Determinism: `flush()` is immediate unless a test gates it, and the
 * element's `play()` promises are settled by the test in the exact order
 * under test. No timers, no real media, no DOM.
 */

interface Deferred<T> {
	promise: Promise<T>;
	resolve: (value: T | PromiseLike<T>) => void;
	reject: (reason?: unknown) => void;
}

function deferred<T = void>(): Deferred<T> {
	let resolve: Deferred<T>['resolve'] = () => {};
	let reject: Deferred<T>['reject'] = () => {};
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

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

function setup(audio: AudioLike, flush: () => Promise<void> = () => Promise.resolve()) {
	const onPlaybackFailure = vi.fn();
	const playback = createPlayback({ audio: () => audio, flush, onPlaybackFailure });
	return { playback, onPlaybackFailure };
}

describe('playback failure visibility', () => {
	it('reports a rejected play() from the latest attempt exactly once', async () => {
		const { audio } = fakeAudio({
			play: vi.fn(() => Promise.reject(new Error('NotSupportedError')))
		});
		const { playback, onPlaybackFailure } = setup(audio);

		await expect(playback.play()).resolves.toBeUndefined();

		expect(onPlaybackFailure).toHaveBeenCalledTimes(1);
		// No automatic retry: one playback.play() reached the element once.
		expect(audio.play).toHaveBeenCalledTimes(1);
	});

	it('reports a synchronously throwing play() as a failure', async () => {
		const { audio } = fakeAudio({
			play: vi.fn(() => {
				throw new Error('sync failure');
			})
		});
		const { playback, onPlaybackFailure } = setup(audio);

		await expect(playback.play()).resolves.toBeUndefined();
		expect(onPlaybackFailure).toHaveBeenCalledTimes(1);
	});

	it('does not report a failure when the latest play succeeds', async () => {
		const { audio } = fakeAudio();
		const { playback, onPlaybackFailure } = setup(audio);

		await playback.play();

		expect(audio.play).toHaveBeenCalledTimes(1);
		expect(onPlaybackFailure).not.toHaveBeenCalled();
	});

	it('allows an explicit retry after a failure and stays quiet when it succeeds', async () => {
		let shouldReject = true;
		const { audio } = fakeAudio({
			play: vi.fn(() =>
				shouldReject ? Promise.reject(new Error('NotAllowedError')) : Promise.resolve()
			)
		});
		const { playback, onPlaybackFailure } = setup(audio);

		await playback.play();
		expect(onPlaybackFailure).toHaveBeenCalledTimes(1);

		// The retry action: a fresh, explicit play() must reach the element.
		shouldReject = false;
		await playback.play();

		expect(audio.play).toHaveBeenCalledTimes(2);
		expect(onPlaybackFailure).toHaveBeenCalledTimes(1);
	});

	it('reports each failing explicit attempt, with no hidden retries in between', async () => {
		const { audio } = fakeAudio({
			play: vi.fn(() => Promise.reject(new Error('NotSupportedError')))
		});
		const { playback, onPlaybackFailure } = setup(audio);

		await playback.play();
		await playback.play();

		expect(audio.play).toHaveBeenCalledTimes(2);
		expect(onPlaybackFailure).toHaveBeenCalledTimes(2);
	});

	it('does not report a failure while the audio element is not mounted', async () => {
		const onPlaybackFailure = vi.fn();
		const playback = createPlayback({
			audio: () => undefined,
			flush: () => Promise.resolve(),
			onPlaybackFailure
		});

		await expect(playback.play()).resolves.toBeUndefined();
		expect(onPlaybackFailure).not.toHaveBeenCalled();
	});
});

describe('stale failures cannot overwrite newer intent', () => {
	it('ignores a delayed rejection from a play superseded by a newer play', async () => {
		const first = deferred();
		let attempt = 0;
		const { audio } = fakeAudio({
			play: vi.fn(() => {
				attempt += 1;
				return attempt === 1 ? first.promise : Promise.resolve();
			})
		});
		const { playback, onPlaybackFailure } = setup(audio);

		const stalePlay = playback.play();
		// Let the first attempt reach the element before superseding it.
		await vi.waitFor(() => expect(audio.play).toHaveBeenCalledTimes(1));

		const currentPlay = playback.play();
		await currentPlay;
		expect(audio.play).toHaveBeenCalledTimes(2);

		// The old attempt fails only now; the new track is already playing.
		first.reject(new Error('AbortError'));
		await expect(stalePlay).resolves.toBeUndefined();

		expect(onPlaybackFailure).not.toHaveBeenCalled();
	});

	it('does not flag an error when a pending play rejects after an intentional pause', async () => {
		const pending = deferred();
		const { audio } = fakeAudio({ play: vi.fn(() => pending.promise) });
		const { playback, onPlaybackFailure } = setup(audio);

		const playAttempt = playback.play();
		await vi.waitFor(() => expect(audio.play).toHaveBeenCalledTimes(1));

		playback.pause();
		// Browsers reject the interrupted play() promise (AbortError) after a
		// pause; that is the user pausing, not a playback failure.
		pending.reject(new Error('AbortError'));
		await expect(playAttempt).resolves.toBeUndefined();

		expect(onPlaybackFailure).not.toHaveBeenCalled();
	});

	it('does not flag an error when a pending play rejects after unload (queue cleared)', async () => {
		const pending = deferred();
		const { audio } = fakeAudio({ play: vi.fn(() => pending.promise) });
		const { playback, onPlaybackFailure } = setup(audio);

		const playAttempt = playback.play();
		await vi.waitFor(() => expect(audio.play).toHaveBeenCalledTimes(1));

		const unloadAttempt = playback.unload();
		pending.reject(new Error('AbortError'));
		await expect(playAttempt).resolves.toBeUndefined();
		await unloadAttempt;

		expect(onPlaybackFailure).not.toHaveBeenCalled();
	});

	it('never reaches the element, so never fails, when superseded while awaiting flush', async () => {
		const flushGate = deferred();
		const { audio } = fakeAudio({
			play: vi.fn(() => Promise.reject(new Error('NotSupportedError')))
		});
		const { playback, onPlaybackFailure } = setup(audio, () => flushGate.promise);

		const stalePlay = playback.play();
		playback.pause();
		flushGate.resolve();
		await expect(stalePlay).resolves.toBeUndefined();

		expect(audio.play).not.toHaveBeenCalled();
		expect(onPlaybackFailure).not.toHaveBeenCalled();
	});
});
