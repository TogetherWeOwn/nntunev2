/**
 * Builder-owned editQueue tests. AudioFake is a unit double for the media
 * element's observable surface; it is NOT decoder or browser evidence.
 */
import { describe, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, currentTrack, type QueueState, type Track } from './queue.js';
import type { QueueAction } from './queueActions.js';
import type { SnapshotStorage } from '../library/cache.js';

const mk = (id: string, title = `Track ${id.toUpperCase()}`): Track => ({ id, title, artist: 'Artist', streamUrl: `/${id}` });
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

describe('editQueue feedback and no-ops', () => {
	it.each([
		[{ type: 'move', from: 0, to: 2 }, 'Track A moved to 3 of 4', false],
		[{ type: 'remove', index: 3 }, 'Track D removed from the queue.', true],
		[{ type: 'select', index: 3 }, 'Selected Track D', false],
		[{ type: 'clear' }, 'Queue cleared.', true],
		[{ type: 'trimPrevious' }, 'Removed 1 track before the current track.', false],
		[{ type: 'trimUpcoming' }, 'Removed 2 upcoming tracks.', false]
	] as [QueueAction, string, boolean][])('%j publishes exact feedback', (action, text, repeat) => {
		const { session } = setup([A, B, C, D], 1);
		session.setQueueFeedback('old');
		const version = session.snapshot().queueFeedbackVersion;
		expect(session.editQueue(action)).toBe(true);
		expect(session.snapshot().queueFeedback).toBe(text);
		expect(session.snapshot().queueFeedbackVersion).toBe(version + (repeat ? 1 : 0));
	});

	it.each([
		{ type: 'move', from: NaN, to: 1 }, { type: 'move', from: 0.5, to: 1 }, { type: 'move', from: 0, to: 9 },
		{ type: 'remove', index: NaN }, { type: 'remove', index: 1.5 }, { type: 'remove', index: -1 }, { type: 'remove', index: 4 },
		{ type: 'select', index: NaN }, { type: 'select', index: 2.5 }, { type: 'select', index: 7 }, { type: 'select', index: 1 }
	] as QueueAction[])('%j preserves queue identity, feedback, audio and observers', (action) => {
		const { session, flush } = setup([A, B, C, D], 1);
		const { audio } = mount(session, B);
		session.setQueueFeedback('keep me', true);
		const before = session.snapshot();
		const observer = vi.fn();
		session.onBeforeNavigate(observer);
		expect(session.editQueue(action)).toBe(false);
		const after = session.snapshot();
		expect(after.queue).toBe(before.queue);
		expect(after.queueFeedback).toBe('keep me');
		expect(after.queueFeedbackVersion).toBe(before.queueFeedbackVersion);
		expect(after.audio).toBe(audio.element);
		expect(observer).not.toHaveBeenCalled();
		expect(flush).not.toHaveBeenCalled();
		expectUntouched(audio);
	});

	it('trim no-ops at the edges leave everything alone', () => {
		const { session } = setup([A, B], 0);
		session.setQueueFeedback('keep');
		const queue = session.snapshot().queue;
		expect(session.editQueue({ type: 'trimPrevious' })).toBe(false);
		expect(session.snapshot().queue).toBe(queue);
		session.setQueue({ ...queue, currentIndex: 1 });
		const q2 = session.snapshot().queue;
		session.setQueueFeedback('keep');
		expect(session.editQueue({ type: 'trimUpcoming' })).toBe(false);
		expect(session.snapshot().queue).toBe(q2);
		expect(session.snapshot().queueFeedback).toBe('keep');
	});

	it('clear on an empty queue is a silent no-op', () => {
		const { session } = setup([]);
		session.setQueueFeedback('keep');
		expect(session.editQueue({ type: 'clear' })).toBe(false);
		expect(session.snapshot().queueFeedback).toBe('keep');
	});

	it('blank-title removal falls back to the id and equal text still bumps the version', () => {
		const { session } = setup([A, mk('x', '  '), mk('y', 'Same'), mk('z', 'Same')], 0);
		expect(session.editQueue({ type: 'remove', index: 1 })).toBe(true);
		expect(session.snapshot().queueFeedback).toBe('x removed from the queue.');
		expect(session.editQueue({ type: 'remove', index: 1 })).toBe(true);
		const first = session.snapshot();
		expect(first.queueFeedback).toBe('Same removed from the queue.');
		expect(session.editQueue({ type: 'remove', index: 1 })).toBe(true);
		const second = session.snapshot();
		expect(second.queueFeedback).toBe(first.queueFeedback);
		expect(second.queueFeedbackVersion).toBe(first.queueFeedbackVersion + 1);
	});
});

