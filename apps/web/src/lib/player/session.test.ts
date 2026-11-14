import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createPlayerSession } from './session.js';
import { createQueue, currentTrack, moveTrack, type Track } from './queue.js';
import { RESUME_STORAGE_KEY } from './resume.js';
import type { SnapshotStorage } from '../library/cache.js';

const A: Track = { id: 'a', title: 'Track A', artist: 'Artist', streamUrl: '/a' };
const B: Track = { id: 'b', title: 'Track B', artist: 'Artist', streamUrl: '/b' };
const C: Track = { id: 'c', title: 'Track C', artist: 'Artist', streamUrl: '/c' };
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

/** A flush whose resolution the test controls, one pending call at a time. */
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

function mount(session: ReturnType<typeof setup>['session'], track: Track) {
	const audio = new AudioFake(track.streamUrl);
	const detach = session.attach(audio.element);
	return { audio, detach };
}

async function startPlaying(session: ReturnType<typeof setup>['session'], audio: AudioFake) {
	await session.play();
	audio.emit('playing');
}

function end(audio: AudioFake) {
	audio.currentTime = audio.duration;
	audio.ended = true;
	audio.paused = true;
	audio.emit('pause');
	audio.emit('ended');
}

afterEach(() => { vi.unstubAllGlobals(); });

