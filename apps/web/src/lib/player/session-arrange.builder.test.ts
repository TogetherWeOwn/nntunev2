/**
 * Builder-owned playNext/shuffle tests. AudioFake is a unit double for the
 * media element's observable surface; it is NOT decoder or browser evidence.
 */
import { describe, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, currentTrack, type Track } from './queue.js';
import type { SnapshotStorage } from '../library/cache.js';

const mk = (id: string): Track => ({ id, title: `Track ${id.toUpperCase()}`, artist: 'Artist', streamUrl: `/${id}` });
const A = mk('a'), B = mk('b'), C = mk('c'), D = mk('d');

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
	play = vi.fn(async () => { this.paused = false; this.ended = false; this.emit('play'); this.emit('playing'); });
	pause = vi.fn(() => { this.paused = true; this.emit('pause'); });
	load = vi.fn();
	getAttribute(name: string) { return name === 'src' ? this.src : null; }
	emit(name: string) { this.dispatchEvent(new Event(name)); }
	get element() { return this as unknown as HTMLAudioElement; }
}

function makeStorage(): SnapshotStorage {
	const values = new Map<string, string>();
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => { values.set(key, value); },
		removeItem: (key) => { values.delete(key); }
	};
}

function setup(tracks: readonly Track[], index = 0) {
	const pending: (() => void)[] = [];
	const flush = vi.fn(() => new Promise<void>((resolve) => { pending.push(resolve); }));
	const release = async () => {
		pending.splice(0).forEach((resolve) => resolve());
		await new Promise((resolve) => setTimeout(resolve, 0));
	};
	const session = createPlayerSession({ flush });
	session.setQueue({ ...createQueue(tracks), currentIndex: index });
	session.start(makeStorage());
	return { session, flush, release };
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
const ids = (session: Session) => session.snapshot().queue.tracks.map((x) => x.id);
const currentId = (session: Session) => currentTrack(session.snapshot().queue)?.id;
function expectUntouched(audio: AudioFake) {
	expect(audio.play).not.toHaveBeenCalled();
	expect(audio.pause).not.toHaveBeenCalled();
	expect(audio.load).not.toHaveBeenCalled();
}

type Arrange = [string, (s: Session, random?: () => number) => boolean];
const playNextTo3: Arrange = ['playNext', (s) => s.playNext(3)];
const shuffleSuffix: Arrange = ['shuffle', (s, random) => s.shuffle(random ?? (() => 0))];

describe('real arrangements keep the live media element', () => {
	it.each(['playing', 'paused'] as const)('%s element keeps identity, position, prefix and calls', async (mode) => {
		for (const [name, run] of [playNextTo3, shuffleSuffix]) {
			const { session, flush } = setup([A, B, C, D], 1);
			const { audio } = mount(session, B);
			if (mode === 'playing') await startPlaying(session, audio);
			audio.currentTime = 42;
			audio.play.mockClear();
			const before = session.snapshot();
			expect(run(session), name).toBe(true);
			const after = session.snapshot();
			expect(after.queue).not.toBe(before.queue);
			expect(ids(session).slice(0, 2)).toEqual(['a', 'b']);
			expect(ids(session).slice().sort()).toEqual(['a', 'b', 'c', 'd']);
			expect(currentId(session)).toBe('b');
			expect(after.audio).toBe(audio.element);
			expect(audio.currentTime).toBe(42);
			expect(audio.paused).toBe(mode === 'paused');
			expectUntouched(audio);
			expect(flush).not.toHaveBeenCalled();
			if (mode === 'playing') expect(after.transport?.status).toBe('playing');
		}
	});

	it('playNext moves the chosen track directly after current, from either side', () => {
		const { session } = setup([A, B, C, D], 1);
		expect(session.playNext(3)).toBe(true);
		expect(ids(session)).toEqual(['a', 'b', 'd', 'c']);
		expect(session.playNext(0)).toBe(true);
		expect(ids(session)).toEqual(['b', 'a', 'd', 'c']);
		expect(currentId(session)).toBe('b');
	});

	it('a shuffle that yields the same order is still a real replacement', () => {
		const { session } = setup([A, B, C], 0);
		const before = session.snapshot().queue;
		// Draws of 0.99 pick the last slot each time, so the order is unchanged.
		expect(session.shuffle(() => 0.99)).toBe(true);
		expect(ids(session)).toEqual(['a', 'b', 'c']);
		expect(session.snapshot().queue).not.toBe(before);
	});

	it('a real edit clears stale feedback without bumping its version', () => {
		const { session } = setup([A, B, C, D], 0);
		session.setQueueFeedback('old', true);
		const version = session.snapshot().queueFeedbackVersion;
		expect(session.playNext(2)).toBe(true);
		expect(session.snapshot().queueFeedback).toBe('');
		expect(session.snapshot().queueFeedbackVersion).toBe(version);
	});
});

describe('no-op arrangements', () => {
	const cases: [string, (s: Session) => boolean, number][] = [
		['playNext current', (s) => s.playNext(1), 1],
		['playNext already next', (s) => s.playNext(2), 1],
		['playNext NaN', (s) => s.playNext(NaN), 1],
		['playNext fractional', (s) => s.playNext(1.5), 1],
		['playNext negative', (s) => s.playNext(-1), 1],
		['playNext out of range', (s) => s.playNext(9), 1],
		['shuffle last current', (s) => s.shuffle(), 3],
		['shuffle single upcoming', (s) => s.shuffle(), 2]
	];
	it.each(cases)('%s keeps identity and audio, clears stale feedback, still calls observers', (_n, run, index) => {
		const { session, flush } = setup([A, B, C, D], index);
		const { audio } = mount(session, [A, B, C, D][index]!);
		session.setQueueFeedback('stale', true);
		const before = session.snapshot();
		const proposals: unknown[] = [];
		const observer = vi.fn((next: unknown) => { proposals.push(next); });
		session.onBeforeNavigate(observer);
		expect(run(session)).toBe(false);
		const after = session.snapshot();
		expect(after.queue).toBe(before.queue);
		expect(after.audio).toBe(audio.element);
		expect(after.queueFeedback).toBe('');
		expect(after.queueFeedbackVersion).toBe(before.queueFeedbackVersion);
		expect(observer).toHaveBeenCalledOnce();
		expect(proposals[0]).toBe(before.queue);
		expect(flush).not.toHaveBeenCalled();
		expectUntouched(audio);
	});

	it('shuffle with no current selection does not draw', () => {
		const { session } = setup([A, B, C], -1);
		const random = vi.fn(() => 0);
		expect(session.shuffle(random)).toBe(false);
		expect(random).not.toHaveBeenCalled();
	});

	it('a no-op preserves a pending Next intent', async () => {
		const { session, flush, release } = setup([A, B, C], 0);
		const first = mount(session, A);
		await startPlaying(session, first.audio);
		const moving = session.next();
		expect(flush).toHaveBeenCalledOnce();
		const queue = session.snapshot().queue;
		session.setQueueFeedback('stale');
		expect(session.playNext(1)).toBe(false);
		expect(session.shuffle()).toBe(false);
		expect(session.snapshot().queue).toBe(queue);
		expect(session.snapshot().queueFeedback).toBe('');
		const second = mount(session, B);
		second.audio.currentTime = 9;
		await release();
		await moving;
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).toHaveBeenCalledOnce();
	});
});