describe('noncurrent edits keep the live media element', () => {
	const actions: [string, QueueAction, string[]][] = [
		['move', { type: 'move', from: 3, to: 0 }, ['d', 'a', 'b', 'c']],
		['move across current', { type: 'move', from: 0, to: 3 }, ['b', 'c', 'd', 'a']],
		['remove before', { type: 'remove', index: 0 }, ['b', 'c', 'd']],
		['remove after', { type: 'remove', index: 3 }, ['a', 'b', 'c']],
		['trimPrevious', { type: 'trimPrevious' }, ['b', 'c', 'd']],
		['trimUpcoming', { type: 'trimUpcoming' }, ['a', 'b']]
	];
	it.each(['playing', 'paused'] as const)('%s element keeps identity, position and calls', async (mode) => {
		for (const [, action, expected] of actions) {
			const { session, flush } = setup([A, B, C, D], 1);
			const { audio } = mount(session, B);
			if (mode === 'playing') await startPlaying(session, audio);
			audio.currentTime = 42;
			audio.play.mockClear();
			expect(session.editQueue(action)).toBe(true);
			expect(ids(session)).toEqual(expected);
			expect(currentId(session)).toBe('b');
			expect(session.snapshot().audio).toBe(audio.element);
			expect(audio.currentTime).toBe(42);
			expect(audio.paused).toBe(mode === 'paused');
			expectUntouched(audio);
			expect(flush).not.toHaveBeenCalled();
			if (mode === 'playing') expect(session.snapshot().transport?.status).toBe('playing');
		}
	});
});

describe('current-track edits never autoplay', () => {
	it.each([
		['select', { type: 'select', index: 2 } as QueueAction, 'c'],
		['remove current', { type: 'remove', index: 1 } as QueueAction, 'c']
	])('%s while playing retires the element and leaves the replacement idle', async (_n, action, expected) => {
		const { session, flush, release } = setup([A, B, C, D], 1);
		const old = mount(session, B);
		await startPlaying(session, old.audio);
		old.audio.play.mockClear();
		expect(session.editQueue(action)).toBe(true);
		expect(currentId(session)).toBe(expected);
		expect(session.snapshot().audio).toBeNull();
		const next = mount(session, C);
		await release();
		expect(flush).not.toHaveBeenCalled();
		expect(next.audio.play).not.toHaveBeenCalled();
		expect(next.audio.paused).toBe(true);
		expect(next.audio.currentTime).toBe(0);
		expect(old.audio.play).not.toHaveBeenCalled();
	});

	it('removing the last current track falls back to the new last track without playing', async () => {
		const { session, release } = setup([A, B], 1);
		const old = mount(session, B);
		await startPlaying(session, old.audio);
		expect(session.editQueue({ type: 'remove', index: 1 })).toBe(true);
		expect(currentId(session)).toBe('a');
		const next = mount(session, A);
		await release();
		expect(next.audio.play).not.toHaveBeenCalled();
	});

	it('clear drops audio and the queue and announces once', async () => {
		const { session, release } = setup([A, B], 0);
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		audio.play.mockClear();
		expect(session.editQueue({ type: 'clear' })).toBe(true);
		const s = session.snapshot();
		expect(s.queue).toEqual({ tracks: [], currentIndex: -1 });
		expect(s.audio).toBeNull();
		expect(s.queueFeedback).toBe('Queue cleared.');
		expect(s.queueFeedbackExposed).toBe(true);
		await release();
		expect(audio.play).not.toHaveBeenCalled();
	});
});

