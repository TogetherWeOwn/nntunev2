import { describe, expect, it, vi } from 'vitest';
import { createPlayback, type AudioLike } from './playback.js';

/**
 * Acceptance contract: the *latest* playback intent wins across delayed
 * flushes. `play()` and `unload()` await `flush()` before touching the
 * element again; if a newer operation superseded them while they waited,
 * their continuation must be inert. Resolving an older flush must never
 * resurrect a stale `play()` or `load()`.
 *
 * Determinism: every `flush()` call returns its own individually controlled
 * promise, resolved by the test in the exact interleaving under test. No
 * timers, no real media, no DOM.
 */

interface ControlledFlush {
	flush: () => Promise<void>;
	/** Resolve the flush promise handed out on call `index` (0-based). */
	release: (index: number) => void;
	/** Number of times `flush()` has been called so far. */
	callCount: () => number;
}

function controlledFlush(): ControlledFlush {
	const resolvers: Array<() => void> = [];
	return {
		flush: () =>
			new Promise<void>((resolve) => {
				resolvers.push(resolve);
			}),
		release: (index) => {
			const resolve = resolvers[index];
			if (!resolve) {
				throw new Error(`flush call ${index} was never made (only ${resolvers.length})`);
			}
			resolve();
		},
		callCount: () => resolvers.length
	};
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

describe('latest playback intent across delayed flushes', () => {
	it('makes a play awaiting flush inert after a later pause', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const pendingPlay = playback.play();
		playback.pause();
		expect(calls).toEqual(['pause']);

		gate.release(0);
		await pendingPlay;

		// The pause is the latest intent: the older flush resolving must not
		// resume playback.
		expect(calls).toEqual(['pause']);
		expect(audio.play).not.toHaveBeenCalled();
	});

	it('makes a play awaiting flush inert after a later unload', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const pendingPlay = playback.play(); // flush #0
		const pendingUnload = playback.unload(); // pauses now, flush #1
		expect(calls).toEqual(['pause']);

		// The stale play's flush resolves first: still no play().
		gate.release(0);
		await pendingPlay;
		expect(calls).toEqual(['pause']);

		// The unload is the latest intent, so its own continuation runs.
		gate.release(1);
		await pendingUnload;
		expect(calls).toEqual(['pause', 'load']);
	});

	it('makes an unload awaiting flush inert after a later play', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const pendingUnload = playback.unload(); // pauses now, flush #0
		const pendingPlay = playback.play(); // flush #1
		expect(calls).toEqual(['pause']);

		// The unload's flush resolves after a newer play intent appeared:
		// load() here would release the source the play is about to start.
		gate.release(0);
		await pendingUnload;
		expect(calls).toEqual(['pause']);
		expect(audio.load).not.toHaveBeenCalled();

		gate.release(1);
		await pendingPlay;
		expect(calls).toEqual(['pause', 'play']);
	});

	it('executes only the latest of two overlapping plays (in-order flush)', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const first = playback.play(); // flush #0
		const second = playback.play(); // flush #1
		expect(gate.callCount()).toBe(2);

		gate.release(0);
		await first;
		expect(calls).toEqual([]);

		gate.release(1);
		await second;
		expect(calls).toEqual(['play']);
		expect(audio.play).toHaveBeenCalledTimes(1);
	});

	it('executes only the latest of two overlapping plays (out-of-order flush)', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const first = playback.play(); // flush #0
		const second = playback.play(); // flush #1

		// The newer flush lands first and plays; the older flush landing
		// afterwards must not play a second time.
		gate.release(1);
		await second;
		expect(calls).toEqual(['play']);

		gate.release(0);
		await first;
		expect(calls).toEqual(['play']);
		expect(audio.play).toHaveBeenCalledTimes(1);
	});

	it('does not let a superseded unload load() after the newer play already ran', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const pendingUnload = playback.unload(); // pauses now, flush #0
		const pendingPlay = playback.play(); // flush #1

		gate.release(1);
		await pendingPlay;
		expect(calls).toEqual(['pause', 'play']);

		gate.release(0);
		await pendingUnload;
		expect(calls).toEqual(['pause', 'play']);
		expect(audio.load).not.toHaveBeenCalled();
	});

	it('keeps the newest play valid after an intervening pause (play, pause, play)', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const firstPlay = playback.play(); // flush #0
		playback.pause();
		const secondPlay = playback.play(); // flush #1

		gate.release(0);
		await firstPlay;
		expect(calls).toEqual(['pause']);

		// A pause invalidates only the play that preceded it; a play issued
		// after the pause is the latest intent and must still run.
		gate.release(1);
		await secondPlay;
		expect(calls).toEqual(['pause', 'play']);
		expect(audio.play).toHaveBeenCalledTimes(1);
	});

	it('invalidates a whole stale chain (unload, play, then pause)', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const pendingUnload = playback.unload(); // pause #1, flush #0
		const pendingPlay = playback.play(); // flush #1
		playback.pause(); // pause #2 — latest intent

		gate.release(0);
		gate.release(1);
		await Promise.all([pendingUnload, pendingPlay]);

		expect(calls).toEqual(['pause', 'pause']);
		expect(audio.play).not.toHaveBeenCalled();
		expect(audio.load).not.toHaveBeenCalled();
	});
});

describe('positive controls: ordinary sequencing is unchanged', () => {
	it('still plays only after the flush when nothing supersedes it', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const pending = playback.play();
		expect(calls).toEqual([]);
		expect(audio.play).not.toHaveBeenCalled();

		gate.release(0);
		await pending;
		expect(calls).toEqual(['play']);
	});

	it('still pauses immediately and load()s after the flush on an ordinary unload', async () => {
		const { audio, calls } = fakeAudio();
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const pending = playback.unload();
		expect(calls).toEqual(['pause']);
		expect(audio.load).not.toHaveBeenCalled();

		gate.release(0);
		await pending;
		expect(calls).toEqual(['pause', 'load']);
	});

	it('is safe across interleavings while the audio element is absent', async () => {
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => undefined, flush: gate.flush });

		const pendingPlay = playback.play();
		const pendingUnload = playback.unload();
		playback.pause();

		gate.release(0);
		gate.release(1);
		await expect(pendingPlay).resolves.toBeUndefined();
		await expect(pendingUnload).resolves.toBeUndefined();
	});

	it('a rejected audio.play() from the latest play resolves and later navigation still works', async () => {
		const calls: string[] = [];
		let shouldReject = true;
		const { audio } = fakeAudio({
			play: vi.fn(() => {
				calls.push('play');
				return shouldReject
					? Promise.reject(new Error('NotSupportedError'))
					: Promise.resolve();
			}),
			pause: vi.fn(() => void calls.push('pause'))
		});
		const gate = controlledFlush();
		const playback = createPlayback({ audio: () => audio, flush: gate.flush });

		const rejected = playback.play(); // flush #0
		gate.release(0);
		await expect(rejected).resolves.toBeUndefined();
		expect(calls).toEqual(['play']);

		// Queue navigation after the rejection: pause still acts at once and
		// a fresh play still reaches the element.
		playback.pause();
		shouldReject = false;
		const next = playback.play(); // flush #1
		gate.release(1);
		await expect(next).resolves.toBeUndefined();
		expect(calls).toEqual(['play', 'pause', 'play']);
	});
});
