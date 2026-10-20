import { describe, expect, it, vi } from 'vitest';
import { createPlayback, type AudioLike } from './playback.js';

function deferred() {
	let resolve = () => {};
	let reject = (_reason: Error) => {};
	const promise = new Promise<void>((done, fail) => {
		resolve = done;
		reject = fail;
	});
	return { promise, resolve, reject };
}

function audio(play: AudioLike['play']): AudioLike {
	return { play, pause: vi.fn(), load: vi.fn(), getAttribute: () => '/audio' };
}

describe('current playback start outcomes', () => {
	it('reports a start only after the current play resolves', async () => {
		const pending = deferred();
		const onPlaybackStarted = vi.fn();
		const element = audio(() => pending.promise);
		const playback = createPlayback({
			audio: () => element,
			flush: () => Promise.resolve(),
			onPlaybackStarted
		});
		const attempt = playback.play();
		await Promise.resolve();
		expect(onPlaybackStarted).not.toHaveBeenCalled();
		pending.resolve();
		await attempt;
		expect(onPlaybackStarted).toHaveBeenCalledTimes(1);
	});

	it('ignores a success after pause', async () => {
		const pending = deferred();
		const element = audio(() => pending.promise);
		const onPlaybackStarted = vi.fn();
		const playback = createPlayback({ audio: () => element, flush: () => Promise.resolve(), onPlaybackStarted });
		const attempt = playback.play();
		await Promise.resolve();
		playback.pause();
		pending.resolve();
		await attempt;
		expect(onPlaybackStarted).not.toHaveBeenCalled();
	});

	it('does not report a success when no element is mounted', async () => {
		const onPlaybackStarted = vi.fn();
		await createPlayback({ audio: () => undefined, flush: () => Promise.resolve(), onPlaybackStarted }).play();
		expect(onPlaybackStarted).not.toHaveBeenCalled();
	});

	it('ignores a rejection from a replaced element without a newer intent', async () => {
		const pending = deferred();
		const play = vi.fn(() => pending.promise);
		let element = audio(play);
		const onPlaybackFailure = vi.fn();
		const playback = createPlayback({ audio: () => element, flush: () => Promise.resolve(), onPlaybackFailure });
		const attempt = playback.play();
		await Promise.resolve();
		expect(play).toHaveBeenCalledTimes(1);
		element = audio(() => Promise.resolve());
		pending.reject(new Error('old resource failed'));
		await attempt;
		expect(onPlaybackFailure).not.toHaveBeenCalled();
	});

	it('ignores a success from a replaced element', async () => {
		const pending = deferred();
		let element = audio(() => pending.promise);
		const onPlaybackStarted = vi.fn();
		const playback = createPlayback({ audio: () => element, flush: () => Promise.resolve(), onPlaybackStarted });
		const attempt = playback.play();
		await Promise.resolve();
		element = audio(() => Promise.resolve());
		pending.resolve();
		await attempt;
		expect(onPlaybackStarted).not.toHaveBeenCalled();
	});
});
