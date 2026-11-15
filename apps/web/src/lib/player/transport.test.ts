import { describe, expect, it, vi } from 'vitest';
import { createTransport, type TransportSnapshot } from './transport.js';
import type { MediaSessionLike, MediaSessionSeekDetails } from './mediaSession.js';
import type { Track } from './queue.js';
import { RESUME_STORAGE_KEY } from './resume.js';
import { SOUND_PREFERENCE_STORAGE_KEY, serializeSoundPreference } from './soundPreference.js';
import { PLAYBACK_SPEED_STORAGE_KEY } from './playbackSpeed.js';

const FIRST: Track = { id: 'first', title: 'First track', artist: 'Test artist', streamUrl: '/first' };
const SECOND: Track = { id: 'second', title: 'Second track', artist: 'Test artist', streamUrl: '/second' };

class AudioFake extends EventTarget {
	src = '/first';
	paused = true;
	ended = false;
	error: MediaError | null = null;
	currentTime = 0;
	duration = 100;
	volume = 1;
	muted = false;
	playbackRate = 1;
	readyState = 4;
	play = vi.fn(async () => {
		this.paused = false;
		this.emit('play');
	});
	pause = vi.fn(() => {
		this.paused = true;
		this.emit('pause');
	});
	load = vi.fn();
	getAttribute(name: string) { return name === 'src' ? this.src : null; }
	emit(name: string) { this.dispatchEvent(new Event(name)); }
	get element() { return this as unknown as HTMLAudioElement; }
}

function setup(initial: Record<string, string> = {}) {
	const values = new Map(Object.entries(initial));
	const storage = {
		getItem: vi.fn((key: string) => values.get(key) ?? null),
		setItem: vi.fn((key: string, value: string) => { values.set(key, value); }),
		removeItem: vi.fn((key: string) => { values.delete(key); })
	};
	const handlers = new Map<string, ((details?: MediaSessionSeekDetails) => void) | null>();
	const session: MediaSessionLike = {
		metadata: null,
		setActionHandler: (action, handler) => { handlers.set(action, handler); },
		setPositionState: vi.fn()
	};
	const onchange = vi.fn<(snapshot: TransportSnapshot) => void>();
	const onnext = vi.fn();
	const onprevious = vi.fn();
	const onended = vi.fn();
	let now = 0;
	const transport = createTransport({
		storage, onchange, onnext, onprevious, onended, now: () => now,
		mediaSession: {
			session, baseUrl: 'https://example.test',
			MetadataCtor: class { constructor(public init: unknown) {} }
		}
	});
	const audio = new AudioFake();
	transport.connect(audio.element, FIRST);
	return {
		transport, audio, storage, values, session, handlers, onchange, onnext, onprevious, onended,
		advance: (ms: number) => { now += ms; transport.tick(); }
	};
}

async function start(state: ReturnType<typeof setup>) {
	await state.transport.play();
	state.audio.emit('playing');
}

function end(audio: AudioFake) {
	audio.currentTime = audio.duration;
	audio.ended = true;
	audio.paused = true;
	audio.emit('pause');
	audio.emit('ended');
}

