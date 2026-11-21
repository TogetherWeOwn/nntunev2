import { afterEach, expect, onTestFinished, test, vi } from 'vitest';
import { get } from 'svelte/store';
import { createPlayerController } from './controller.svelte.js';
import { createMediaController } from './mediaController.js';
import type { MediaMetadataInit, MediaSessionLike } from './mediaSession.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';

const actions = ['seekto', 'seekforward', 'seekbackward'] as const;
const coral = { id: 'coral', title: 'Coral 🎵', artist: 'Fixture Artist', album: 'Fixture Album', streamUrl: '/coral' };
const moss = { id: 'moss', title: ' ', artist: ' ', streamUrl: '/moss' };

class Decoder extends EventTarget {
	isConnected = true;
	paused = true;
	ended = false;
	error: { code: number } | null = null;
	position = 20;
	seeking = false;
	duration = NaN;
	ranges: [number, number][] = [];
	beforeRead?: () => void;
	write = vi.fn((value: number) => { this.position = value; });
	get currentTime() { this.beforeRead?.(); return this.position; }
	set currentTime(value: number) { this.write(value); }
	get seekable() {
		const { ranges } = this;
		return { length: ranges.length, start: (i: number) => ranges[i]![0], end: (i: number) => ranges[i]![1] };
	}
	emit(...types: string[]) { for (const type of types) this.dispatchEvent(new Event(type)); }
	setReady(ranges: [number, number][] = [[0, 60]]) {
		Object.assign(this, { duration: 60, ranges, HAVE_METADATA: 1, readyState: 1, playbackRate: 1 });
		this.emit('loadedmetadata');
	}
	constructor(readonly source: string) { super(); }
	getAttribute() { return this.source; }
	fastSeek = vi.fn();
	load = vi.fn(() => { this.error = null; });
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
	const handlers = new Map<string, Parameters<MediaSessionLike['setActionHandler']>[1]>();
	const activeActions = new Set<string>();
	const session: MediaSessionLike = {
		metadata: null,
		setPositionState: vi.fn(),
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

test('native and compact Play leave a terminal error for explicit Retry', async () => {
	const { handler } = stubSession();
	const { media, audio, status } = player();
	audio.error = { code: 4 };
	audio.dispatchEvent(new Event('error'));
	expect(status()).toBe('error');

	handler('play')!();
	await media.playCurrent();
	expect(audio.load).not.toHaveBeenCalled();
	expect(audio.play).not.toHaveBeenCalled();
	expect(status()).toBe('error');

	await media.retryCurrent();
	expect(audio.load).toHaveBeenCalledOnce();
	expect(audio.play).toHaveBeenCalledOnce();
	expect(status()).toBe('playing');
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
		play();
		expect(newer.play).not.toHaveBeenCalled();
		handler('play')!();
		expect(newer.play).toHaveBeenCalledOnce();
	} else {
		play();
	}
	pause();
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
	partial.media.pauseCurrent();
	expect(partial.status()).toBe('paused');
	await partial.media.playCurrent();
	expect(partial.status()).toBe('playing');
	handler('pause')!();
	expect(partial.status()).toBe('paused');
	partial.media.destroy();
});

test.each(['nexttrack', 'previoustrack'] as const)('unsupported %s leaves the other queue action usable', (unsupported) => {
	const { handler } = stubSession(unsupported);
	const next = unsupported === 'previoustrack';
	const base = { ...createQueue([coral, moss]), currentIndex: next ? 0 : 1 };
	const { queue, media, reconcileLibrary, purgeAtAuthBoundary, destroy } = createPlayerController(base, { flush: async () => {} }, () => true);
	const audio = new Decoder(base.tracks[base.currentIndex].streamUrl);
	try {
		expect(reconcileLibrary(base.tracks, () => true)).toBe(true);
		media.attach(audio.element);
		expect(handler(unsupported)).toBeUndefined();
		expect(queue.snapshot).toBe(base);
		handler(next ? 'nexttrack' : 'previoustrack')!();
		expect(queue.snapshot.currentIndex).toBe(next ? 1 : 0);
		expect(queue.snapshot.tracks).toBe(base.tracks);
		expect(audio.play).not.toHaveBeenCalled();
	} finally {
		purgeAtAuthBoundary();
		destroy();
	}
});

function setup({ unsupported, connected = true, ready = connected }: { unsupported?: string; connected?: boolean; ready?: boolean } = {}) {
	const { session, handler } = stubSession(unsupported);
	const queue = createQueueController(createQueue([coral, moss]));
	const commit = (next: typeof queue.snapshot) => {
		queue.publish(next, queue.begin(next, 'replace'));
		return queue.snapshot === next;
	};
	const media = createMediaController(queue, { commit, flush: async () => {}, browseCopy: { invalidate() {} }, announce() {} }, () => true);
	const audio = new Decoder(coral.streamUrl);
	audio.isConnected = connected;
	const detach = media.attach(audio.element);
	if (ready) audio.setReady();
	onTestFinished(() => media.destroy());
	return { handler, session, media, audio, detach, queue, select: (track: typeof moss) => commit(createQueue([track])),
		fire: (action: typeof actions[number], details: object = {}) => handler(action)!({ action, ...details }) };
}

test.each([['seekto', false], ['seekforward', false], ['seekto', true]] as const)('system seek readback and %s endpoint (superseded: %s)', async (endpoint, superseded) => {
	const { audio, media, session, fire, queue } = setup();
	const initial = queue.snapshot;
	vi.mocked(session.setPositionState)!.mockClear();
	for (const [action, details, target] of [
		['seekto', { seekTime: 12.25, fastSeek: true }, 12.25], ['seekforward', {}, 22.25],
		['seekbackward', { seekOffset: 2.5 }, 19.75], ['seekforward', { seekOffset: 100 }, 60],
		['seekbackward', { seekOffset: 100 }, 0], ['seekto', { seekTime: 100 }, 60], ['seekto', { seekTime: 0 }, 0]
	] as const) {
		fire(action, details);
		expect(audio.currentTime).toBe(target);
		if (action === 'seekto') expect(get(media.absoluteSeek).position).toBe(target);
	}
	expect(audio.paused).toBe(true);
	expect(audio.play).not.toHaveBeenCalled();
	await media.playCurrent();
	fire('seekto', { seekTime: 12.25 });
	expect(audio).toMatchObject({ currentTime: 12.25, paused: false });
	for (const unused of [audio.pause, audio.fastSeek, session.setPositionState]) expect(unused).not.toHaveBeenCalled();
	audio.emit('seeked');
	expect(session.setPositionState).toHaveBeenCalledExactlyOnceWith({ duration: 60, position: 12.25, playbackRate: 1 });
	audio.emit('playing');
	audio.write.mockImplementationOnce((target) => { audio.position = target - 0.000001; });
	fire(endpoint, { seekTime: 60, seekOffset: 100 });
	expect(audio.currentTime).toBe(59.999999);
	if (superseded) media.seek(get(media.absoluteSeek).attachment, 59.9999995);
	audio.emit('seeking', 'seeked');
	Object.assign(audio, { position: 60, ended: true, paused: true });
	audio.emit('pause', 'ended');
	if (superseded) expect(queue.snapshot.currentIndex).toBe(1);
	else expect(queue.snapshot).toBe(initial);
	expect(audio.play).toHaveBeenCalledOnce();
	expect(audio.load).not.toHaveBeenCalled();
	expect(get(media.playbackStatus)).toBe('paused');
});

test.each([['seekforward', 59.98, 1, 'Forward'], ['seekbackward', 0.02, -1, 'Back']] as const)('%s distinguishes a fractional move from a no-op', (action, edge, sign, word) => {
	const { audio, media, fire } = setup();
	fire(action, { seekOffset: 1e-16 });
	expect(audio.write).not.toHaveBeenCalled();
	expect(get(media.seekFeedback).jumpStatus).toBe('The player did not accept that jump.');
	for (const drift of [0, 0.001]) {
		audio.write.mockClear().mockImplementationOnce(() => { audio.position += sign * drift; });
		fire(action, { seekOffset: 0.01 });
		expect(audio.write).toHaveBeenCalledExactlyOnceWith(20 + sign * 0.01);
		expect(get(media.seekFeedback).jumpStatus).toBe('The player did not accept that jump.');
	}
	audio.position = edge;
	fire(action, { seekOffset: 0.01 });
	expect(audio.currentTime).toBe(edge + sign * 0.01);
	expect(get(media.seekFeedback).jumpStatus).toContain(`${word} 0.01 seconds`);
});

test('invalid inputs leave time unchanged and native ranges govern seek registration', () => {
	const { audio, media, handler, fire } = setup({ ready: false });
	audio.setReady([[0, 0]]);
	for (const action of actions) expect(handler(action)).toBeFalsy();
	audio.setReady([[0, 24], [40, 60]]);
	const before = get(media.seekFeedback);
	for (const action of actions) {
		const field = action === 'seekto' ? 'seekTime' : 'seekOffset';
		for (const value of [NaN, Infinity, -1, '5', null, true, {}, action === 'seekto' ? undefined : 0]) fire(action, { [field]: value });
		fire(action, { action: 'play', seekTime: 5, seekOffset: 5 });
	}
	expect(audio.write).not.toHaveBeenCalled();
	expect(get(media.seekFeedback)).toBe(before);
	expect(audio.play).not.toHaveBeenCalled();
	fire('seekto', { seekTime: 30 });
	expect(get(media.absoluteSeek).feedback).toBe('That position is not seekable yet.');
	fire('seekforward', { seekOffset: 4.02 });
	expect(get(media.seekFeedback).jumpStatus).toBe('Cannot jump: this track cannot seek yet.');
	expect(audio.currentTime).toBe(20);
	for (const action of actions) expect(handler(action)).toBeTypeOf('function');
});

test.each([
	['seekto', 0, false], ['seekto', 1, false], ['seekforward', 1, false],
	['seekto', 0, true], ['seekto', 1, true], ['seekto', 2, true], ['seekforward', 1, true]
] as const)('%s retires after loss (%i valid reads, competing intent: %s)', (action, validReads, competing) => {
	const { audio, media, handler, fire } = setup();
	fire('seekforward', { seekOffset: 1 }); // Seed a prior success message.
	audio.position = 20;
	const retired = actions.map((action) => handler(action)!);
	const ranges = audio.seekable;
	let reads = 0;
	const seekable = vi.spyOn(audio, 'seekable', 'get').mockImplementation(() => {
		const read = reads++;
		if (competing && read === validReads) media.seek(get(media.absoluteSeek).attachment, 30);
		return { ...ranges, length: Number(read < validReads || (competing && read > validReads)) };
	});
	fire(action, { seekTime: 10, seekOffset: 3 });
	const position = competing ? 30 : 20;
	expect(audio.currentTime).toBe(position);
	if (!competing) for (const action of actions) expect(handler(action)).toBeNull();
	if (validReads && !competing) {
		if (action === 'seekto') expect(get(media.absoluteSeek).feedback).toBe('That position is not seekable yet.');
		else expect(get(media.seekFeedback).jumpStatus).toBe('Cannot jump: this track cannot seek yet.');
	}
	seekable.mockRestore();
	audio.setReady(); // No native event ever published the loss.
	for (const [i, action] of actions.entries()) {
		retired[i]!({ action, seekTime: 10, seekOffset: 1 });
		expect(audio.currentTime).toBe(position);
		expect(handler(action)).not.toBe(retired[i]);
	}
	fire('seekto', { seekTime: 10 });
	expect(audio.currentTime).toBe(10);
});

test.each(['seekto', 'seekforward'] as const)('%s respects registration and intent during reentrant seeks', (action) => {
	const { audio, media, fire } = setup();
	const compact = (target: number) => media.seek(get(media.absoluteSeek).attachment, target);
	const system = () => fire(action, { seekTime: 10, seekOffset: 5 });
	audio.beforeRead = () => { audio.beforeRead = undefined; compact(30); };
	system();
	expect(audio.currentTime).toBe(30); // Preflight cannot supersede the newer compact write.
	audio.write.mockClear().mockImplementationOnce((seconds) => {
		system();
		audio.setReady([]);
		audio.setReady();
		audio.position = seconds;
	});
	compact(40);
	expect(audio.write.mock.calls).toEqual([[40]]); // Recovery did not revive the queued write.
});

test('late publication binds supported seek actions and preserves both Play controls', async () => {
	const { audio, media, select, handler, fire } = setup({ unsupported: 'seekto', connected: false });
	expect(handler('play')).toBeUndefined();
	audio.isConnected = true;
	select({ ...coral });
	audio.setReady();
	expect(handler('seekto')).toBeUndefined();
	fire('seekbackward');
	expect(audio.currentTime).toBe(10);
	await media.playCurrent();
	expect(audio.play).toHaveBeenCalledOnce();
	media.pauseCurrent();
	expect(audio.paused).toBe(true);
	handler('play')!();
	expect(audio.play).toHaveBeenCalledTimes(2);
});

test.each(['decoder', 'source', 'auth', 'detach'])('fences all queued callbacks across %s changes', (boundary) => {
	const { audio, media, queue, select, detach, handler, fire } = setup();
	const retired = actions.map((action) => handler(action)!);
	if (boundary === 'auth') {
		queue.purgeAtAuthBoundary(() => media.purgeAtAuthBoundary(() => {}));
		select(coral);
	} else if (boundary === 'source') {
		select(moss); // Source changes before the old audio detaches.
		for (const action of actions) expect(handler(action)).toBeNull();
		select(coral); // Round-trip must not revive old callbacks.
	} else if (boundary === 'detach') detach();
	const current = boundary === 'source' ? audio : new Decoder(coral.streamUrl);
	if (current !== audio) {
		media.attach(current.element);
		for (const action of actions) expect(handler(action)).toBeNull();
	}
	current.setReady();
	for (const [i, action] of actions.entries()) {
		retired[i]!({ action, seekTime: 2, seekOffset: 5 });
		expect([audio.currentTime, current.currentTime]).toEqual([20, 20]);
	}
	fire('seekto', { seekTime: 6 });
	expect(current.currentTime).toBe(6);
});
