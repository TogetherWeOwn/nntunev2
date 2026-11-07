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

/** Private-source lifecycle units only: AudioFake does not prove browser teardown or decoding. */
describe('queued Play source-input lifetime', () => {
	const metadata = [A, B, C].map((track) => ({ ...track, streamUrl: '' }));

	function sourceContext() {
		const ctx = setup(metadata);
		const owner = ctx.session.sourceInputOwner();
		const accept = (tracks = [{ id: 'a', streamUrl: '/accepted' }], authoritative = false) =>
			ctx.session.setQueue(ctx.session.snapshot().queue, { owner, tracks, authoritative });
		return { ...ctx, owner, accept };
	}

	it('keeps accepted sources private and detached, without hydrating metadata or future ids', async () => {
		const { session, accept, release, flush } = sourceContext();
		const input = { id: 'a', streamUrl: '/accepted', catalogueOnly: 'not retained' };
		expect(accept([input, { id: 'outside', streamUrl: '/outside' }])).toBe(true);
		input.streamUrl = '/mutated';
		expect(session.snapshot().queue.tracks.every((track) => track.streamUrl === '')).toBe(true);
		expect(JSON.stringify(session.snapshot())).not.toContain('/accepted');
		expect(JSON.stringify(session.snapshot())).not.toContain('catalogueOnly');
		expect(flush).not.toHaveBeenCalled();
		const playing = session.playQueuedTrack('a');
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/accepted');
		expect(session.snapshot().queue.tracks.slice(1).every((track) => track.streamUrl === '')).toBe(true);
		const { audio } = mount(session, { ...A, streamUrl: '/accepted' });
		await release();
		await playing;
		expect(audio.play).toHaveBeenCalledOnce();
		expect(session.snapshot().queueFeedback).toBe('Playing Track A.');
		session.setQueue(createQueue([{ ...A, id: 'outside', streamUrl: '' }]));
		await session.playQueuedTrack('outside');
		expect(session.snapshot().queueFeedback).toBe('Selected Track A; no stream to play.');
		session.destroy();
	});

	it.each(['accepted', 'blocked', 'no-stream'] as const)('preserves the exact %s outcome and single-attempt oracle', async (outcome) => {
		const { session, accept, release, flush } = sourceContext();
		const queue = session.snapshot().queue;
		session.setQueue({ ...queue, tracks: queue.tracks.map((track) => track.id === 'a' ? { ...track, title: 'Dawn Chorus' } : track) });
		accept([{ id: 'a', streamUrl: outcome === 'no-stream' ? '' : '/accepted' }]);
		const playing = session.playQueuedTrack('a');
		let audio: AudioFake | undefined;
		if (outcome !== 'no-stream') {
			audio = mount(session, { ...A, streamUrl: '/accepted' }).audio;
			if (outcome === 'blocked') audio.play.mockRejectedValue(new Error('gesture rejected'));
			await release();
		}
		await playing;
		expect(ids(session)).toEqual(['a', 'b', 'c']);
		expect(currentId(session)).toBe('a');
		expect(session.snapshot().queueFeedback).toBe(outcome === 'accepted' ? 'Playing Dawn Chorus.'
			: outcome === 'blocked' ? 'Selected Dawn Chorus, paused: playback was blocked.'
				: 'Selected Dawn Chorus; no stream to play.');
		expect(session.snapshot().queueFeedbackVersion).toBe(1);
		if (audio) expect(audio.play).toHaveBeenCalledOnce();
		else expect(flush).not.toHaveBeenCalled();
		session.destroy();
	});

	it.each(['resolve', 'reject'] as const)('a superseded delayed %s preserves the winner and original attempt count', async (result) => {
		const { session, accept, release } = sourceContext();
		accept();
		const playing = session.playQueuedTrack('a');
		const { audio } = mount(session, { ...A, streamUrl: '/accepted' });
		let finish!: () => void;
		audio.play.mockImplementation(() => new Promise<void>((resolve, reject) => {
			finish = result === 'resolve' ? resolve : () => reject(new Error('late rejection'));
		}));
		await release();
		expect(audio.play).toHaveBeenCalledOnce();
		session.editQueue({ type: 'select', index: 1 });
		const winning = session.snapshot();
		finish();
		await playing;
		expect(currentId(session)).toBe('b');
		expect(session.snapshot().queueFeedback).toBe('Selected Track B');
		expect(session.snapshot().queueFeedbackVersion).toBe(winning.queueFeedbackVersion);
		expect(session.snapshot().audio).toBeNull();
		expect(audio.play).toHaveBeenCalledOnce();
		session.destroy();
	});

	it.each(['refused', 'evicted'] as const)('accepted live evidence is independent of %s storage', async (mode) => {
		const { session, release } = setup(metadata);
		session.destroy();
		const saved = new Map<string, string>();
		const storage = {
			getItem: vi.fn((key: string) => saved.get(key) ?? null),
			setItem: vi.fn((key: string, value: string) => {
				if (mode === 'refused') throw new Error('storage refused');
				saved.set(key, value);
			}),
			removeItem: vi.fn((key: string) => { saved.delete(key); })
		};
		session.start(storage);
		session.setQueue(createQueue(metadata), { owner: session.sourceInputOwner(), tracks: [{ id: 'a', streamUrl: '/accepted' }] });
		saved.clear();
		storage.getItem.mockClear();
		expect(storage.setItem).not.toHaveBeenCalled();
		const playing = session.playQueuedTrack('a');
		// Source resolution itself never rereads the persisted catalogue.
		expect(storage.getItem).not.toHaveBeenCalled();
		const { audio } = mount(session, { ...A, streamUrl: '/accepted' });
		await release();
		await playing;
		expect(audio.play).toHaveBeenCalledOnce();
		expect(session.snapshot().queueFeedback).toBe('Playing Track A.');
		expect([...saved.values()].join('')).not.toContain('/accepted');
		session.destroy();
	});

	it.each(['empty', 'changed', 'removed'] as const)('an authoritative %s input replaces old fallback evidence', async (change) => {
		const { session, accept, release } = sourceContext();
		accept();
		accept(change === 'removed' ? [] : [{ id: 'a', streamUrl: change === 'empty' ? '' : '/new' }], true);
		const playing = session.playQueuedTrack('a');
		if (change === 'changed') {
			expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/new');
			const { audio } = mount(session, { ...A, streamUrl: '/new' });
			await release();
			await playing;
			expect(audio.play).toHaveBeenCalledOnce();
		} else {
			await playing;
			expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('');
			expect(session.snapshot().queueFeedback).toBe('Selected Track A; no stream to play.');
		}
		session.destroy();
	});

	it('retains accepted evidence when no authoritative replacement commits', async () => {
		const { session, accept, release } = sourceContext();
		accept();
		// Models absence of a successful replacement, not an exercised HTTP failure.
		session.setQueueLiveRegions(false);
		session.setQueueLiveRegions(true);
		session.setQueueFilter('Track');
		session.toggleQueueSelection('b');
		const playing = session.playQueuedTrack('a');
		const { audio } = mount(session, { ...A, streamUrl: '/accepted' });
		await release();
		await playing;
		expect(audio.play).toHaveBeenCalledOnce();
		expect(session.snapshot().queueFeedback).toBe('Playing Track A.');
		session.destroy();
	});

	it('keeps a lawful queued source and the same paused decoder across authoritative source omission', async () => {
		const { session } = setup([A, B]);
		const { audio } = mount(session, A);
		audio.currentTime = 37;
		const before = session.snapshot();
		expect(session.setQueue(before.queue, { owner: session.sourceInputOwner(), tracks: [], authoritative: true })).toBe(true);
		expect(session.snapshot()).toBe(before);
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/a');
		expect(session.snapshot().audio).toBe(audio.element);
		expect(audio.currentTime).toBe(37);
		expectUntouched(audio);
		session.destroy();
	});

	it('prefers the existing queued source over a different accepted fallback', async () => {
		const { session, release } = setup([A, B]);
		session.setQueue(session.snapshot().queue, { owner: session.sourceInputOwner(), tracks: [{ id: 'a', streamUrl: '/other' }] });
		const { audio } = mount(session, A);
		audio.currentTime = 23;
		const playing = session.playQueuedTrack('a');
		await release();
		await playing;
		expect(session.snapshot().audio).toBe(audio.element);
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/a');
		expect(audio.currentTime).toBe(23);
		expect(audio.play).toHaveBeenCalledOnce();
		expect(audio.pause).not.toHaveBeenCalled();
		session.destroy();
	});

	it('remove/re-add cannot resurrect the old incarnation or its pending Play', async () => {
		const { session, accept, release } = sourceContext();
		accept();
		const playing = session.playQueuedTrack('a');
		session.editQueue({ type: 'remove', index: 0 });
		session.setQueue(createQueue(metadata));
		await release();
		await playing;
		await session.playQueuedTrack('a');
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('');
		expect(session.snapshot().queueFeedback).toBe('Selected Track A; no stream to play.');
		session.destroy();
	});

	it.each(['resetAtAuthBoundary', 'destroy'] as const)('%s rejects a stale producer and clears private evidence', async (boundary) => {
		const { session, accept, owner, release } = sourceContext();
		accept();
		const playing = session.playQueuedTrack('a');
		session[boundary]();
		expect(session.setQueue(createQueue(metadata), { owner, tracks: [{ id: 'a', streamUrl: '/old-account' }] })).toBe(false);
		await release();
		await playing;
		expect(session.snapshot().queue.tracks).toEqual([]);
		expect(session.snapshot().queueFeedback).toBe('');
		if (boundary === 'resetAtAuthBoundary') {
			session.setQueue(createQueue(metadata));
			await session.playQueuedTrack('a');
			expect(session.snapshot().queueFeedback).toBe('Selected Track A; no stream to play.');
		}
		session.destroy();
	});

	it('commits membership and detached evidence together before notifying queue subscribers', async () => {
		const { session, release } = setup([]);
		let playing: Promise<void> | undefined;
		let armed = true;
		const stop = session.subscribe((snapshot) => {
			if (armed && snapshot.queue.tracks.some((track) => track.id === 'a')) {
				armed = false;
				playing = session.playQueuedTrack('a');
			}
		});
		// The subscriber supersedes the outer publication; it must not be reported committed.
		expect(session.setQueue(createQueue(metadata), { owner: session.sourceInputOwner(), tracks: [{ id: 'a', streamUrl: '/atomic' }] })).toBe(false);
		expect(playing).toBeDefined();
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/atomic');
		const { audio } = mount(session, { ...A, streamUrl: '/atomic' });
		await release();
		await playing;
		expect(audio.play).toHaveBeenCalledOnce();
		stop();
		session.destroy();
	});

	it.each(['feedback', 'queue', 'export', 'attachment'] as const)('auth retirement at the %s notification cannot continue old Play', async (seam) => {
		const { session, accept, release, flush } = sourceContext();
		accept();
		let armed = false;
		const stop = (seam === 'export' ? session.queueExport : session).subscribe(() => {
			if (!armed) return;
			const snapshot = session.snapshot();
			if (seam === 'queue' && currentTrack(snapshot.queue)?.streamUrl !== '/accepted') return;
			if (seam === 'attachment' && !snapshot.audio) return;
			armed = false;
			session.resetAtAuthBoundary();
		});
		armed = true;
		const playing = session.playQueuedTrack('a');
		const audio = new AudioFake('/accepted');
		if (seam === 'attachment') session.attach(audio.element);
		await release();
		await playing;
		expect(session.snapshot().queue.tracks).toEqual([]);
		expect(session.snapshot().audio).toBeNull();
		expect(session.snapshot().queueFeedback).toBe('');
		expect(audio.play).not.toHaveBeenCalled();
		if (seam !== 'attachment') expect(flush).not.toHaveBeenCalled();
		stop();
		session.destroy();
	});

	it('authoritative replacement after hydration retires a pending fallback-dependent attempt', async () => {
		const { session, accept, release } = sourceContext();
		accept();
		const playing = session.playQueuedTrack('a');
		const { audio } = mount(session, { ...A, streamUrl: '/accepted' });
		accept([{ id: 'a', streamUrl: '' }], true);
		await release();
		await playing;
		expect(audio.play).not.toHaveBeenCalled();
		expect(session.snapshot().queueFeedback).toBe('');
		// The accepted queued source is not erased by catalogue omission.
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/accepted');
		session.destroy();
	});

	it('auth retirement during source disconnect prevents stale hydration, attachment and playback', async () => {
		const { session, release, flush } = setup([A, { ...B, streamUrl: '' }]);
		session.setQueue(session.snapshot().queue, { owner: session.sourceInputOwner(), tracks: [{ id: 'b', streamUrl: '/fallback-b' }] });
		const { audio } = mount(session, A);
		const before = session.snapshot().transport;
		let armed = false;
		const stop = session.subscribe((snapshot) => {
			if (!armed || snapshot.transport === before) return;
			armed = false;
			session.resetAtAuthBoundary();
		});
		armed = true;
		const playing = session.playQueuedTrack('b');
		await release();
		await playing;
		expect(session.snapshot().queue).toEqual(createQueue([]));
		expect(session.snapshot().audio).toBeNull();
		expect(session.snapshot().queueFeedback).toBe('');
		expect(audio.play).not.toHaveBeenCalled();
		expect(flush).not.toHaveBeenCalled();
		stop();
		session.destroy();
	});

	it('auth retirement at the completed transport attachment notification cannot reconnect old audio', async () => {
		const { session, accept, release } = sourceContext();
		accept();
		const playing = session.playQueuedTrack('a');
		const audio = new AudioFake('/accepted');
		const before = session.snapshot().transport;
		let armed = false;
		const stop = session.subscribe((snapshot) => {
			if (!armed || snapshot.audio !== audio.element || snapshot.transport === before) return;
			armed = false;
			session.resetAtAuthBoundary();
		});
		armed = true;
		session.attach(audio.element);
		await release();
		await playing;
		await session.play();
		expect(session.snapshot().audio).toBeNull();
		expect(session.snapshot().queue.tracks).toEqual([]);
		expect(audio.play).not.toHaveBeenCalled();
		stop();
		session.destroy();
	});

	it('a feedback subscriber retiring the action prevents later route aftermath listeners', async () => {
		const { session, accept } = sourceContext();
		accept();
		const listener = vi.fn();
		session.onBeforeNavigate(listener);
		let armed = false;
		const stop = session.subscribe(() => {
			if (!armed) return;
			armed = false;
			session.resetAtAuthBoundary();
		});
		armed = true;
		await session.playQueuedTrack('a');
		expect(listener).not.toHaveBeenCalled();
		expect(session.snapshot().queue.tracks).toEqual([]);
		stop();
		session.destroy();
	});

	it('a retiring navigation listener prevents the next old-action listener', async () => {
		const { session, accept } = sourceContext();
		accept();
		session.onBeforeNavigate(() => session.resetAtAuthBoundary());
		const later = vi.fn();
		session.onBeforeNavigate(later);
		await session.playQueuedTrack('a');
		expect(later).not.toHaveBeenCalled();
		expect(session.snapshot().queueFeedback).toBe('');
		session.destroy();
	});

	it('a newer authoritative source commit during export reset cannot be overwritten by ingress', async () => {
		const { session, accept, release, owner } = sourceContext();
		accept();
		let armed = false;
		const stop = session.queueExport.subscribe(() => {
			if (!armed) return;
			armed = false;
			accept([{ id: 'a', streamUrl: '/newer' }], true);
		});
		armed = true;
		expect(session.setQueue(createQueue([...metadata, { ...D, streamUrl: '' }]), {
			owner, tracks: [{ id: 'a', streamUrl: '/obsolete' }, { id: 'd', streamUrl: '/d' }]
		})).toBe(false);
		expect(ids(session)).toEqual(['a', 'b', 'c']);
		const playing = session.playQueuedTrack('a');
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/newer');
		const { audio } = mount(session, { ...A, streamUrl: '/newer' });
		await release();
		await playing;
		expect(audio.play).toHaveBeenCalledOnce();
		stop();
		session.destroy();
	});

	it.each(['remove', 'playNext'] as const)('explicit %s operands never become selection or alter a surviving decoder', (action) => {
		const { session } = setup([A, B, C, D]);
		const { audio } = mount(session, A);
		audio.currentTime = 31;
		session.toggleQueueSelection('b');
		session.setQueueFilter('Track B');
		const selection = session.snapshot().queueSelection;
		const seen: string[][] = [];
		const stop = session.subscribe((snapshot) => { seen.push([...snapshot.queueSelection.ids]); });
		const operands = ['d', 'c', 'd', 'missing'];
		expect(action === 'remove' ? session.removeQueuedIds(operands) : session.playNextQueuedIds(operands)).toBe(true);
		expect(ids(session)).toEqual(action === 'remove' ? ['a', 'b'] : ['a', 'c', 'd', 'b']);
		expect(session.snapshot().queueSelection).toBe(selection);
		expect(seen.every((ids) => ids.length === 1 && ids[0] === 'b')).toBe(true);
		expect(session.snapshot().queueFeedback).toBe(action === 'remove' ? '2 selected tracks removed from queue.' : 'Tracks placed next in the queue.');
		expect(session.snapshot().queueFeedbackVersion).toBe(1);
		expect(session.snapshot().audio).toBe(audio.element);
		expect(audio.currentTime).toBe(31);
		expectUntouched(audio);
		stop();
		session.destroy();
	});

	it.each(['remove', 'playNext'] as const)('explicit %s true no-ops keep selection, feedback and snapshot identity', (action) => {
		const { session, flush } = setup([A, B, C]);
		session.toggleQueueSelection('c');
		session.setQueueFeedback('keep', true);
		const before = session.snapshot();
		expect(action === 'remove' ? session.removeQueuedIds(['missing']) : session.playNextQueuedIds(['a', 'missing'])).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(flush).not.toHaveBeenCalled();
		session.destroy();
	});

	it.each(['auth', 'destroy', 'replacement'] as const)('explicit operands cannot overwrite a reentrant %s', (boundary) => {
		for (const action of ['remove', 'playNext'] as const) {
			const { session } = setup([A, B, C, D]);
			session.toggleQueueSelection('b');
			let armed = false;
			const stop = session.queueExport.subscribe(() => {
				if (!armed) return;
				armed = false;
				if (boundary === 'auth') session.resetAtAuthBoundary();
				else if (boundary === 'destroy') session.destroy();
				else session.setQueue(createQueue([D]));
			});
			armed = true;
			expect(action === 'remove' ? session.removeQueuedIds(['c']) : session.playNextQueuedIds(['d'])).toBe(false);
			expect(session.snapshot().queue).toEqual(createQueue(boundary === 'replacement' ? [D] : []));
			expect(session.snapshot().queueFeedback).toBe('');
			stop();
			session.destroy();
		}
	});
});