describe('native zero-assignment boundary', () => {
	for (const outcome of ['throw', 'sticky', 'nan', 'infinity', 'negative-infinity', 'success'] as const) {
		it(`delivers completion only after a normal ${outcome} setter return, before observations`, () => {
			const state = setup();
			let position = 37;
			let assigned = false;
			let reset = false;
			const writes: number[] = [];
			state.audio.duration = NaN;
			Object.defineProperty(state.audio, 'currentTime', {
				get: () => { if (assigned) expect(reset).toBe(true); return position; },
				set: (value: number) => {
					writes.push(value);
					expect(reset).toBe(false);
					if (outcome === 'throw') throw new Error('refused');
					if (outcome === 'success') position = value;
					if (outcome === 'nan') position = NaN;
					if (outcome === 'infinity') position = Infinity;
					if (outcome === 'negative-infinity') position = -Infinity;
					assigned = true;
				}
			});
			const onAssigned = vi.fn(() => { expect(assigned).toBe(true); reset = true; });
			expect(state.transport.seekToStart({ isCurrent: () => true, onAssigned })).toBe(outcome === 'success');
			expect(writes).toEqual([0]);
			expect(onAssigned).toHaveBeenCalledTimes(outcome === 'throw' ? 0 : 1);
			expect(reset).toBe(outcome !== 'throw');
			expect(state.audio.play).not.toHaveBeenCalled();
			expect(state.audio.load).not.toHaveBeenCalled();
		});
	}

	for (const point of ['before', 'setter', 'reset'] as const) {
		it(`stops stale zero-seek continuation at ${point}`, () => {
			const state = setup();
			let current = point !== 'before';
			let position = 37;
			const writes: number[] = [];
			Object.defineProperty(state.audio, 'currentTime', {
				get: () => position,
				set: (value: number) => { writes.push(value); position = value; if (point === 'setter') current = false; }
			});
			state.onchange.mockClear();
			const onAssigned = vi.fn(() => { if (point === 'reset') current = false; });
			expect(state.transport.seekToStart({ isCurrent: () => current, onAssigned })).toBe(false);
			expect(writes).toEqual(point === 'before' ? [] : [0]);
			expect(onAssigned).toHaveBeenCalledTimes(point === 'reset' ? 1 : 0);
			expect(state.onchange).not.toHaveBeenCalled();
			expect(state.audio.play).not.toHaveBeenCalled();
		});
	}

	it('does not retain the completion callback for ordinary seeks', () => {
		const state = setup();
		const onAssigned = vi.fn();
		expect(state.transport.seekToStart({ isCurrent: () => true, onAssigned })).toBe(true);
		expect(state.transport.seek(10)).toBe(true);
		expect(onAssigned).toHaveBeenCalledOnce();
	});

	it('cancels pending resume even when a zero setter throws', () => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"second":25}' });
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		next.duration = NaN;
		let position = 37;
		let refuse = true;
		Object.defineProperty(next, 'currentTime', {
			get: () => position,
			set: (value: number) => { if (refuse) throw new Error('refused'); position = value; }
		});
		state.transport.connect(next.element, SECOND);
		const onAssigned = vi.fn();
		expect(state.transport.seekToStart({ isCurrent: () => true, onAssigned })).toBe(false);
		refuse = false;
		next.duration = 100;
		next.emit('loadedmetadata');
		expect(position).toBe(37);
		expect(onAssigned).not.toHaveBeenCalled();
	});
});

