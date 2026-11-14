import { afterEach, describe, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, currentTrack, type Track } from './queue.js';
import { SOUND_PREFERENCE_STORAGE_KEY } from './soundPreference.js';
import { PLAYBACK_SPEED_STORAGE_KEY } from './playbackSpeed.js';
import { RESUME_STORAGE_KEY } from './resume.js';
import type { SnapshotStorage } from '../library/cache.js';

const A: Track = { id: 'a', title: 'Track A', artist: 'Artist', streamUrl: '/a' };
const B: Track = { id: 'b', title: 'Track B', artist: 'Artist', streamUrl: '/b' };
const C: Track = { id: 'c', title: 'Track C', artist: 'Artist', streamUrl: '/c' };
const D: Track = { id: 'd', title: 'Track D', artist: 'Artist', streamUrl: '/d' };
const REPEAT_KEY = 'nntune:library-repeat:v1';

class AudioFake extends EventTarget {
	paused = true;
	ended = false;
	error: MediaError | null = null;
	currentTime = 0;
	duration = 100;
	volume = 1;
	muted = false;
	playbackRate = 1;
	readyState = 4;
	get seekable() { return { length: 1, start: () => 0, end: () => this.duration }; }
	constructor(public src: string) { super(); }
	play = vi.fn(async () => {
		this.paused = false;
		this.ended = false;
		this.emit('play');
		this.emit('playing');
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

function makeStorage(initial: Record<string, string> = {}) {
	const values = new Map(Object.entries(initial));
	const storage = {
		getItem: vi.fn((key: string) => values.get(key) ?? null),
		setItem: vi.fn((key: string, value: string) => { values.set(key, value); }),
		removeItem: vi.fn((key: string) => { values.delete(key); })
	} satisfies SnapshotStorage;
	return { storage, values };
}

function controlledFlush() {
	const pending: (() => void)[] = [];
	return {
		flush: vi.fn(() => new Promise<void>((resolve) => { pending.push(resolve); })),
		release: async () => {
			pending.splice(0).forEach((resolve) => resolve());
			await new Promise((resolve) => setTimeout(resolve, 0));
		}
	};
}

function setup(tracks: readonly Track[] = [A, B], initial: Record<string, string> = {}) {
	const flushControl = controlledFlush();
	const session = createPlayerSession({ flush: flushControl.flush });
	const { storage, values } = makeStorage(initial);
	session.setQueue(createQueue(tracks));
	session.start(storage);
	return { session, storage, values, ...flushControl };
}

type Session = ReturnType<typeof setup>['session'];

function mount(session: Session, track: Track) {
	const audio = new AudioFake(track.streamUrl);
	const detach = session.attach(audio.element);
	return { audio, detach };
}

async function startPlaying(session: Session, audio: AudioFake) {
	await session.play();
	audio.emit('playing');
}

function select(session: Session, index: number) {
	session.setQueue({ ...session.snapshot().queue, currentIndex: index });
}

const currentId = (session: Session) => currentTrack(session.snapshot().queue)?.id;

afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });

describe('seek, seekBy and restart', () => {
	it('seeks and clamps within a known duration without starting playback', () => {
		const { session } = setup();
		const { audio } = mount(session, A);
		expect(session.seek(30)).toBe(true);
		expect(audio.currentTime).toBe(30);
		expect(session.seekBy(-100)).toBe(true);
		expect(audio.currentTime).toBe(0);
		expect(session.seekBy(500)).toBe(true);
		expect(audio.currentTime).toBe(100);
		expect(audio.play).not.toHaveBeenCalled();
		expect(audio.paused).toBe(true);
	});

	it('refuses seek and restart for unknown duration, then recovers with usable native media', () => {
		const { session } = setup();
		const { audio } = mount(session, A);
		audio.currentTime = 12;
		audio.duration = NaN;
		expect(session.seek(5)).toBe(false);
		expect(session.seekBy(5)).toBe(false);
		expect(audio.currentTime).toBe(12);
		audio.duration = 100;
		expect(session.seek(NaN)).toBe(false);
		expect(session.seekBy(Infinity)).toBe(false);
		expect(audio.currentTime).toBe(12);
		audio.duration = NaN;
		expect(session.restart()).toBe(false);
		expect(audio.currentTime).toBe(12);
		audio.duration = 100;
		expect(session.restart()).toBe(true);
		expect(audio.currentTime).toBe(0);
		expect(audio.play).not.toHaveBeenCalled();
	});

	it('reports false when the element refuses the setter, and never plays', () => {
		const { session } = setup();
		const { audio } = mount(session, A);
		Object.defineProperty(audio, 'currentTime', {
			get: () => 20, set: () => { throw new Error('not seekable'); }
		});
		expect(session.seek(50)).toBe(false);
		expect(session.seekBy(5)).toBe(false);
		expect(session.restart()).toBe(false);
		expect(audio.play).not.toHaveBeenCalled();
	});

	it('is a harmless false before start and with no live element', () => {
		const bare = createPlayerSession({ flush: async () => {} });
		expect(bare.seek(1)).toBe(false);
		expect(bare.seekBy(1)).toBe(false);
		expect(bare.restart()).toBe(false);
		const { session } = setup();
		expect(session.seek(1)).toBe(false);
		expect(session.restart()).toBe(false);
	});

	it('does not move a playing track out of playing when seeking', async () => {
		const { session } = setup();
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		audio.play.mockClear();
		session.seek(40);
		session.restart();
		expect(audio.play).not.toHaveBeenCalled();
		expect(session.snapshot().transport?.status).toBe('playing');
	});
});

describe('volume, mute and rate', () => {
	it('applies, clamps and persists volume and mute', () => {
		const { session, values } = setup();
		const { audio } = mount(session, A);
		session.setVolume(0.4);
		expect(audio.volume).toBe(0.4);
		session.setVolume(7);
		expect(audio.volume).toBe(1);
		session.setVolume(NaN);
		expect(audio.volume).toBe(1);
		session.toggleMute();
		expect(audio.muted).toBe(true);
		expect(JSON.parse(values.get(SOUND_PREFERENCE_STORAGE_KEY)!)).toMatchObject({ volume: 1, muted: true });
		session.setVolume(0.2);
		expect(audio.muted).toBe(false);
		expect(session.snapshot().transport?.sound).toMatchObject({ volume: 0.2, muted: false });
	});

	it('carries volume, mute and rate to the replacement element and storage', () => {
		const { session, values } = setup();
		const first = mount(session, A);
		session.setVolume(0.3);
		session.toggleMute();
		session.setSpeed(1.5);
		expect(first.audio.playbackRate).toBe(1.5);
		expect(values.get(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.5');
		select(session, 1);
		const second = mount(session, B);
		expect(second.audio.volume).toBe(0.3);
		expect(second.audio.muted).toBe(true);
		expect(second.audio.playbackRate).toBe(1.5);
		expect(JSON.parse(values.get(SOUND_PREFERENCE_STORAGE_KEY)!)).toMatchObject({ volume: 0.3, muted: true });
	});

	it('clamps playback rate and ignores volume with no live element', () => {
		const { session, values } = setup();
		session.setVolume(0.5);
		session.toggleMute();
		expect(values.has(SOUND_PREFERENCE_STORAGE_KEY)).toBe(false);
		const { audio } = mount(session, A);
		session.setSpeed(9);
		expect(audio.playbackRate).toBe(2);
		session.setSpeed(0.1);
		expect(audio.playbackRate).toBe(0.5);
	});
});

describe('sleep fencing', () => {
	it('does not arm without a live source, arms with one, and cancels', () => {
		const { session } = setup();
		session.armSleep({ kind: 'end-of-track' });
		expect(session.snapshot().transport?.sleep ?? null).toBeNull();
		mount(session, A);
		session.armSleep({ kind: 'end-of-track' });
		expect(session.snapshot().transport?.sleep).toEqual({ kind: 'end-of-track' });
		session.cancelSleep();
		expect(session.snapshot().transport?.sleep).toBeNull();
	});

	it('discards an armed timer at the auth boundary so it cannot pause the next account', async () => {
		vi.useFakeTimers({ toFake: ['Date'] });
		const { session } = setup();
		const first = mount(session, A);
		await startPlaying(session, first.audio);
		session.armSleep({ kind: 'duration', ms: 1000 });
		session.resetAtAuthBoundary();
		expect(session.snapshot().transport?.sleep).toBeNull();
		session.setQueue(createQueue([C]));
		const next = mount(session, C);
		await startPlaying(session, next.audio);
		next.audio.pause.mockClear();
		vi.setSystemTime(Date.now() + 5000);
		session.tick();
		expect(next.audio.pause).not.toHaveBeenCalled();
		expect(session.snapshot().transport?.status).toBe('playing');
	});

	it('fires a duration timer by pausing the live source', async () => {
		vi.useFakeTimers({ toFake: ['Date'] });
		const { session } = setup();
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		session.armSleep({ kind: 'duration', ms: 1000 });
		vi.setSystemTime(Date.now() + 1500);
		session.tick();
		expect(audio.pause).toHaveBeenCalled();
		expect(session.snapshot().transport?.status).toBe('paused');
		expect(session.snapshot().transport?.sleep).toBeNull();
	});
});

describe('shuffle and repeat', () => {
	it('shuffles only upcoming tracks and keeps the current track attached and playing', async () => {
		const { session } = setup([A, B, C, D]);
		select(session, 1);
		const { audio } = mount(session, B);
		await startPlaying(session, audio);
		audio.pause.mockClear();
		session.shuffle(() => 0);
		expect(session.snapshot().queue.tracks.map((t) => t.id)).toEqual(['a', 'b', 'd', 'c']);
		expect(currentId(session)).toBe('b');
		expect(session.snapshot().audio).toBe(audio.element);
		expect(audio.pause).not.toHaveBeenCalled();
		expect(session.snapshot().transport?.status).toBe('playing');
	});

	it('cycles repeat off -> queue -> one -> off and persists each step', () => {
		const { session, values } = setup();
		const seen: string[] = [];
		for (let i = 0; i < 4; i += 1) {
			session.cycleRepeat();
			seen.push(session.snapshot().repeat);
			if (i === 0) expect(values.get(REPEAT_KEY)).toBe('queue');
		}
		expect(seen).toEqual(['queue', 'one', 'off', 'queue']);
		session.cycleRepeat();
		session.cycleRepeat();
		expect(values.get(REPEAT_KEY)).toBe('off');
	});
});

describe('next and previous', () => {
	it('next while playing moves on, starts from zero and keeps playing', async () => {
		const { session, release } = setup([A, B]);
		const first = mount(session, A);
		await startPlaying(session, first.audio);
		const moving = session.next();
		expect(currentId(session)).toBe('b');
		expect(first.audio.pause).toHaveBeenCalled();
		const second = mount(session, B);
		second.audio.currentTime = 17;
		await release();
		await moving;
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).toHaveBeenCalledOnce();
	});

	it('next while paused moves on and starts from zero without playing', async () => {
		const { session, release } = setup([A, B]);
		const first = mount(session, A);
		await startPlaying(session, first.audio);
		session.pause();
		const moving = session.next();
		const second = mount(session, B);
		second.audio.currentTime = 17;
		await release();
		await moving;
		expect(currentId(session)).toBe('b');
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).not.toHaveBeenCalled();
	});

	it('previous after more than 3 seconds restarts the same track without navigating', async () => {
		const { session } = setup([A, B]);
		select(session, 1);
		const { audio } = mount(session, B);
		await startPlaying(session, audio);
		audio.currentTime = 3.5;
		const queue = session.snapshot().queue;
		await session.previous();
		expect(session.snapshot().queue).toBe(queue);
		expect(audio.currentTime).toBe(0);
		expect(session.snapshot().audio).toBe(audio.element);
	});

	it('previous at 3 seconds or less moves back, resuming play when playing and not when paused', async () => {
		const { session, release } = setup([A, B, C]);
		select(session, 2);
		const third = mount(session, C);
		await startPlaying(session, third.audio);
		third.audio.currentTime = 3;
		const back = session.previous();
		expect(currentId(session)).toBe('b');
		const second = mount(session, B);
		await release();
		await back;
		expect(second.audio.play).toHaveBeenCalledOnce();

		session.pause();
		second.audio.currentTime = 1;
		const again = session.previous();
		const first = mount(session, A);
		first.audio.currentTime = 9;
		await release();
		await again;
		expect(currentId(session)).toBe('a');
		expect(first.audio.currentTime).toBe(0);
		expect(first.audio.play).not.toHaveBeenCalled();
	});

	it.each(['off', 'queue', 'one'] as const)('clamps at both ends with repeat %s', async (mode) => {
		const { session } = setup([A, B], { [REPEAT_KEY]: mode });
		const first = mount(session, A);
		const start = session.snapshot().queue;
		await session.previous();
		expect(session.snapshot().queue).toBe(start);
		expect(first.audio.pause).not.toHaveBeenCalled();
		select(session, 1);
		const last = mount(session, B);
		const end = session.snapshot().queue;
		await session.next();
		expect(session.snapshot().queue).toBe(end);
		expect(session.snapshot().audio).toBe(last.audio.element);
		expect(last.audio.pause).not.toHaveBeenCalled();
	});
});

describe('navigation intent observers', () => {
	it('notifies before replacing a source and supports route cleanup', async () => {
		const { session, release } = setup([A, B, C]);
		mount(session, A);
		const observer = vi.fn((next) => {
			expect(currentId(session)).toBe('a');
			expect(currentTrack(next)?.id).toBe('b');
		});
		const off = session.onBeforeNavigate(observer);
		const moving = session.next();
		expect(observer).toHaveBeenCalledOnce();
		mount(session, B);
		await release();
		await moving;
		off();
		const again = session.next();
		mount(session, C);
		await release();
		await again;
		expect(observer).toHaveBeenCalledOnce();
		expect(currentId(session)).toBe('c');
	});

	it('notifies on a clamped Next without touching media or pending playing intent', async () => {
		const { session } = setup([A]);
		const { audio } = mount(session, A);
		const observer = vi.fn();
		session.onBeforeNavigate(observer);
		const playing = session.play();
		await session.next();
		await playing;
		expect(observer).toHaveBeenCalledExactlyOnceWith(session.snapshot().queue);
		expect(audio.paused).toBe(false);
		expect(audio.play).toHaveBeenCalledOnce();
		expect(audio.pause).not.toHaveBeenCalled();
		expect(audio.load).not.toHaveBeenCalled();
	});

	it.each(['auth', 'source', 'destroy'] as const)('fences a %s boundary crossed by an observer', async (boundary) => {
		const { session, flush } = setup([A, B]);
		const { audio } = mount(session, A);
		session.onBeforeNavigate(() => {
			if (boundary === 'auth') session.resetAtAuthBoundary();
			else if (boundary === 'destroy') session.destroy();
			else session.setQueue(createQueue([C]));
		});
		await session.next();
		expect(currentId(session)).toBe(boundary === 'source' ? 'c' : undefined);
		expect(flush).not.toHaveBeenCalled();
		expect(audio.play).not.toHaveBeenCalled();
	});

	it('notifies on Previous restart without changing its paused playback intent', async () => {
		const { session } = setup([A, B]);
		select(session, 1);
		const { audio } = mount(session, B);
		audio.currentTime = 15;
		const observer = vi.fn();
		session.onBeforeNavigate(observer);
		await session.previous();
		expect(observer).toHaveBeenCalledExactlyOnceWith(session.snapshot().queue);
		expect(audio.currentTime).toBe(0);
		expect(audio.play).not.toHaveBeenCalled();
		expect(audio.pause).not.toHaveBeenCalled();
		expect(audio.load).not.toHaveBeenCalled();
	});
});

describe('deferred navigation flush races', () => {
	async function pendingNext() {
		const ctx = setup([A, B, C]);
		const first = mount(ctx.session, A);
		await startPlaying(ctx.session, first.audio);
		const moving = ctx.session.next();
		const second = mount(ctx.session, B);
		return { ...ctx, moving, second };
	}

	it('is overtaken by an explicit pause', async () => {
		const { session, release, moving, second } = await pendingNext();
		session.pause();
		await release();
		await moving;
		expect(second.audio.play).not.toHaveBeenCalled();
		expect(second.audio.currentTime).toBe(0);
	});

	it.each(['next', 'previous'] as const)('%s still resets saved resume after a pending pause', async (action) => {
		const { session, release } = setup([A, B, C], { [RESUME_STORAGE_KEY]: '{"b":25}' });
		if (action === 'previous') select(session, 2);
		const first = mount(session, action === 'next' ? A : C);
		await startPlaying(session, first.audio);
		const moving = session[action]();
		const second = mount(session, B);
		expect(second.audio.currentTime).toBe(25);
		session.pause();
		await release();
		await moving;
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).not.toHaveBeenCalled();
		await session.play();
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).toHaveBeenCalledOnce();
	});

