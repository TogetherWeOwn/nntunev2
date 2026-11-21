import { get } from 'svelte/store';
import { expect, test, vi } from 'vitest';
import { createMediaController } from './mediaController.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';

const track = { id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral' };

class RetryAudio extends EventTarget {
	isConnected = true;
	paused = true;
	ended = false;
	error: { code: number } | null = null;
	getAttribute() { return track.streamUrl; }
	load = vi.fn(() => { this.error = null; });
	play = vi.fn(async () => { this.paused = false; });
	pause = vi.fn(() => { this.paused = true; });
	get element() { return this as unknown as HTMLAudioElement; }
	fail() {
		this.error = { code: 4 };
		this.dispatchEvent(new Event('error'));
	}
}

function setup() {
	const queue = createQueueController(createQueue([track]));
	const media = createMediaController(queue, undefined, (audio, src) => audio.getAttribute('src') === src);
	const audio = new RetryAudio();
	return { queue, media, audio, status: () => get(media.playbackStatus) };
}

test('unavailable and refused Play cannot trigger a reload', async () => {
	const { media, audio, status } = setup();
	await media.retryCurrent();
	expect(status()).toBe('unavailable');
	media.attach(audio.element);
	await media.retryCurrent();
	expect(audio.play).not.toHaveBeenCalled();
	audio.play.mockRejectedValueOnce(new DOMException('Refused', 'NotAllowedError'));
	await media.playCurrent();
	expect(status()).toBe('blocked');
	await media.retryCurrent();
	expect(audio.play).toHaveBeenCalledOnce();
	expect(audio.load).not.toHaveBeenCalled();
	await media.playCurrent();
	expect(status()).toBe('playing');
	expect(audio.load).not.toHaveBeenCalled();
});

test('only an explicit gesture reloads the decoder, once per attempt, and a second failure offers Retry again', async () => {
	const { media, audio, status } = setup();
	media.attach(audio.element);
	audio.fail();
	await media.playCurrent();
	expect(status()).toBe('error');
	expect(audio.load).not.toHaveBeenCalled();
	expect(audio.play).not.toHaveBeenCalled();

	const first = Promise.withResolvers<void>();
	audio.play.mockReturnValueOnce(first.promise);
	const retrying = media.retryCurrent();
	await media.retryCurrent();
	await media.playCurrent();
	expect(status()).toBe('retrying');
	expect(audio.load).toHaveBeenCalledOnce();
	expect(audio.play).toHaveBeenCalledOnce();
	audio.fail();
	first.reject(new DOMException('Bad resource', 'NotSupportedError'));
	await retrying;
	expect(status()).toBe('error');
	expect(audio.load).toHaveBeenCalledOnce();

	await media.retryCurrent();
	expect(audio.load).toHaveBeenCalledTimes(2);
	expect(audio.play).toHaveBeenCalledTimes(2);
	expect(media.audio).toBe(audio.element);
	expect(status()).toBe('playing');
});

test('a refused Retry on a successfully reset resource returns to Play, not reload', async () => {
	const { media, audio, status } = setup();
	media.attach(audio.element);
	audio.fail();
	audio.play.mockRejectedValueOnce(new DOMException('Refused', 'NotAllowedError'));
	await media.retryCurrent();
	expect(status()).toBe('blocked');
	await media.retryCurrent();
	await media.playCurrent();
	expect(audio.load).toHaveBeenCalledOnce();
	expect(audio.play).toHaveBeenCalledTimes(2);
	expect(status()).toBe('playing');
});

test('an interrupted Retry settles as paused and leaves the reset resource playable', async () => {
	const { media, audio, status } = setup();
	media.attach(audio.element);
	audio.fail();
	audio.play.mockRejectedValueOnce(new DOMException('Interrupted', 'AbortError'));
	await media.retryCurrent();
	expect(status()).toBe('paused');
	await media.playCurrent();
	expect(audio.load).toHaveBeenCalledOnce();
	expect(audio.play).toHaveBeenCalledTimes(2);
	expect(status()).toBe('playing');
});

test.each(['same URL replacement', 'auth purge'] as const)('stale Retry does not affect playback after %s', async (boundary) => {
	const { media, queue, audio, status } = setup();
	const detach = media.attach(audio.element);
	audio.fail();
	const pending = Promise.withResolvers<void>();
	audio.play.mockImplementationOnce(() => { audio.paused = false; return pending.promise; });
	const retrying = media.retryCurrent();
	expect(audio.load).toHaveBeenCalledOnce();
	const replacement = new RetryAudio();
	if (boundary === 'same URL replacement') {
		const next = createQueue([{ ...track, id: 'new-selection' }]);
		queue.publish(next, queue.begin(next, 'replace'));
		media.attach(replacement.element);
	} else {
		queue.purgeAtAuthBoundary(() => media.purgeAtAuthBoundary(() => {}));
	}
	detach();
	audio.fail();
	pending.resolve();
	await retrying;
	await media.retryCurrent();
	expect(audio.pause).toHaveBeenCalledOnce();
	expect(audio.load).toHaveBeenCalledOnce();
	expect(status()).toBe(boundary === 'auth purge' ? 'unavailable' : 'paused');
	expect(replacement.load).not.toHaveBeenCalled();
	expect(replacement.play).not.toHaveBeenCalled();
});

test('a late rejection from the failed Play cannot settle the newer Retry', async () => {
	const { media, audio, status } = setup();
	media.attach(audio.element);
	const old = Promise.withResolvers<void>();
	audio.play.mockReturnValueOnce(old.promise);
	const playing = media.playCurrent();
	audio.fail();
	const next = Promise.withResolvers<void>();
	audio.play.mockImplementationOnce(() => { audio.paused = false; return next.promise; });
	const retrying = media.retryCurrent();
	old.reject(new DOMException('Bad resource', 'NotSupportedError'));
	await playing;
	expect(status()).toBe('retrying');
	next.resolve();
	await retrying;
	expect(status()).toBe('playing');
});

test('Retry checks auth after publishing its pending state and before touching the decoder', async () => {
	const { media, queue, audio, status } = setup();
	media.attach(audio.element);
	audio.fail();
	const unsubscribe = media.playbackStatus.subscribe((state) => {
		if (state === 'retrying') queue.purgeAtAuthBoundary(() => media.purgeAtAuthBoundary(() => {}));
	});
	await media.retryCurrent();
	unsubscribe();
	expect(status()).toBe('unavailable');
	expect(audio.load).not.toHaveBeenCalled();
	expect(audio.play).not.toHaveBeenCalled();
});