describe('app player transport', () => {
	it('distinguishes pending play, audible playback, interruption and manual pause', async () => {
		const state = setup();
		expect(state.transport.snapshot().status).toBe('selected');
		await state.transport.play();
		expect(state.transport.snapshot().status).toBe('loading');
		state.audio.emit('waiting');
		expect(state.transport.snapshot().status).toBe('loading');
		state.audio.emit('playing');
		expect(state.transport.snapshot().status).toBe('playing');
		state.audio.emit('stalled');
		expect(state.transport.snapshot().status).toBe('playing');
		state.audio.emit('waiting');
		expect(state.transport.snapshot().status).toBe('buffering');
		state.audio.emit('playing');
		expect(state.transport.snapshot().status).toBe('playing');
		state.transport.pause();
		state.audio.emit('waiting');
		expect(state.transport.snapshot().status).toBe('paused');
	});

	it('reports refused playback, but a superseded failure cannot affect a replacement', async () => {
		const state = setup();
		state.audio.play.mockRejectedValueOnce(new Error('refused'));
		await state.transport.play();
		expect(state.transport.snapshot().status).toBe('error');
		let reject!: (reason: Error) => void;
		state.audio.play.mockImplementationOnce(() => new Promise<void>((_resolve, fail) => { reject = fail; }));
		const pending = state.transport.play();
		await Promise.resolve();
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		state.transport.connect(next.element, SECOND);
		reject(new Error('old source aborted'));
		await pending;
		expect(state.transport.snapshot().status).toBe('selected');
	});

	it('reloads a terminal media error on retry and restores its position and speed', async () => {
		const state = setup({ [PLAYBACK_SPEED_STORAGE_KEY]: '1.5' });
		state.audio.currentTime = 37;
		state.audio.error = { code: 4 } as MediaError;
		state.audio.emit('error');
		state.audio.load.mockImplementationOnce(() => {
			state.audio.error = null;
			state.audio.currentTime = 0;
			state.audio.playbackRate = 1;
			queueMicrotask(() => state.audio.emit('ratechange'));
		});
		await state.transport.retry();
		expect(state.audio.playbackRate).toBe(1.5);
		expect(state.values.get(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.5');
		expect(state.audio.load).toHaveBeenCalledOnce();
		expect(state.audio.play).toHaveBeenCalledOnce();
		state.audio.emit('loadedmetadata');
		expect(state.audio.currentTime).toBe(37);
	});

	it('does not reload a healthy resource when retrying a playback refusal', async () => {
		const state = setup();
		state.audio.currentTime = 23;
		await state.transport.retry();
		expect(state.audio.load).not.toHaveBeenCalled();
		expect(state.audio.currentTime).toBe(23);
	});

	it('pause and source replacement fence a retry waiting to play', async () => {
		const state = setup();
		state.audio.error = { code: 4 } as MediaError;
		const retry = state.transport.retry();
		state.transport.pause();
		await retry;
		expect(state.audio.play).not.toHaveBeenCalled();
		const secondRetry = state.transport.retry();
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		state.transport.connect(next.element, SECOND);
		await secondRetry;
		expect(state.audio.play).not.toHaveBeenCalled();
		expect(next.play).not.toHaveBeenCalled();
		expect(state.transport.snapshot().status).toBe('selected');
	});

	it('contains a refused reload without claiming retry success', async () => {
		const state = setup();
		state.audio.error = { code: 4 } as MediaError;
		state.audio.load.mockImplementationOnce(() => { throw new Error('refused reload'); });
		await state.transport.retry();
		expect(state.transport.snapshot().status).toBe('error');
		expect(state.audio.play).not.toHaveBeenCalled();
	});

	it('cancels a play waiting for flush when the user pauses', async () => {
		const state = setup();
		const pending = state.transport.play();
		state.transport.pause();
		await pending;
		expect(state.audio.play).not.toHaveBeenCalled();
		expect(state.transport.snapshot().status).toBe('paused');
	});

	it('rejects unknown durations and invalid seeks, clamps finite requests, and reflects readback', () => {
		const state = setup();
		state.audio.duration = NaN;
		expect(state.transport.seek(50)).toBe(false);
		expect(state.transport.snapshot().duration).toBeNull();
		state.audio.duration = 100;
		expect(state.transport.seek(NaN)).toBe(false);
		expect(state.transport.seek(Infinity)).toBe(false);
		expect(state.transport.seek(150)).toBe(true);
		expect(state.audio.currentTime).toBe(100);
		expect(state.transport.seek(-10)).toBe(true);
		expect(state.audio.currentTime).toBe(0);
		Object.defineProperty(state.audio, 'currentTime', { get: () => 12, set: () => {} });
		expect(state.transport.seek(60)).toBe(false);
		expect(state.transport.snapshot().position).toBe(12);
	});

	it('seeks to the start before duration is known and supersedes deferred resume', () => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"second":25}' });
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		next.duration = NaN;
		next.currentTime = 37;
		state.transport.connect(next.element, SECOND);
		expect(state.transport.seekToStart()).toBe(true);
		expect(next.currentTime).toBe(0);
		next.duration = 100;
		next.emit('loadedmetadata');
		expect(next.currentTime).toBe(0);
		expect(next.play).not.toHaveBeenCalled();
	});

	it('reports refused or ignored start seeks without claiming zero', () => {
		const state = setup();
		Object.defineProperty(state.audio, 'currentTime', {
			get: () => 37, set: () => { throw new Error('not seekable'); }, configurable: true
		});
		expect(state.transport.seekToStart()).toBe(false);
		Object.defineProperty(state.audio, 'currentTime', { get: () => 37, set: () => {} });
		expect(state.transport.seekToStart()).toBe(false);
		expect(state.transport.snapshot().position).toBe(37);
	});

	it('keeps seek and transport usable when storage and media setters throw', async () => {
		const state = setup();
		state.storage.setItem.mockImplementation(() => { throw new Error('storage refused'); });
		Object.defineProperty(state.audio, 'currentTime', { get: () => 7, set: () => { throw new Error('not ready'); } });
		expect(state.transport.seek(40)).toBe(false);
		state.transport.setVolume(0.4);
		expect(state.audio.volume).toBe(0.4);
		state.transport.setSpeed(1.5);
		expect(state.audio.playbackRate).toBe(1.5);
		await start(state);
		expect(state.transport.snapshot().status).toBe('playing');
	});

	it('restores sound and speed on replacement elements, adopting external changes', () => {
		const state = setup({
			[SOUND_PREFERENCE_STORAGE_KEY]: serializeSoundPreference({ volume: 0.25, muted: true }),
			[PLAYBACK_SPEED_STORAGE_KEY]: '1.5'
		});
		expect(state.audio.volume).toBe(0.25);
		expect(state.audio.muted).toBe(true);
		expect(state.audio.playbackRate).toBe(1.5);
		state.transport.setVolume(0.7);
		expect(state.audio.muted).toBe(false);
		state.transport.toggleMute();
		expect(state.audio.muted).toBe(true);
		state.audio.playbackRate = 1.25;
		state.audio.emit('ratechange');
		state.audio.volume = 0.35;
		state.audio.emit('volumechange');
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		state.transport.connect(next.element, SECOND);
		expect(next.volume).toBe(0.35);
		expect(next.muted).toBe(true);
		expect(next.playbackRate).toBe(1.25);
	});

	it('publishes actual system-controlled volume and speed, never the refused request', () => {
		const state = setup();
		Object.defineProperty(state.audio, 'volume', { get: () => 1, set: () => {} });
		Object.defineProperty(state.audio, 'playbackRate', { get: () => 1, set: () => {} });
		state.transport.setVolume(0.2);
		state.transport.setSpeed(2);
		expect(state.transport.snapshot()).toMatchObject({ sound: { volume: 1, restricted: true }, rate: 1 });
		expect(JSON.parse(state.values.get(SOUND_PREFERENCE_STORAGE_KEY)!)).toMatchObject({ volume: 1 });
		expect(state.values.get(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1');
	});

	it('restores a deferred resume on metadata but lets explicit seeks supersede it', () => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"first":32,"second":18}' });
		expect(state.audio.currentTime).toBe(32);
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		next.duration = NaN;
		state.transport.connect(next.element, SECOND);
		expect(next.currentTime).toBe(0);
		// An unload before metadata must not overwrite the deferred saved position with zero.
		state.transport.persistPosition();
		expect(JSON.parse(state.values.get(RESUME_STORAGE_KEY)!)).toMatchObject({ second: 18 });
		next.duration = 100;
		expect(state.transport.seek(44)).toBe(true);
		next.emit('loadedmetadata');
		expect(next.currentTime).toBe(44);
		state.transport.pause();
		expect(JSON.parse(state.values.get(RESUME_STORAGE_KEY)!)).toMatchObject({ second: 44 });
	});

	it('retries a refused resume on canplay without claiming a restored position', () => {
		const state = setup();
		state.values.set(RESUME_STORAGE_KEY, '{"second":25}');
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		let writable = false;
		let position = 0;
		Object.defineProperty(next, 'currentTime', {
			get: () => position, set: (value: number) => { if (writable) position = value; }
		});
		state.transport.connect(next.element, SECOND);
		expect(state.transport.snapshot().position).toBe(0);
		writable = true;
		next.emit('canplay');
		expect(state.transport.snapshot().position).toBe(25);
	});

	it.each([0, 60])('a native seek to %s supersedes a deferred resume', (position) => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"second":32}' });
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		next.duration = NaN;
		state.transport.connect(next.element, SECOND);
		next.currentTime = position;
		next.emit('seeking');
		next.emit('seeked');
		next.duration = 100;
		next.emit('canplay');
		expect(next.currentTime).toBe(position);
	});

	it('an explicit library seek retires a refused restore even without native seek events', () => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"second":32}' });
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		let writable = false;
		let position = 0;
		Object.defineProperty(next, 'currentTime', {
			get: () => position, set: (value: number) => { if (writable) position = value; }
		});
		state.transport.connect(next.element, SECOND);
		state.transport.cancelPendingResume();
		writable = true;
		next.currentTime = 0;
		next.emit('canplay');
		expect(next.currentTime).toBe(0);
	});

	it('restore-generated synchronous seek events do not retire a refused restore retry', () => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"second":32}' });
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		let writable = false;
		let position = 0;
		Object.defineProperty(next, 'currentTime', {
			get: () => position,
			set: (value: number) => {
				if (writable) position = value;
				next.emit('seeking');
				next.emit('seeked');
			}
		});
		state.transport.connect(next.element, SECOND);
		writable = true;
		next.emit('canplay');
		expect(next.currentTime).toBe(32);
	});

	it('only counts verified natural ends, once, and clears their resume entry', async () => {
		const state = setup();
		state.audio.emit('ended');
		expect(state.onended).not.toHaveBeenCalled();
		await start(state);
		state.transport.seek(80);
		state.audio.emit('seeked');
		expect(state.onended).not.toHaveBeenCalled();
		end(state.audio);
		state.audio.emit('ended');
		expect(state.onended).toHaveBeenCalledExactlyOnceWith('first');
		expect(JSON.parse(state.values.get(RESUME_STORAGE_KEY) ?? '{}')).not.toHaveProperty('first');
	});

	it.each(['transport', 'native'] as const)('does not count a terminal %s seek as natural completion', async (control) => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"first":32}' });
		await start(state);
		state.transport.armSleep({ kind: 'stop-after-tracks', count: 2 });
		if (control === 'transport') state.transport.seek(100);
		else state.audio.currentTime = 100;
		state.audio.emit('seeking');
		state.audio.emit('seeked');
		end(state.audio);
		state.audio.emit('ended');
		expect(state.onended).not.toHaveBeenCalled();
		expect(state.transport.snapshot()).toMatchObject({
			status: 'paused', sleep: { kind: 'stop-after-tracks', remainingTracks: 2 }
		});
		expect(JSON.parse(state.values.get(RESUME_STORAGE_KEY)!)).toHaveProperty('first', 32);

		// A nonterminal seek followed by actual playback can complete naturally again.
		state.audio.ended = false;
		state.transport.seek(80);
		state.audio.emit('seeking');
		state.audio.emit('seeked');
		await start(state);
		end(state.audio);
		expect(state.onended).toHaveBeenCalledExactlyOnceWith('first');
		expect(state.transport.snapshot().sleep).toEqual({ kind: 'stop-after-tracks', remainingTracks: 1 });
	});

	it.each([2, 4])('adopts an unpaused element at readyState %s without needing another play event', async (readyState) => {
		const state = setup();
		state.transport.armSleep({ kind: 'stop-after-tracks', count: 2 });
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		next.paused = false;
		next.readyState = readyState;
		next.currentTime = 24;
		state.transport.connect(next.element, SECOND);
		expect(state.transport.snapshot().status).toBe(readyState >= 3 ? 'playing' : 'loading');
		await state.transport.play();
		expect(next.play).not.toHaveBeenCalled();
		expect(next.pause).not.toHaveBeenCalled();
		if (readyState < 3) {
			next.emit('waiting');
			expect(state.transport.snapshot().status).toBe('loading');
			next.readyState = 4;
			next.emit('playing');
		}
		next.emit('waiting');
		expect(state.transport.snapshot().status).toBe('buffering');
		next.emit('playing');
		end(next);
		next.emit('ended');
		expect(state.onended).toHaveBeenCalledExactlyOnceWith('second');
		expect(state.transport.snapshot().sleep).toEqual({ kind: 'stop-after-tracks', remainingTracks: 1 });
	});

	it('counts the existing playback on an adopted source without rewinding to its saved resume', () => {
		const state = setup({ [RESUME_STORAGE_KEY]: '{"second":12}' });
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		next.paused = false;
		next.currentTime = 50;
		state.transport.connect(next.element, SECOND);
		expect(state.transport.snapshot()).toMatchObject({ status: 'playing', position: 50 });
		end(next);
		next.emit('ended');
		expect(state.onended).toHaveBeenCalledExactlyOnceWith('second');
		expect(JSON.parse(state.values.get(RESUME_STORAGE_KEY)!)).not.toHaveProperty('second');
	});

	it('leaves audible playback unchanged on repeated pointer and MediaSession Play', async () => {
		const state = setup();
		await start(state);
		await state.transport.play();
		state.handlers.get('play')?.();
		await Promise.resolve();
		expect(state.transport.snapshot().status).toBe('playing');
		expect(state.audio.play).toHaveBeenCalledOnce();
	});

	it('cancels sleep across an account boundary without pausing the next account', async () => {
		const state = setup();
		await start(state);
		state.transport.armSleep({ kind: 'duration', ms: 1000 });
		state.transport.disconnect(false);
		expect(state.transport.snapshot().sleep).toBeNull();
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		state.transport.connect(next.element, SECOND);
		await state.transport.play();
		next.emit('playing');
		state.advance(1000);
		expect(next.paused).toBe(false);
	});

	it('end-of-track sleep stops without advancing or changing the selected track', async () => {
		const state = setup();
		await start(state);
		state.transport.armSleep({ kind: 'end-of-track' });
		end(state.audio);
		expect(state.onended).not.toHaveBeenCalled();
		expect(state.audio.paused).toBe(true);
		expect(state.transport.snapshot()).toMatchObject({ status: 'paused', sleep: null });
	});

	it('duration sleep is clock-driven, and stop-after-N survives element replacements', async () => {
		const state = setup();
		await start(state);
		state.transport.armSleep({ kind: 'duration', ms: 60_000 });
		state.advance(59_000);
		expect(state.audio.paused).toBe(false);
		expect(state.transport.snapshot().sleep).toEqual({ kind: 'duration', remainingMs: 1000 });
		state.advance(1000);
		expect(state.audio.paused).toBe(true);
		state.audio.ended = false;
		await start(state);
		state.transport.armSleep({ kind: 'stop-after-tracks', count: 2 });
		end(state.audio);
		expect(state.onended).toHaveBeenCalledOnce();
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		state.transport.connect(next.element, SECOND);
		await state.transport.play();
		next.emit('playing');
		end(next);
		expect(state.onended).toHaveBeenCalledOnce();
		expect(state.transport.snapshot().sleep).toBeNull();
	});

	it('MediaSession and pointer controls share the same seek and transport paths', async () => {
		const state = setup();
		state.handlers.get('seekto')?.({ action: 'seekto', seekTime: 45 });
		expect(state.audio.currentTime).toBe(45);
		state.handlers.get('seekforward')?.({ action: 'seekforward', seekOffset: 20 });
		expect(state.audio.currentTime).toBe(65);
		state.handlers.get('seekbackward')?.({ action: 'seekbackward' });
		expect(state.audio.currentTime).toBe(55);
		state.handlers.get('seekforward')?.({ action: 'seekforward', seekOffset: -20 });
		expect(state.audio.currentTime).toBe(55);
		state.handlers.get('nexttrack')?.();
		state.handlers.get('previoustrack')?.();
		expect(state.onnext).toHaveBeenCalledOnce();
		expect(state.onprevious).toHaveBeenCalledOnce();
		state.handlers.get('play')?.();
		await Promise.resolve();
		expect(state.audio.play).toHaveBeenCalledOnce();
		state.handlers.get('pause')?.();
		expect(state.audio.paused).toBe(true);
		expect(state.session.setPositionState).toHaveBeenCalledWith({ duration: 100, position: 55, playbackRate: 1 });
	});

	it('retires old event listeners, callbacks and teardowns without disturbing the current source', () => {
		const state = setup();
		const oldNext = state.handlers.get('nexttrack');
		const oldSeek = state.handlers.get('seekto');
		const oldCleanup = state.transport.connect(state.audio.element, FIRST);
		const next = new AudioFake();
		next.src = SECOND.streamUrl;
		state.transport.connect(next.element, SECOND);
		oldCleanup();
		state.onchange.mockClear();
		state.audio.currentTime = 70;
		state.audio.emit('timeupdate');
		state.audio.emit('error');
		oldNext?.();
		oldSeek?.({ action: 'seekto', seekTime: 80 });
		expect(state.onchange).not.toHaveBeenCalled();
		expect(state.onnext).not.toHaveBeenCalled();
		expect(next.currentTime).toBe(0);
		expect(state.transport.snapshot().status).toBe('selected');
		state.transport.seek(20);
		expect(next.currentTime).toBe(20);
	});

	it('does not act on a mismatched src, and account-boundary disconnect never writes a position', () => {
		const state = setup();
		state.audio.src = '/replaced';
		state.transport.seek(60);
		state.audio.emit('playing');
		state.audio.emit('error');
		expect(state.audio.currentTime).toBe(0);
		expect(state.transport.snapshot().status).toBe('selected');
		state.audio.src = FIRST.streamUrl;
		state.audio.currentTime = 50;
		state.storage.setItem.mockClear();
		state.transport.disconnect(false);
		expect(state.storage.setItem).not.toHaveBeenCalled();
		expect(state.session.metadata).toBeNull();
		for (const handler of state.handlers.values()) expect(handler).toBeNull();
	});
});
