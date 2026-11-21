import { get } from 'svelte/store';
import { afterEach, expect, test, vi } from 'vitest';
import { createMediaController } from './mediaController.js';
import type { MediaSessionLike, MediaSessionSeekDetails } from './mediaSession.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';

const actions = ['seekto', 'seekforward', 'seekbackward'] as const;
const track = { id: 'one', title: 'One', artist: '', streamUrl: '/one' };
const other = { id: 'two', title: 'Two', artist: '', streamUrl: '/two' };

class Decoder extends EventTarget {
	isConnected = true;
	paused = true;
	ended = false;
	error = null;
	seeking = false;
	currentTime = 20;
	duration = NaN;
	ranges: [number, number][] = [];
	get seekable() {
		return { length: this.ranges.length, start: (i: number) => this.ranges[i]![0], end: (i: number) => this.ranges[i]![1] };
	}
	constructor(public source: string) { super(); }
	getAttribute() { return this.source; }
	setReady(ranges: [number, number][] = [[0, 60]]) {
		this.duration = 60;
		this.ranges = ranges;
		this.dispatchEvent(new Event('loadedmetadata'));
	}
	play = vi.fn(async () => { this.paused = false; });
	pause = vi.fn(() => { this.paused = true; });
	load = vi.fn();
	fastSeek = vi.fn();
	get element() { return this as unknown as HTMLAudioElement; }
}

function setup(unsupported?: string) {
	const handlers = new Map<string, Parameters<MediaSessionLike['setActionHandler']>[1]>();
	const session: MediaSessionLike = {
		metadata: null,
		setPositionState: vi.fn(),
		setActionHandler(action, handler) {
			if (action === unsupported) throw new DOMException('unsupported', 'NotSupportedError');
			handlers.set(action, handler);
		}
	};
	vi.stubGlobal('navigator', { mediaSession: session });
	const queue = createQueueController(createQueue([track]));
	const media = createMediaController(queue, undefined, () => true);
	const audio = new Decoder(track.streamUrl);
	const detach = media.attach(audio.element);
	const select = (selected: typeof track) => {
		const next = createQueue([selected]);
		queue.publish(next, queue.begin(next, 'replace'));
	};
	const fire = (action: typeof actions[number], details: object = {}) => {
		expect(handlers.get(action)).toBeTypeOf('function');
		handlers.get(action)!({ action, ...details } as MediaSessionSeekDetails);
	};
	return { handlers, session, media, audio, detach, queue, select, fire };
}

afterEach(() => vi.unstubAllGlobals());

test('system seeks use owner readback, clamping and feedback without starting or reloading audio', async () => {
	const { audio, media, session, fire } = setup();
	audio.setReady();
	vi.mocked(session.setPositionState)!.mockClear();
	fire('seekto', { seekTime: 12.25, fastSeek: true });
	expect(get(media.absoluteSeek).position).toBe(12.25);
	fire('seekforward');
	expect(audio.currentTime).toBe(22.25);
	fire('seekbackward', { seekOffset: 2.5 });
	expect(audio.currentTime).toBe(19.75);
	expect(get(media.seekFeedback).jumpStatus).toMatch(/^Back 2\.5 seconds/);
	fire('seekforward', { seekOffset: 100 });
	expect(audio.currentTime).toBe(60);
	fire('seekbackward', { seekOffset: 100 });
	expect(audio.currentTime).toBe(0);
	fire('seekto', { seekTime: 100 });
	expect(get(media.absoluteSeek).position).toBe(60);
	fire('seekto', { seekTime: 0 });
	expect(audio.currentTime).toBe(0);
	expect(audio.paused).toBe(true);
	expect(audio.play).not.toHaveBeenCalled();
	await media.playCurrent();
	fire('seekforward', { seekOffset: 1 });
	expect(audio.currentTime).toBe(1);
	expect(audio.paused).toBe(false);
	expect(audio.play).toHaveBeenCalledOnce();
	expect(audio.pause).not.toHaveBeenCalled();
	expect(audio.load).not.toHaveBeenCalled();
	expect(audio.fastSeek).not.toHaveBeenCalled();
	expect(session.setPositionState).not.toHaveBeenCalled();
	media.destroy();
});