describe('player session', () => {
	it('is isolated per instance and inert before start (SSR safe)', async () => {
		const first = createPlayerSession({ flush: async () => {} });
		const second = createPlayerSession({ flush: async () => {} });
		first.setQueue(createQueue([A, B]));
		first.setRepeat('one');
		expect(second.snapshot().queue.tracks).toEqual([]);
		expect(second.snapshot().repeat).toBe('off');
		expect(get(first).queue.tracks).toHaveLength(2);
		expect(get(second)).toMatchObject({ audio: null, transport: null, announcement: '' });
		// Without start() there is no transport: controls are harmless no-ops.
		await first.play();
		first.pause();
		first.tick();
		first.persistPosition();
		expect(first.snapshot().transport).toBeNull();
		const unmounted = new AudioFake(A.streamUrl);
		first.attach(unmounted.element)();
		expect(unmounted.play).not.toHaveBeenCalled();
	});

	it('keeps the queue when subscribers unmount and remount, and when the audio element remounts', () => {
		const { session } = setup([A, B, C]);
		const stop = session.subscribe(() => {});
		session.setQueue({ ...session.snapshot().queue, currentIndex: 1 });
		stop();
		const values: string[] = [];
		const stopAgain = session.subscribe((s) => values.push(currentTrack(s.queue)?.id ?? ''));
		expect(values).toEqual(['b']);
		stopAgain();

		const first = mount(session, B);
		first.detach();
		expect(session.snapshot().audio).toBeNull();
		expect(currentTrack(session.snapshot().queue)?.id).toBe('b');
		const second = mount(session, B);
		expect(session.snapshot().audio).toBe(second.audio.element);
		expect(session.snapshot().transport?.status).toBe('selected');
	});

	it('refuses an element whose src does not match the selected track', () => {
		const { session } = setup();
		const stale = new AudioFake('/other');
		session.attach(stale.element)();
		expect(session.snapshot().audio).toBeNull();
	});

	it('treats refused or corrupt repeat storage as off and keeps in-memory choices', () => {
		const refusing = createPlayerSession({ flush: async () => {} });
		const throwing = {
			getItem: vi.fn(() => { throw new Error('blocked'); }),
			setItem: vi.fn(() => { throw new Error('blocked'); }),
			removeItem: vi.fn(() => { throw new Error('blocked'); })
		};
		refusing.start(throwing);
		expect(refusing.snapshot().repeat).toBe('off');
		refusing.setRepeat('queue');
		expect(refusing.snapshot().repeat).toBe('queue');

		const corrupt = createPlayerSession({ flush: async () => {} });
		corrupt.start(makeStorage({ [REPEAT_KEY]: '{not a mode' }).storage);
		expect(corrupt.snapshot().repeat).toBe('off');

		const restored = createPlayerSession({ flush: async () => {} });
		restored.start(makeStorage({ [REPEAT_KEY]: 'one' }).storage);
		expect(restored.snapshot().repeat).toBe('one');

		const none = createPlayerSession({ flush: async () => {} });
		none.start(null);
		expect(none.snapshot().repeat).toBe('off');
		expect(none.snapshot().transport).toBeNull();
	});

	it('persists the repeat choice through storage', () => {
		const { session, values } = setup();
		session.setRepeat('queue');
		expect(values.get(REPEAT_KEY)).toBe('queue');
	});

	it('does not reset repeat, queue or playback when start is called again', async () => {
		const { session } = setup([A, B], { [REPEAT_KEY]: 'one' });
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		const later = makeStorage({ [REPEAT_KEY]: 'queue' });
		session.start(later.storage);
		expect(later.storage.getItem).not.toHaveBeenCalled();
		expect(session.snapshot().repeat).toBe('one');
		expect(session.snapshot().queue.tracks).toHaveLength(2);
		expect(session.snapshot().audio).toBe(audio.element);
		expect(session.snapshot().transport?.status).toBe('playing');
		expect(audio.pause).not.toHaveBeenCalled();
	});

	it('pauses and retires the old element when the selected source changes', async () => {
		const { session } = setup();
		const { audio: old } = mount(session, A);
		await startPlaying(session, old);
		session.setQueue({ ...session.snapshot().queue, currentIndex: 1 });
		expect(old.pause).toHaveBeenCalled();
		expect(old.paused).toBe(true);
		expect(session.snapshot().audio).toBeNull();
		expect(session.snapshot().transport?.status).toBe('selected');

		// Late events from the retired element cannot change the new selection.
		const before = session.snapshot().transport;
		old.currentTime = 50;
		old.emit('playing');
		old.emit('error');
		end(old);
		expect(session.snapshot().transport).toEqual(before);
		expect(session.snapshot().announcement).toBe('');

		const replacement = mount(session, B);
		expect(session.snapshot().audio).toBe(replacement.audio.element);
		// The retired element's teardown must not detach its replacement.
		const { detach } = mount(session, B);
		detach();
		expect(session.snapshot().audio).toBeNull();
	});

	it('also treats a changed stream URL for the same id as a source change', async () => {
		const { session } = setup();
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		const [, ...rest] = session.snapshot().queue.tracks;
		session.setQueue({ tracks: [{ ...A, streamUrl: '/a-v2' }, ...rest], currentIndex: 0 });
		expect(audio.pause).toHaveBeenCalled();
		expect(session.snapshot().audio).toBeNull();
	});

	it('keeps playing, connected and attached when a reorder keeps the same source', async () => {
		const { session } = setup([A, B, C]);
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		audio.pause.mockClear();
		const reordered = moveTrack(session.snapshot().queue, 2, 1);
		expect(reordered).not.toBe(session.snapshot().queue);
		session.setQueue(reordered);
		session.setQueue({ ...reordered });
		expect(audio.pause).not.toHaveBeenCalled();
		expect(session.snapshot().audio).toBe(audio.element);
		expect(session.snapshot().transport?.status).toBe('playing');
		// Still connected: element events keep reaching the transport.
		audio.currentTime = 33;
		audio.emit('timeupdate');
		expect(session.snapshot().transport?.position).toBe(33);
	});

	it('discards audio, queue and the previous account position at an auth boundary', async () => {
		const { session, values } = setup();
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		audio.currentTime = 40;
		session.resetAtAuthBoundary();
		expect(audio.pause).toHaveBeenCalled();
		expect(session.snapshot()).toMatchObject({
			audio: null, announcement: '', announcementVersion: 0, queue: { tracks: [], currentIndex: -1 }
		});
		expect(session.snapshot().transport?.status).toBe('selected');
		expect(values.get(RESUME_STORAGE_KEY) ?? '').not.toContain('"a"');
		audio.emit('playing');
		expect(session.snapshot().transport?.status).toBe('selected');
		// The next account starts from a clean queue and a fresh element.
		session.setQueue(createQueue([C]));
		const next = mount(session, C);
		expect(session.snapshot().audio).toBe(next.audio.element);
		expect(next.audio.currentTime).toBe(0);
	});

	describe('persistent queue feedback', () => {
		it('retains feedback across subscriber remounts without storing it', () => {
			const { session, storage } = setup();
			const off = session.subscribe(() => {});
			session.setQueueFeedback('Track B removed from the queue.', true);
			off();
			const seen: string[] = [];
			const stop = session.subscribe((snapshot) => seen.push(snapshot.queueFeedback));
			expect(seen).toEqual(['Track B removed from the queue.']);
			expect(storage.setItem).not.toHaveBeenCalled();
			stop();
		});

		it('refreshes equal outcomes only when the action requests it', () => {
			const { session } = setup();
			session.setQueueFeedback('Removed Untitled.', true);
			session.setQueueFeedback('Removed Untitled.', true);
			expect(session.snapshot().queueFeedbackVersion).toBe(2);
			session.setQueueFeedback('');
			expect(session.snapshot()).toMatchObject({ queueFeedback: '', queueFeedbackVersion: 2 });
		});

		it('clears old feedback on queue replacement and clamped navigation without a route observer', async () => {
			const { session } = setup([A]);
			session.setQueueFeedback('Old move');
			await session.next();
			expect(session.snapshot().queueFeedback).toBe('');
			session.setQueueFeedback('Old selection');
			session.setQueue(createQueue([B]));
			expect(session.snapshot().queueFeedback).toBe('');
		});

		it('drops account metadata and feedback identity at the auth boundary', () => {
			const { session } = setup();
			session.setQueueFeedback('Removed Track A.', true);
			session.resetAtAuthBoundary();
			expect(session.snapshot()).toMatchObject({ queueFeedback: '', queueFeedbackVersion: 0, queueFeedbackExposed: false });
			session.exposeQueueFeedback();
			expect(session.snapshot()).toMatchObject({ queueFeedback: '', queueFeedbackVersion: 0, queueFeedbackExposed: true });
			session.setQueueFeedback('New account');
			session.destroy();
			expect(session.snapshot().queueFeedback).toBe('');
		});

		it('retires route work synchronously on natural advancement, then publishes the played outcome', async () => {
			const { session, release } = setup();
			const first = mount(session, A);
			await startPlaying(session, first.audio);
			session.setQueueFeedback('Old move');
			const observer = vi.fn((next) => {
				expect(currentTrack(session.snapshot().queue)?.id).toBe('a');
				expect(currentTrack(next)?.id).toBe('b');
				expect(session.snapshot().queueFeedback).toBe('');
			});
			session.onBeforeNavigate(observer);
			end(first.audio);
			expect(observer).toHaveBeenCalledOnce();
			mount(session, B);
			await release();
			expect(session.snapshot()).toMatchObject({ queueFeedback: 'Playing Track B.', queueFeedbackVersion: 1 });
		});

		for (const boundary of ['auth', 'selection', 'destroy'] as const) {
			it(`does not advance after a natural-end observer crosses ${boundary}`, async () => {
				const { session, release, flush } = setup();
				const first = mount(session, A);
				await startPlaying(session, first.audio);
				session.onBeforeNavigate(() => {
					if (boundary === 'auth') session.resetAtAuthBoundary();
					else if (boundary === 'selection') session.setQueue(createQueue([C]));
					else session.destroy();
				});
				end(first.audio);
				await release();
				expect(flush).not.toHaveBeenCalled();
				expect(currentTrack(session.snapshot().queue)?.id).toBe(boundary === 'selection' ? 'c' : undefined);
				expect(session.snapshot().queueFeedback).toBe('');
			});
		}
	});

	describe('natural end', () => {
		it('advances to the next track, restarts it from zero and announces it', async () => {
			const { session, release } = setup([A, B]);
			const first = mount(session, A);
			await startPlaying(session, first.audio);
			end(first.audio);
			expect(currentTrack(session.snapshot().queue)?.id).toBe('b');
			expect(session.snapshot().audio).toBeNull();
			const second = mount(session, B);
			second.audio.currentTime = 17;
			await release();
			expect(second.audio.currentTime).toBe(0);
			expect(second.audio.play).toHaveBeenCalledOnce();
			expect(session.snapshot().announcement).toBe('Playing Track B.');
			expect(session.snapshot().announcementVersion).toBe(1);
		});

		it('stops at the end of the queue when repeat is off', async () => {
			const { session, release } = setup([A]);
			const { audio } = mount(session, A);
			await startPlaying(session, audio);
			audio.play.mockClear();
			end(audio);
			await release();
			expect(audio.play).not.toHaveBeenCalled();
			expect(currentTrack(session.snapshot().queue)?.id).toBe('a');
			expect(session.snapshot().announcement).toBe('');
		});

		it('repeats one track in place without replacing the element', async () => {
			const { session, release } = setup([A, B], { [REPEAT_KEY]: 'one' });
			const { audio } = mount(session, A);
			await startPlaying(session, audio);
			audio.play.mockClear();
			end(audio);
			await release();
			expect(currentTrack(session.snapshot().queue)?.id).toBe('a');
			expect(session.snapshot().audio).toBe(audio.element);
			expect(audio.currentTime).toBe(0);
			expect(audio.play).toHaveBeenCalledOnce();
			expect(session.snapshot().announcement).toBe('Playing Track A.');
		});

		it('restarts a same-source replay even if duration is temporarily unknown', async () => {
			const { session, release } = setup([A], { [REPEAT_KEY]: 'one' });
			const { audio } = mount(session, A);
			await startPlaying(session, audio);
			end(audio);
			audio.currentTime = 37;
			audio.duration = NaN;
			await release();
			expect(audio.currentTime).toBe(0);
			expect(audio.play).toHaveBeenCalledTimes(2);
			expect(session.snapshot().announcement).toBe('Playing Track A.');
		});

		it('does not play or announce a replay when the start seek is refused', async () => {
			const { session, release } = setup([A], { [REPEAT_KEY]: 'one' });
			const { audio } = mount(session, A);
			await startPlaying(session, audio);
			end(audio);
			Object.defineProperty(audio, 'currentTime', {
				get: () => 37, set: () => { throw new Error('not seekable'); }
			});
			await release();
			expect(audio.play).toHaveBeenCalledOnce();
			expect(audio.paused).toBe(true);
			expect(session.snapshot().announcement).toBe('');
		});

		for (const repeat of ['one', 'queue'] as const) {
			for (const outcome of ['throw', 'sticky', 'nan', 'infinity', 'negative-infinity', 'success', 'unknown-duration'] as const) {
				it(`natural repeat ${repeat} resets jump at assignment, not outcome: ${outcome}`, async () => {
					const { session, release } = setup([A], { [REPEAT_KEY]: repeat });
					const { audio } = mount(session, A);
					Object.defineProperty(audio, 'seekable', {
						value: { length: 1, start: () => 0, end: () => 100 }
					});
					audio.currentTime = 30;
					session.jumpRelative('forward');
					const before = session.snapshot();
					expect(before.jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
					await startPlaying(session, audio);
					end(audio);
					let position = 100;
					let assigned = false;
					const writes: number[] = [];
					Object.defineProperty(audio, 'currentTime', {
						get: () => {
							if (assigned) expect(session.snapshot().jumpStatus).toBe('');
							return position;
						},
						set: (value: number) => {
							writes.push(value);
							expect(session.snapshot().jumpStatus).toBe(before.jumpStatus);
							if (outcome === 'throw') throw new Error('refused');
							if (outcome === 'success' || outcome === 'unknown-duration') position = value;
							if (outcome === 'nan') position = NaN;
							if (outcome === 'infinity') position = Infinity;
							if (outcome === 'negative-infinity') position = -Infinity;
							assigned = true;
						}
					});
					if (outcome === 'unknown-duration') audio.duration = NaN;
					await release();
					expect(writes).toEqual([0]);
					expect(session.snapshot().jumpStatus).toBe(outcome === 'throw' ? before.jumpStatus : '');
					expect(session.snapshot().jumpStatusVersion).toBe(before.jumpStatusVersion);
					expect(session.snapshot().audio).toBe(audio.element);
					if (repeat === 'one') expect(session.snapshot().queue).toBe(before.queue);
					else expect(session.snapshot().queue).toEqual(before.queue);
					const accepted = outcome === 'success' || outcome === 'unknown-duration';
					expect(audio.play).toHaveBeenCalledTimes(accepted ? 2 : 1);
					expect(session.snapshot().announcement).toBe(accepted ? 'Playing Track A.' : '');
					session.destroy();
				});
			}

			it(`a jump to duration does not trigger natural repeat ${repeat}`, async () => {
				const { session, release } = setup([A], { [REPEAT_KEY]: repeat });
				const { audio } = mount(session, A);
				Object.defineProperty(audio, 'seekable', {
					value: { length: 1, start: () => 0, end: () => 100 }
				});
				await startPlaying(session, audio);
				audio.currentTime = 95;
				session.jumpRelative('forward');
				audio.emit('seeking');
				audio.emit('seeked');
				end(audio);
				await release();
				expect(audio.currentTime).toBe(100);
				expect(audio.play).toHaveBeenCalledOnce();
				expect(session.snapshot().jumpStatus).toBe('Forward to the end, 1:40.');
				expect(session.snapshot().announcement).toBe('');
				session.destroy();
			});
		}

		it('wraps to the first track when repeating the queue', async () => {
			const { session, release } = setup([A, B], { [REPEAT_KEY]: 'queue' });
			session.setQueue({ ...session.snapshot().queue, currentIndex: 1 });
			const last = mount(session, B);
			await startPlaying(session, last.audio);
			end(last.audio);
			expect(currentTrack(session.snapshot().queue)?.id).toBe('a');
			const first = mount(session, A);
			await release();
			expect(first.audio.play).toHaveBeenCalledOnce();
		});

		it('does not start a continuation that is overtaken by an explicit pause', async () => {
			const { session, release } = setup([A, B]);
			const first = mount(session, A);
			await startPlaying(session, first.audio);
			end(first.audio);
			const second = mount(session, B);
			session.pause();
			await release();
			expect(second.audio.play).not.toHaveBeenCalled();
			expect(session.snapshot().announcement).toBe('');
		});

		it('does not start a continuation that is overtaken by an auth reset', async () => {
			const { session, release } = setup([A, B]);
			const first = mount(session, A);
			await startPlaying(session, first.audio);
			end(first.audio);
			const second = mount(session, B);
			session.resetAtAuthBoundary();
			// A new account selects the very same track and mounts its element.
			session.setQueue(createQueue([B]));
			const third = mount(session, B);
			await release();
			expect(second.audio.play).not.toHaveBeenCalled();
			expect(third.audio.play).not.toHaveBeenCalled();
			expect(third.audio.currentTime).toBe(0);
			expect(session.snapshot().announcement).toBe('');
		});

		it('does not start a continuation that is overtaken by a new selection', async () => {
			const { session, release } = setup([A, B, C]);
			const first = mount(session, A);
			await startPlaying(session, first.audio);
			end(first.audio);
			const second = mount(session, B);
			session.setQueue({ ...session.snapshot().queue, currentIndex: 2 });
			const third = mount(session, C);
			await release();
			expect(second.audio.play).not.toHaveBeenCalled();
			expect(third.audio.play).not.toHaveBeenCalled();
			expect(session.snapshot().announcement).toBe('');
		});

		it('does not continue when the session is destroyed while waiting', async () => {
			const { session, release } = setup([A, B]);
			const first = mount(session, A);
			await startPlaying(session, first.audio);
			end(first.audio);
			const second = mount(session, B);
			session.destroy();
			await release();
			expect(second.audio.play).not.toHaveBeenCalled();
			expect(session.snapshot().announcement).toBe('');
		});
	});

	describe('system Next', () => {
		function stubMediaSession() {
			const handlers = new Map<string, (() => void) | null>();
			vi.stubGlobal('navigator', {
				mediaSession: {
					metadata: null,
					setActionHandler: (action: string, handler: (() => void) | null) => { handlers.set(action, handler); },
					setPositionState: () => {}
				}
			});
			return handlers;
		}

		it('clamps at the last track even with repeat on, while natural end wraps', async () => {
			const handlers = stubMediaSession();
			const { session, release } = setup([A, B], { [REPEAT_KEY]: 'queue' });
			const first = mount(session, A);
			handlers.get('nexttrack')?.();
			expect(currentTrack(session.snapshot().queue)?.id).toBe('b');
			expect(first.audio.pause).toHaveBeenCalled();

			const last = mount(session, B);
			const queue = session.snapshot().queue;
			handlers.get('nexttrack')?.();
			expect(session.snapshot().queue).toBe(queue);
			expect(session.snapshot().audio).toBe(last.audio.element);
			expect(last.audio.pause).not.toHaveBeenCalled();

			await release();
			await startPlaying(session, last.audio);
			end(last.audio);
			expect(currentTrack(session.snapshot().queue)?.id).toBe('a');
			mount(session, A);
			await release();
		});

		it('clamps Previous at the first track', () => {
			const handlers = stubMediaSession();
			const { session } = setup([A, B]);
			const { audio } = mount(session, A);
			const queue = session.snapshot().queue;
			handlers.get('previoustrack')?.();
			expect(session.snapshot().queue).toBe(queue);
			expect(audio.pause).not.toHaveBeenCalled();
		});
	});

	it('saves the resume position on destroy, then drops all state', async () => {
		const { session, values } = setup();
		const { audio } = mount(session, A);
		await startPlaying(session, audio);
		audio.currentTime = 42;
		session.destroy();
		expect(JSON.parse(values.get(RESUME_STORAGE_KEY)!)).toMatchObject({ a: 42 });
		expect(audio.pause).toHaveBeenCalled();
		expect(session.snapshot()).toMatchObject({
			audio: null, transport: null, announcement: '', queue: { tracks: [], currentIndex: -1 }
		});
		// The retired element and torn-down transport are inert afterwards.
		audio.emit('playing');
		await session.play();
		session.tick();
		expect(session.snapshot().transport).toBeNull();
		expect(session.snapshot().audio).toBeNull();
	});

	it('can start again after destroy using fresh storage', () => {
		const { session } = setup();
		session.destroy();
		const fresh = makeStorage({ [REPEAT_KEY]: 'queue' });
		session.start(fresh.storage);
		expect(session.snapshot().repeat).toBe('queue');
	});
});
