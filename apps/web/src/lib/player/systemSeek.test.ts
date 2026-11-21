import { get } from 'svelte/store';
import { expect, onTestFinished, test, vi } from 'vitest';
import { createMediaController } from './mediaController.js';
import type { MediaSessionLike } from './mediaSession.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';

const actions = ['seekto', 'seekforward', 'seekbackward'] as const;
const track = { id: 'one', title: 'One', artist: '', streamUrl: '/one' };
const other = { id: 'two', title: 'Two', artist: '', streamUrl: '/two' };

class Decoder extends EventTarget {
	paused = true;
	ended = false;
	error = null;
	seeking = false;
	position = 20;
	beforeRead?: () => void;
	write = (value: number) => { this.position = value; };
	get currentTime() { this.beforeRead?.(); return this.position; }
	set currentTime(value: number) { this.write(value); }
	duration = NaN;
	ranges: [number, number][] = [];
	get seekable() {
		const { ranges } = this;
		return { length: ranges.length, start: (i: number) => ranges[i]![0], end: (i: number) => ranges[i]![1] };
	}
	constructor(public source: string, public isConnected = true) { super(); }
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

function setup({ unsupported, connected = true, ready = connected }: { unsupported?: string; connected?: boolean; ready?: boolean } = {}) {
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
	const audio = new Decoder(track.streamUrl, connected);
	const detach = media.attach(audio.element);
	if (ready) audio.setReady();
	onTestFinished(() => { media.destroy(); vi.unstubAllGlobals(); });
	const select = (selected: typeof track) => {
		const next = createQueue([selected]);
		queue.publish(next, queue.begin(next, 'replace'));
	};
	const fire = (action: typeof actions[number], details: object = {}) => handlers.get(action)!({ action, ...details });
	return { handlers, session, media, audio, detach, queue, select, fire };
}

test('system seeks use owner readback, clamping and feedback without starting or reloading audio', async () => {
	const { audio, media, session, fire } = setup();
	vi.mocked(session.setPositionState)!.mockClear();
	for (const [action, details, target] of [
		['seekto', { seekTime: 12.25, fastSeek: true }, 12.25], ['seekforward', {}, 22.25],
		['seekbackward', { seekOffset: 2.5 }, 19.75], ['seekforward', { seekOffset: 100 }, 60],
		['seekbackward', { seekOffset: 100 }, 0], ['seekto', { seekTime: 100 }, 60], ['seekto', { seekTime: 0 }, 0]
	] as const) {
		fire(action, details);
		expect(audio.currentTime).toBe(target);
		if (action === 'seekto') expect(get(media.absoluteSeek).position).toBe(target);
		if (target === 19.75) expect(get(media.seekFeedback).jumpStatus).toMatch(/^Back 2\.5 seconds/);
	}
	expect(audio.paused).toBe(true);
	expect(audio.play).not.toHaveBeenCalled();
	await media.playCurrent();
	fire('seekforward', { seekOffset: 1 });
	expect(audio.currentTime).toBe(1);
	expect(audio.paused).toBe(false);
	expect(audio.play).toHaveBeenCalledOnce();
	for (const unused of [audio.pause, audio.load, audio.fastSeek, session.setPositionState]) expect(unused).not.toHaveBeenCalled();
});

test.each([['seekforward', 59.98, 1, 'Forward'], ['seekbackward', 0.02, -1, 'Back']] as const)(
	'%s accepts small steps near edges but rejects drift or no-op readback', (action, edge, sign, word) => {
		const { audio, media, fire } = setup();
		for (const drift of [0, 0.001]) {
			audio.position = 20;
			audio.write = vi.fn(() => { audio.position += sign * drift; });
			fire(action, { seekOffset: 0.01 });
			expect(audio.write).toHaveBeenCalledWith(20 + sign * 0.01);
			expect(get(media.seekFeedback).jumpStatus).toBe('The player did not accept that jump.');
		}
		audio.position = edge;
		audio.write = (value) => { audio.position = value; };
		fire(action, { seekOffset: 0.01 });
		expect(audio.currentTime).toBe(edge + sign * 0.01);
		expect(get(media.seekFeedback).jumpStatus).toContain(`${word} 0.01 seconds`);
	}
);

test('malformed details leave the native position and feedback unchanged', () => {
	const { audio, media, fire } = setup();
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
});

test('only positive-width ranges advertise and admit seeks', () => {
	const { audio, media, handlers, fire } = setup({ ready: false });
	for (const action of actions) expect(handlers.get(action)).toBeFalsy();
	audio.setReady([[0, 0]]);
	for (const action of actions) expect(handlers.get(action)).toBeFalsy();
	audio.setReady([[0, 24], [40, 60]]);
	fire('seekto', { seekTime: 30 });
	expect(get(media.absoluteSeek).feedback).toBe('That position is not seekable yet.');
	fire('seekforward', { seekOffset: 4.02 });
	expect(get(media.seekFeedback).jumpStatus).toBe('Cannot jump: this track cannot seek yet.');
	expect(audio.currentTime).toBe(20);
	for (const action of actions) expect(handlers.get(action)).toBeTypeOf('function');
});

test.each([
	['seekto', 0, false], ['seekto', 1, false], ['seekforward', 1, false],
	['seekto', 0, true], ['seekto', 1, true], ['seekto', 2, true], ['seekforward', 1, true]
] as const)('%s retires after loss (%i valid reads, competing intent: %s)', (action, validReads, competing) => {
	const { audio, media, handlers, fire } = setup();
	fire('seekforward', { seekOffset: 1 }); // Seed a prior success message.
	audio.position = 20;
	const retired = actions.map((action) => handlers.get(action)!);
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
	if (!competing) for (const action of actions) expect(handlers.get(action)).toBeNull();
	if (validReads && !competing) {
		if (action === 'seekto') expect(get(media.absoluteSeek).feedback).toBe('That position is not seekable yet.');
		else expect(get(media.seekFeedback).jumpStatus).toBe('Cannot jump: this track cannot seek yet.');
	}
	seekable.mockRestore();
	audio.setReady(); // No native event ever published the loss.
	for (const [i, action] of actions.entries()) {
		retired[i]!({ action, seekTime: 10, seekOffset: 1 });
		expect(audio.currentTime).toBe(position);
		expect(handlers.get(action)).not.toBe(retired[i]);
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
	const write = vi.fn((seconds: number) => {
		if (seconds === 40) {
			system();
			for (const ranges of [[], undefined]) audio.setReady(ranges);
		}
		audio.position = seconds;
	});
	audio.write = write;
	compact(40);
	expect(write.mock.calls).toEqual([[40]]); // Recovery did not revive the queued write.
	fire('seekto', { seekTime: 10 });
	expect(audio.currentTime).toBe(10);
});

test('late publication binds supported seek actions and preserves both Play controls', async () => {
	const { audio, media, select, handlers, fire } = setup({ unsupported: 'seekto', connected: false });
	expect(handlers.get('play')).toBeUndefined();
	audio.isConnected = true;
	select({ ...track });
	audio.setReady();
	expect(handlers.get('seekto')).toBeUndefined();
	fire('seekbackward');
	expect(audio.currentTime).toBe(10);
	await media.playCurrent();
	expect(audio.play).toHaveBeenCalledOnce();
	media.pauseCurrent();
	expect(audio.paused).toBe(true);
	handlers.get('play')!();
	expect(audio.play).toHaveBeenCalledTimes(2);
});

test.each(['decoder', 'source', 'auth', 'detach'])('fences all queued callbacks across %s changes', (boundary) => {
	const { audio, media, queue, select, detach, handlers, fire } = setup();
	const retired = actions.map((action) => handlers.get(action)!);
	if (boundary === 'auth') {
		queue.purgeAtAuthBoundary(() => media.purgeAtAuthBoundary(() => {}));
		select(track);
	} else if (boundary === 'source') {
		select(other); // Source changes before the old audio detaches.
		for (const action of actions) expect(handlers.get(action)).toBeNull();
		select(track); // Round-trip must not revive old callbacks.
	} else if (boundary === 'detach') detach();
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
});
