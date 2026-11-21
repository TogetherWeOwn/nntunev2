import { afterEach, expect, test, vi } from 'vitest';
import { get } from 'svelte/store';
import { createMediaController } from './mediaController.js';
import type { MediaMetadataInit, MediaSessionLike } from './mediaSession.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';

const coral = { id: 'coral', title: 'Coral 🎵', artist: 'Fixture Artist', album: 'Fixture Album', streamUrl: '/coral' };
const moss = { id: 'moss', title: ' ', artist: ' ', streamUrl: '/moss' };

class Decoder extends EventTarget {
	isConnected = true;
	paused = true;
	ended = false;
	error = null;
	constructor(readonly source: string) { super(); }
	getAttribute() { return this.source; }
	play = vi.fn(async () => { this.paused = false; });
	pause = vi.fn(() => { this.paused = true; });
	get element() { return this as unknown as HTMLAudioElement; }
}

function player() {
	const queue = createQueueController(createQueue([coral]));
	const media = createMediaController(queue, undefined, () => true);
	const audio = new Decoder(coral.streamUrl);
	const detach = media.attach(audio.element);
	return { queue, media, audio, detach, status: () => get(media.playbackStatus) };
}

function stubSession(unsupported?: string) {
	const handlers = new Map<string, (() => void) | null>();
	const activeActions = new Set<string>();
	const session: MediaSessionLike = {
		metadata: null,
		setActionHandler(action, handler) {
			if (action === unsupported) throw new DOMException('unsupported', 'NotSupportedError');
			if (handler) activeActions.add(action);
			handlers.set(action, handler);
		}
	};
	vi.stubGlobal('navigator', { mediaSession: session });
	vi.stubGlobal('MediaMetadata', class {
		constructor(init: MediaMetadataInit) { Object.assign(this, init); }
	});
	return { session, activeActions, handler: (action: string) => handlers.get(action) };
}

afterEach(() => vi.unstubAllGlobals());

test('the native attachment publishes only Play/Pause and updates its current metadata in place', async () => {
	const { session, activeActions, handler } = stubSession();
	const { queue, media, audio, status } = player();
	const play = handler('play')!;
	expect(activeActions).toEqual(new Set(['play', 'pause']));
	expect(session.metadata).toMatchObject({ title: coral.title, artist: coral.artist, album: coral.album });
	const updated = createQueue([{ ...coral, title: 'Updated title' }, moss]);
	queue.publish(updated, queue.begin(updated, 'replace'));
	expect(session.metadata).toMatchObject({ title: 'Updated title' });
	expect(media.audio).toBe(audio.element);

	play();
	await media.playCurrent(); // Also a compact Play: no second native request.
	expect(audio.play).toHaveBeenCalledOnce();
	expect(status()).toBe('playing');
	handler('pause')!();
	expect(audio.pause).toHaveBeenCalledOnce();
	expect(status()).toBe('paused');
	media.destroy();
});

test.each(['replacement', 'detach', 'account purge', 'destroy'])('revokes captured Play/Pause before %s can drive a decoder', async (end) => {
	const { session, handler } = stubSession();
	const { queue, media, audio, detach, status } = player();
	const play = handler('play')!, pause = handler('pause')!;
	const pending = Promise.withResolvers<void>();
	audio.play.mockImplementationOnce(() => { audio.paused = false; return pending.promise; });
	play();
	expect(status()).toBe('starting');

	if (end === 'detach') detach();
	else if (end === 'destroy') media.destroy();
	else if (end === 'account purge') queue.purgeAtAuthBoundary(() => {
		media.purgeAtAuthBoundary(() => {
			expect(session.metadata).toBeNull();
			expect(handler('play')).toBeNull();
		});
	});
	else media.reset();
	expect(session.metadata).toBeNull();
	expect(handler('play')).toBeNull();
	expect(handler('pause')).toBeNull();
	const newer = new Decoder(moss.streamUrl);
	if (end === 'replacement') {
		const next = createQueue([moss]);
		queue.publish(next, queue.begin(next, 'replace'));
		media.attach(newer.element);
		// The retired DOM attachment's eventual cleanup must not clear its successor.
		detach();
		expect(session.metadata).toMatchObject({ title: moss.id, artist: '' });
		expect(session.metadata).not.toHaveProperty('album');
		await media.playCurrent();
	}
	pause();
	play();
	pending.resolve();
	await pending.promise;
	expect(audio.play).toHaveBeenCalledOnce();
	expect(audio.pause).toHaveBeenCalledOnce();
	expect(newer.pause).not.toHaveBeenCalled();
	expect(newer.play).toHaveBeenCalledTimes(end === 'replacement' ? 1 : 0);
	expect(status()).toBe(end === 'replacement' ? 'playing' : 'unavailable');
	media.destroy();
});

test('compact controls work with Media Session absent or one action unsupported', async () => {
	vi.stubGlobal('navigator', {});
	const unsupported = player();
	await unsupported.media.playCurrent();
	expect(unsupported.status()).toBe('playing');
	unsupported.media.pauseCurrent();
	expect(unsupported.status()).toBe('paused');
	unsupported.media.destroy();

	const { handler } = stubSession('play');
	const partial = player();
	await partial.media.playCurrent();
	expect(partial.status()).toBe('playing');
	handler('pause')!();
	expect(partial.status()).toBe('paused');
	partial.media.destroy();
});