describe('observer ordering and reentrant fencing', () => {
	it('observer sees the old queue, a cleared region and the proposed queue before publish', () => {
		const { session } = setup([A, B, C], 0);
		mount(session, A);
		session.setQueueFeedback('stale');
		const seen: { current?: string; feedback: string; next: QueueState; same: boolean }[] = [];
		session.onBeforeNavigate((next) => {
			seen.push({ current: currentId(session), feedback: session.snapshot().queueFeedback, next, same: next === session.snapshot().queue });
		});
		expect(session.editQueue({ type: 'select', index: 2 })).toBe(true);
		expect(seen).toHaveLength(1);
		expect(seen[0]).toMatchObject({ current: 'a', feedback: '', same: false });
		expect(currentTrack(seen[0]!.next)?.id).toBe('c');
		expect(currentId(session)).toBe('c');
		expect(session.snapshot().queueFeedback).toBe('Selected Track C');
	});

	it('runs every observer once, in registration order, and supports unsubscribe', () => {
		const { session } = setup([A, B, C], 0);
		const order: string[] = [];
		const off = session.onBeforeNavigate(() => { order.push('one'); });
		session.onBeforeNavigate(() => { order.push('two'); });
		session.editQueue({ type: 'remove', index: 2 });
		off();
		session.editQueue({ type: 'remove', index: 1 });
		expect(order).toEqual(['one', 'two', 'two']);
	});

	it.each(['auth', 'source', 'destroy'] as const)('an observer crossing a %s boundary fences the edit', (boundary) => {
		const { session, flush } = setup([A, B, C], 0);
		const { audio } = mount(session, A);
		session.onBeforeNavigate(() => {
			if (boundary === 'auth') session.resetAtAuthBoundary();
			else if (boundary === 'destroy') session.destroy();
			else session.setQueue(createQueue([D]));
		});
		expect(session.editQueue({ type: 'trimUpcoming' })).toBe(false);
		const s = session.snapshot();
		expect(ids(session)).toEqual(boundary === 'source' ? ['d'] : []);
		expect(s.queueFeedback).toBe('');
		expect(s.queueFeedbackVersion).toBe(0);
		if (boundary !== 'source') expect(s.queueFeedbackExposed).toBe(false);
		expect(flush).not.toHaveBeenCalled();
		expect(audio.play).not.toHaveBeenCalled();
	});
});

describe('lifecycle', () => {
	it('before start is a harmless false that does not mutate', () => {
		const session = createPlayerSession({ flush: async () => {} });
		session.setQueue(createQueue([A, B]));
		const before = session.snapshot();
		expect(session.editQueue({ type: 'clear' })).toBe(false);
		expect(session.editQueue({ type: 'remove', index: 9 })).toBe(false);
		expect(session.snapshot().queue).toBe(before.queue);
		expect(session.snapshot().queueFeedback).toBe('');
	});

	it('after destroy is a harmless false', () => {
		const { session } = setup([A, B], 0);
		session.destroy();
		const before = session.snapshot();
		expect(() => session.editQueue({ type: 'select', index: 1 })).not.toThrow();
		expect(session.editQueue({ type: 'clear' })).toBe(false);
		expect(session.snapshot().queue).toBe(before.queue);
		expect(session.snapshot().queueFeedback).toBe('');
	});
});

describe('explicit pending Next then metadata edit', () => {
	async function pendingNext() {
		const ctx = setup([A, B, C], 0);
		const first = mount(ctx.session, A);
		await startPlaying(ctx.session, first.audio);
		const moving = ctx.session.next();
		expect(ctx.flush).toHaveBeenCalledOnce();
		return { ...ctx, first, moving };
	}

	it('control: without an edit the pending Next starts the new source from zero', async () => {
		const { session, release, moving } = await pendingNext();
		const second = mount(session, B);
		second.audio.currentTime = 9;
		await release();
		await moving;
		expect(second.audio.currentTime).toBe(0);
		expect(second.audio.play).toHaveBeenCalledOnce();
	});

	it('a metadata edit before the flush resolves cancels the late seek and autoplay', async () => {
		const { session, release, moving } = await pendingNext();
		expect(currentId(session)).toBe('b');
		expect(session.editQueue({ type: 'move', from: 2, to: 0 })).toBe(true);
		expect(ids(session)).toEqual(['c', 'a', 'b']);
		const second = mount(session, B);
		second.audio.currentTime = 9;
		await release();
		await moving;
		expect(second.audio.play).not.toHaveBeenCalled();
		expect(second.audio.paused).toBe(true);
		expect(second.audio.currentTime).toBe(9);
		expect(session.snapshot().queueFeedback).toBe('Track C moved to 1 of 3');
	});
});