describe('incoming bridge commit fences', () => {
	it('an exact reorder no-op preserves the snapshot, selection and prior feedback', () => {
		const { session, flush } = setup([A, B, C]);
		session.toggleQueueSelection('c');
		session.setQueueFeedback('keep', true);
		const before = session.snapshot();
		expect(session.reorderQueuedIds(['c'], null)).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(flush).not.toHaveBeenCalled();
		session.destroy();
	});

	it('authoritative replacement during navigation retires an older paired insertion before membership publication', async () => {
		const metadata = [{ ...A, streamUrl: '' }];
		const { session, release } = setup(metadata);
		const owner = session.sourceInputOwner();
		session.setQueue(session.snapshot().queue, { owner, tracks: [{ id: 'a', streamUrl: '/accepted' }] });
		const before = session.snapshot().queue;
		const stop = session.onBeforeNavigate(() => {
			session.setQueue(before, { owner, tracks: [{ id: 'a', streamUrl: '/newer' }], authoritative: true });
		});
		expect(session.insertQueuedTracks([{ ...D, streamUrl: '' }], null, {
			owner, tracks: [{ id: 'a', streamUrl: '/obsolete' }, { id: 'd', streamUrl: '/d' }]
		})).toBe(false);
		expect(session.snapshot().queue).toBe(before);
		expect(session.snapshot().queueFeedback).toBe('');
		stop();
		const playing = session.playQueuedTrack('a');
		expect(currentTrack(session.snapshot().queue)?.streamUrl).toBe('/newer');
		const { audio } = mount(session, { ...A, streamUrl: '/newer' });
		await release();
		await playing;
		expect(audio.play).toHaveBeenCalledOnce();
		expect(session.snapshot().queueFeedback).toBe('Playing Track A.');
		session.destroy();
	});

	it('old audio cleanup cannot erase a new attachment created by its disconnect notification', () => {
		const { session } = setup([A, B]);
		const { detach } = mount(session, A);
		let armed = false;
		let winner: AudioFake | undefined;
		const stop = session.subscribe((snapshot) => {
			if (!armed || snapshot.transport?.status !== 'selected') return;
			armed = false;
			session.setQueue({ ...createQueue([A, B]), currentIndex: 1 });
			winner = mount(session, B).audio;
		});
		armed = true;
		detach();
		expect(winner).toBeDefined();
		expect(session.snapshot().audio).toBe(winner!.element);
		expect(currentId(session)).toBe('b');
		expect(session.snapshot().audio?.getAttribute('src')).toBe('/b');
		stop();
		session.destroy();
	});
});