	it.each(['next', 'previous'] as const)('%s retains an explicit Play before replacement attachment', async (action) => {
		const { session, release } = setup([A, B, C], { [RESUME_STORAGE_KEY]: '{"b":25}' });
		if (action === 'previous') select(session, 2);
		mount(session, action === 'next' ? A : C);
		const moving = session[action]();
		await session.play();
		const second = mount(session, B);
		await release();
		await moving;
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).toHaveBeenCalledOnce();
		expect(session.snapshot().transport?.status).toBe('playing');
	});

	it.each([false, true])('system Play during attachment waits for the zero reset; later pause = %s', async (pause) => {
		const handlers = new Map<string, (() => void) | null>();
		vi.stubGlobal('navigator', {
			mediaSession: {
				metadata: null,
				setActionHandler: (action: string, handler: (() => void) | null) => { handlers.set(action, handler); },
				setPositionState: () => {}
			}
		});
		const { session, release } = setup([A, B], { [RESUME_STORAGE_KEY]: '{"b":25}' });
		mount(session, A);
		const moving = session.next();
		const second = mount(session, B);
		expect(second.audio.currentTime).toBe(25);
		handlers.get('play')!();
		await Promise.resolve();
		expect(second.audio.play).not.toHaveBeenCalled();
		if (pause) handlers.get('pause')!();
		await release();
		await moving;
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).toHaveBeenCalledTimes(pause ? 0 : 1);
	});

	it('Play then Pause before attachment still finishes the zero reset without playback', async () => {
		const { session, release } = setup([A, B], { [RESUME_STORAGE_KEY]: '{"b":25}' });
		mount(session, A);
		const moving = session.next();
		await session.play();
		session.pause();
		const second = mount(session, B);
		await release();
		await moving;
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).not.toHaveBeenCalled();
	});

	it('is overtaken by an auth reset', async () => {
		const { session, release, moving, second } = await pendingNext();
		session.resetAtAuthBoundary();
		session.setQueue(createQueue([B]));
		const third = mount(session, B);
		await release();
		await moving;
		expect(second.audio.play).not.toHaveBeenCalled();
		expect(third.audio.play).not.toHaveBeenCalled();
	});

	it('is overtaken by a different source selection', async () => {
		const { session, release, moving, second } = await pendingNext();
		select(session, 2);
		const third = mount(session, C);
		await release();
		await moving;
		expect(second.audio.play).not.toHaveBeenCalled();
		expect(third.audio.play).not.toHaveBeenCalled();
	});

	it('rapid Next twice: the newest selection inherits playing intent and only it plays', async () => {
		const { session, release } = setup([A, B, C, D]);
		const first = mount(session, A);
		await startPlaying(session, first.audio);
		const one = session.next();
		const mid = mount(session, B);
		const two = session.next();
		expect(currentId(session)).toBe('c');
		const newest = mount(session, C);
		newest.audio.currentTime = 17;
		await release();
		await Promise.all([one, two]);
		expect(mid.audio.play).not.toHaveBeenCalled();
		expect(newest.audio.currentTime).toBe(0);
		expect(newest.audio.play).toHaveBeenCalledOnce();
	});

	it('rapid Next twice before any attachment still plays only the newest source', async () => {
		const { session, release } = setup([A, B, C, D]);
		const first = mount(session, A);
		await startPlaying(session, first.audio);
		const one = session.next();
		const two = session.next();
		const newest = mount(session, C);
		await release();
		await Promise.all([one, two]);
		expect(newest.audio.play).toHaveBeenCalledOnce();
	});

	it('togglePlay during a pending navigation, before attachment, cancels the continuation', async () => {
		const { session, release } = setup([A, B]);
		const first = mount(session, A);
		await startPlaying(session, first.audio);
		const moving = session.next();
		await session.togglePlay();
		const second = mount(session, B);
		await release();
		await moving;
		expect(second.audio.play).not.toHaveBeenCalled();
		expect(second.audio.paused).toBe(true);
	});

	it('does not play if the session is destroyed while waiting', async () => {
		const { session, release, moving, second } = await pendingNext();
		session.destroy();
		await release();
		await moving;
		expect(second.audio.play).not.toHaveBeenCalled();
	});
});

describe('pending toggle', () => {
	it('toggle starts play; a second toggle before the playback flush cancels it', async () => {
		const { session } = setup();
		const { audio } = mount(session, A);
		const first = session.togglePlay();
		expect(session.snapshot().transport?.status).toBe('loading');
		const second = session.togglePlay();
		await Promise.all([first, second]);
		expect(audio.play).not.toHaveBeenCalled();
		expect(audio.paused).toBe(true);
		expect(session.snapshot().transport?.status).toBe('paused');
	});

	it('toggles play then pause across settled states', async () => {
		const { session } = setup();
		const { audio } = mount(session, A);
		await session.togglePlay();
		expect(audio.play).toHaveBeenCalledOnce();
		expect(session.snapshot().transport?.status).toBe('playing');
		await session.togglePlay();
		expect(audio.paused).toBe(true);
		expect(session.snapshot().transport?.status).toBe('paused');
	});
});