test('malformed details leave the native position and feedback unchanged', () => {
	const { audio, media, fire } = setup();
	audio.setReady();
	const feedback = get(media.seekFeedback);
	for (const action of actions) {
		const field = action === 'seekto' ? 'seekTime' : 'seekOffset';
		for (const value of [NaN, Infinity, -1, '5', null, true, {}, action === 'seekto' ? undefined : 0]) {
			fire(action, { [field]: value });
			expect(audio.currentTime).toBe(20);
		}
		fire(action, { action: 'play', seekTime: 5, seekOffset: 5 });
		expect(audio.currentTime).toBe(20);
	}
	expect(get(media.seekFeedback)).toBe(feedback);
	expect(audio.play).not.toHaveBeenCalled();
	media.destroy();
});

test('unseekable media has no handlers and retired registrations cannot revive', () => {
	const { audio, media, handlers, fire } = setup();
	for (const action of actions) expect(handlers.get(action)).toBeFalsy();
	audio.setReady([[0, 0]]);
	for (const action of actions) expect(handlers.get(action)).toBeFalsy();
	audio.setReady([[0, 24], [40, 60]]);
	fire('seekto', { seekTime: 30 });
	expect(get(media.absoluteSeek).feedback).toBe('That position is not seekable yet.');
	fire('seekforward');
	expect(get(media.seekFeedback).jumpStatus).toBe('Cannot jump: this track cannot seek yet.');
	const retired = handlers.get('seekto')!;
	audio.ranges = [];
	retired({ action: 'seekto', seekTime: 10 }); // Lost capability, before its native event.
	expect(audio.currentTime).toBe(20);
	audio.dispatchEvent(new Event('progress'));
	for (const action of actions) expect(handlers.get(action)).toBeNull();
	audio.setReady();
	retired({ action: 'seekto', seekTime: 10 });
	expect(audio.currentTime).toBe(20);
	fire('seekto', { seekTime: 10 });
	expect(audio.currentTime).toBe(10);
	media.destroy();
});

test('one unsupported action does not break another action or compact Play', async () => {
	const { audio, media, handlers, fire } = setup('seekto');
	audio.setReady();
	expect(handlers.get('seekto')).toBeUndefined();
	fire('seekbackward');
	expect(audio.currentTime).toBe(10);
	handlers.get('play')!();
	await media.playCurrent();
	expect(audio.play).toHaveBeenCalledOnce();
	media.destroy();
});

test.each(['decoder', 'source', 'auth', 'detach'])('fences all queued callbacks across %s changes', (boundary) => {
	const { audio, media, queue, select, detach, handlers, fire } = setup();
	audio.setReady();
	const retired = actions.map((action) => handlers.get(action)!);
	if (boundary === 'auth') {
		queue.purgeAtAuthBoundary(() => media.purgeAtAuthBoundary(() => {}));
		select(track);
	} else if (boundary === 'source') {
		select(other); // Source changes before the old audio detaches.
		for (const action of actions) expect(handlers.get(action)).toBeNull();
		select(track); // Round-trip must not revive old callbacks.
	} else if (boundary === 'detach') {
		detach();
	}
	const current = boundary === 'source' ? audio : new Decoder(track.streamUrl);
	if (current !== audio) {
		media.attach(current.element);
		for (const action of actions) expect(handlers.get(action)).toBeNull();
	}
	current.setReady();
	for (const [i, action] of actions.entries()) {
		retired[i]!({ action, seekTime: 2, seekOffset: 5 });
		expect(audio.currentTime).toBe(20);
		expect(current.currentTime).toBe(20);
	}
	fire('seekto', { seekTime: 6 });
	expect(current.currentTime).toBe(6);
	media.destroy();
});