describe('lifecycle', () => {
	it('before start nothing mutates, notifies or draws', () => {
		const session = createPlayerSession({ flush: async () => {} });
		session.setQueue({ ...createQueue([A, B, C, D]), currentIndex: 0 });
		const observer = vi.fn();
		session.onBeforeNavigate(observer);
		session.setQueueFeedback('keep', true);
		const before = session.snapshot();
		const random = vi.fn(() => 0);
		expect(session.playNext(2)).toBe(false);
		expect(session.shuffle(random)).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(observer).not.toHaveBeenCalled();
		expect(random).not.toHaveBeenCalled();
	});

	it('after destroy nothing mutates, notifies or draws', () => {
		const { session } = setup([A, B, C, D], 0);
		const observer = vi.fn();
		session.onBeforeNavigate(observer);
		session.destroy();
		const before = session.snapshot();
		const random = vi.fn(() => 0);
		expect(session.playNext(2)).toBe(false);
		expect(session.shuffle(random)).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(observer).not.toHaveBeenCalled();
		expect(random).not.toHaveBeenCalled();
	});
});

describe('boundary fencing', () => {
	const boundaries = ['auth', 'source', 'destroy'] as const;
	const cross = (session: Session, boundary: (typeof boundaries)[number]) => {
		if (boundary === 'auth') session.resetAtAuthBoundary();
		else if (boundary === 'destroy') session.destroy();
		else session.setQueue(createQueue([D]));
	};
	const expectedIds = (boundary: (typeof boundaries)[number]) => (boundary === 'source' ? ['d'] : []);

	it.each(boundaries)('a beforeNavigate observer crossing %s aborts playNext and shuffle', (boundary) => {
		for (const run of [(s: Session) => s.playNext(3), (s: Session) => s.shuffle(() => 0)]) {
			const { session, flush } = setup([A, B, C, D], 0);
			const { audio } = mount(session, A);
			session.onBeforeNavigate(() => cross(session, boundary));
			expect(run(session)).toBe(false);
			expect(ids(session)).toEqual(expectedIds(boundary));
			expect(session.snapshot().queueFeedbackVersion).toBe(0);
			expect(flush).not.toHaveBeenCalled();
			expect(audio.play).not.toHaveBeenCalled();
		}
	});

	it.each(boundaries)('an injected RNG crossing %s aborts before observers or publish', (boundary) => {
		const { session, flush } = setup([A, B, C, D], 0);
		const { audio } = mount(session, A);
		const observer = vi.fn();
		session.onBeforeNavigate(observer);
		expect(session.shuffle(() => { cross(session, boundary); return 0; })).toBe(false);
		expect(ids(session)).toEqual(expectedIds(boundary));
		expect(observer).not.toHaveBeenCalled();
		expect(flush).not.toHaveBeenCalled();
		expect(audio.play).not.toHaveBeenCalled();
	});

	it.each(boundaries)('a subscriber crossing %s during publish suppresses true success', (boundary) => {
		for (const run of [(s: Session) => s.playNext(3), (s: Session) => s.shuffle(() => 0)]) {
			const { session } = setup([A, B, C, D], 0);
			const start = session.snapshot().queue;
			let armed = false;
			session.subscribe((snap) => {
				if (armed && snap.queue !== start) { armed = false; cross(session, boundary); }
			});
			armed = true;
			expect(run(session)).toBe(false);
			expect(ids(session)).toEqual(expectedIds(boundary));
		}
	});
});

describe('actual edits cancel a pending Next seek and autoplay', () => {
	async function pendingNext() {
		const ctx = setup([A, B, C, D], 0);
		const first = mount(ctx.session, A);
		await startPlaying(ctx.session, first.audio);
		const moving = ctx.session.next();
		expect(ctx.flush).toHaveBeenCalledOnce();
		return { ...ctx, moving };
	}

	it.each([
		['playNext', (s: Session) => s.playNext(3), ['a', 'b', 'd', 'c']],
		['shuffle', (s: Session) => s.shuffle(() => 0), ['a', 'b', 'd', 'c']]
	] as [string, (s: Session) => boolean, string[]][])('%s before the flush resolves leaves the new source idle', async (_n, run, expected) => {
		const { session, release, moving } = await pendingNext();
		expect(currentId(session)).toBe('b');
		expect(run(session)).toBe(true);
		expect(ids(session)).toEqual(expected);
		const second = mount(session, B);
		second.audio.currentTime = 9;
		await release();
		await moving;
		expect(second.audio.play).not.toHaveBeenCalled();
		expect(second.audio.paused).toBe(true);
		expect(second.audio.currentTime).toBe(9);
		expect(session.snapshot().queueFeedback).toBe('');
	});
});
