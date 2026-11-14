import { afterEach, describe, expect, it, vi } from 'vitest';
import { createQueueShare } from './queueShare.js';
import { buildShareQuery } from './shareTimestamp.js';
import type { QueueState, Track } from './queue.js';

const ORIGIN = 'https://nntune.example';

const t = (id: string, title: string = `Title ${id}`, streamUrl = `/${id}.mp3`): Track => ({
	id,
	title,
	artist: 'Artist',
	streamUrl
});

interface Deferred {
	promise: Promise<void>;
	resolve: () => void;
	reject: (e?: unknown) => void;
}
function deferred(): Deferred {
	let resolve!: () => void;
	let reject!: (e?: unknown) => void;
	const promise = new Promise<void>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

type Outcome = [string, string | null];

interface Harness {
	share: ReturnType<typeof createQueueShare>;
	outcomes: Outcome[];
	writes: string[];
	queue: { value: QueueState };
	audio: { currentTime: number };
	last: () => Outcome;
}

function harness(
	opts: {
		queue?: QueueState;
		currentTime?: number;
		audio?: boolean;
		origin?: () => string;
		clipboard?: () => { writeText: (text: string) => Promise<void> } | undefined;
	} = {}
): Harness {
	const queue = { value: opts.queue ?? { tracks: [t('a'), t('b')], currentIndex: 0 } };
	const audio = { currentTime: opts.currentTime ?? 0 };
	const outcomes: Outcome[] = [];
	const writes: string[] = [];
	const share = createQueueShare({
		getQueue: () => queue.value,
		getAudio: () => (opts.audio === false ? null : (audio as unknown as HTMLAudioElement)),
		getOrigin: opts.origin ?? (() => ORIGIN),
		getClipboard:
			opts.clipboard ??
			(() => ({
				writeText: async (text: string) => {
					writes.push(text);
				}
			})),
		onOutcome: (status, url) => {
			outcomes.push([status, url]);
		}
	});
	return { share, outcomes, writes, queue, audio, last: () => outcomes[outcomes.length - 1]! };
}

const url = (id: string, seconds: number) => `${ORIGIN}/library?${buildShareQuery(id, seconds)}`;

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe('createQueueShare copy', () => {
	it('publishes an empty outcome first, then copies and reports success', async () => {
		const h = harness();
		await h.share.copy('b');
		expect(h.outcomes[0]).toEqual(['', null]);
		expect(h.last()).toEqual(['Copied link for Title b.', null]);
		expect(h.writes).toEqual([url('b', 0)]);
	});

	it('uses the live floored currentTime for the current playable track', async () => {
		const h = harness({ currentTime: 83.9 });
		await h.share.copy('a');
		expect(h.writes).toEqual([`${ORIGIN}/library?track=a&t=83`]);
		h.audio.currentTime = 120.2;
		await h.share.copy('a');
		expect(h.writes[1]).toBe(`${ORIGIN}/library?track=a&t=120`);
	});

	it('uses 0 for a non-current track even when audio has a position', async () => {
		const h = harness({ currentTime: 50 });
		await h.share.copy('b');
		expect(h.writes).toEqual([`${ORIGIN}/library?track=b&t=0`]);
	});

	it.each([
		['null audio', { audio: false, currentTime: 9 }],
		['NaN', { currentTime: Number.NaN }],
		['Infinity', { currentTime: Number.POSITIVE_INFINITY }],
		['negative', { currentTime: -3 }]
	])('uses 0 for the current track when audio is unusable (%s)', async (_n, o) => {
		const h = harness(o);
		await h.share.copy('a');
		expect(h.writes).toEqual([`${ORIGIN}/library?track=a&t=0`]);
	});

	it('uses 0 for a current track with no usable stream source', async () => {
		const h = harness({
			queue: { tracks: [t('a', 'A', '   '), t('b')], currentIndex: 0 },
			currentTime: 40
		});
		await h.share.copy('a');
		expect(h.writes).toEqual([`${ORIGIN}/library?track=a&t=0`]);
	});

	it('percent-encodes ids exactly like buildShareQuery', async () => {
		const id = 'a b/c&d=é';
		const h = harness({ queue: { tracks: [t(id, 'X')], currentIndex: 0 }, currentTime: 7.5 });
		await h.share.copy(id);
		expect(h.writes).toEqual([`${ORIGIN}/library?${buildShareQuery(id, 7.5)}`]);
		expect(h.writes[0]).toContain('track=a%20b%2Fc%26d%3D%C3%A9&t=7');
	});

	it('truncates titles over 70 code points to 69 plus an ellipsis', async () => {
		const long = '😀'.repeat(71);
		const exact = '😀'.repeat(70);
		const h = harness({ queue: { tracks: [t('a', long), t('b', exact)], currentIndex: 0 } });
		await h.share.copy('a');
		expect(h.last()[0]).toBe(`Copied link for ${'😀'.repeat(69)}….`);
		await h.share.copy('b');
		expect(h.last()[0]).toBe(`Copied link for ${exact}.`);
	});

	it('falls back to the id when the title is blank', async () => {
		const h = harness({ queue: { tracks: [t('a', '   ')], currentIndex: 0 } });
		await h.share.copy('a');
		expect(h.last()[0]).toBe('Copied link for a.');
	});

	it.each([['missing', 'zzz'], ['blank', '   '], ['empty', '']])(
		'reports unavailable without clipboard for a %s id',
		async (_n, id) => {
			const h = harness();
			await h.share.copy(id);
			expect(h.writes).toEqual([]);
			const [status, fallback] = h.last();
			expect(status).toMatch(/^Link unavailable — /);
			expect(fallback).toBeNull();
		}
	);

	it('reports unavailable without clipboard when the origin getter throws', async () => {
		const throwing = harness({
			origin: () => {
				throw new Error('no origin');
			}
		});
		await throwing.share.copy('a');
		expect(throwing.writes).toEqual([]);
		expect(throwing.last()).toEqual(['Link unavailable — Title a is no longer in the queue.', null]);
	});

	it('gives the manual fallback url when the clipboard is missing', async () => {
		const h = harness({ clipboard: () => undefined });
		await h.share.copy('b');
		expect(h.last()).toEqual(['', url('b', 0)]);
	});

	it('gives the manual fallback url when the clipboard getter throws', async () => {
		const h = harness({
			clipboard: () => {
				throw new Error('denied');
			}
		});
		await h.share.copy('a');
		expect(h.last()).toEqual(['', url('a', 0)]);
	});

	it('gives the manual fallback url when writeText rejects or throws synchronously', async () => {
		const rejecting = harness({ clipboard: () => ({ writeText: () => Promise.reject(new Error('x')) }) });
		await rejecting.share.copy('b');
		expect(rejecting.last()).toEqual(['', url('b', 0)]);

		const throwing = harness({
			clipboard: () => ({
				writeText: () => {
					throw new Error('sync');
				}
			})
		});
		await throwing.share.copy('b');
		expect(throwing.last()).toEqual(['', url('b', 0)]);
	});

	it('does not touch fetch, storage or media state', async () => {
		const fetchSpy = vi.fn();
		const storage = { getItem: vi.fn(), setItem: vi.fn(), removeItem: vi.fn(), clear: vi.fn() };
		vi.stubGlobal('fetch', fetchSpy);
		vi.stubGlobal('localStorage', storage);
		vi.stubGlobal('sessionStorage', storage);
		const play = vi.fn();
		const pause = vi.fn();
		const load = vi.fn();
		const media = {
			currentTime: 12,
			play,
			pause,
			load,
			src: 'x',
			playbackRate: 1,
			volume: 1
		};
		const writes: string[] = [];
		const queue: QueueState = { tracks: [t('a'), t('b')], currentIndex: 0 };
		const frozen = JSON.stringify(queue);
		const share = createQueueShare({
			getQueue: () => queue,
			getAudio: () => media as unknown as HTMLAudioElement,
			getOrigin: () => ORIGIN,
			getClipboard: () => ({ writeText: async (s) => void writes.push(s) }),
			onOutcome: () => {}
		});
		await share.copy('a');
		await share.copy('b');
		expect(fetchSpy).not.toHaveBeenCalled();
		for (const fn of Object.values(storage)) expect(fn).not.toHaveBeenCalled();
		expect(play).not.toHaveBeenCalled();
		expect(pause).not.toHaveBeenCalled();
		expect(load).not.toHaveBeenCalled();
		expect(media).toMatchObject({ currentTime: 12, src: 'x', playbackRate: 1, volume: 1 });
		expect(JSON.stringify(queue)).toBe(frozen);
		expect(writes).toHaveLength(2);
	});
});

describe('createQueueShare async races', () => {
	function pending() {
		const d = deferred();
		const h = harness({
			clipboard: () => ({ writeText: () => d.promise })
		});
		return { d, h };
	}

	it('reports unavailable when the row is removed while the write is pending', async () => {
		const { d, h } = pending();
		const p = h.share.copy('b');
		h.queue.value = { tracks: [t('a')], currentIndex: 0 };
		d.resolve();
		await p;
		expect(h.last()).toEqual(['Link unavailable — Title b is no longer in the queue.', null]);
	});

	it('reports unavailable even when the pending write rejects and the row is gone', async () => {
		const { d, h } = pending();
		const p = h.share.copy('b');
		h.queue.value = { tracks: [t('a')], currentIndex: 0 };
		d.reject(new Error('nope'));
		await p;
		expect(h.last()).toEqual(['Link unavailable — Title b is no longer in the queue.', null]);
	});

	it('still completes when the queue is reordered or the selection changes', async () => {
		const { d, h } = pending();
		const p = h.share.copy('b');
		h.queue.value = { tracks: [t('b'), t('a')], currentIndex: 1 };
		d.resolve();
		await p;
		expect(h.last()).toEqual(['Copied link for Title b.', null]);

		const second = pending();
		const p2 = second.h.share.copy('b');
		second.h.queue.value = { tracks: [t('a'), t('b')], currentIndex: 1 };
		second.d.reject(new Error('late'));
		await p2;
		expect(second.h.last()).toEqual(['', url('b', 0)]);
	});

	it('reset invalidates a pending copy on resolve', async () => {
		const { d, h } = pending();
		const p = h.share.copy('b');
		const before = h.outcomes.length;
		h.share.reset();
		d.resolve();
		await p;
		expect(h.outcomes).toHaveLength(before);
	});

	it('reset invalidates a pending copy on reject', async () => {
		const { d, h } = pending();
		const p = h.share.copy('b');
		const before = h.outcomes.length;
		h.share.reset();
		d.reject(new Error('x'));
		await p;
		expect(h.outcomes).toHaveLength(before);
	});

	it('reset itself publishes nothing', () => {
		const h = harness();
		h.share.reset();
		expect(h.outcomes).toEqual([]);
	});

	it('latest copy wins whichever order the writes settle in', async () => {
		const first = deferred();
		const second = deferred();
		const queue = { tracks: [t('a'), t('b')], currentIndex: 0 };
		const outcomes: Outcome[] = [];
		const pendings = [first, second];
		const share = createQueueShare({
			getQueue: () => queue,
			getAudio: () => null,
			getOrigin: () => ORIGIN,
			getClipboard: () => ({ writeText: () => pendings.shift()!.promise }),
			onOutcome: (s, u) => void outcomes.push([s, u])
		});
		const p1 = share.copy('a');
		const p2 = share.copy('b');
		second.resolve();
		await p2;
		first.resolve();
		await p1;
		const after = outcomes.filter(([s]) => s.startsWith('Copied'));
		expect(after).toEqual([['Copied link for Title b.', null]]);
		expect(outcomes[outcomes.length - 1]).toEqual(['Copied link for Title b.', null]);

		// Old rejection after a newer success must not publish a fallback.
		const f = deferred();
		const s = deferred();
		const q = [f, s];
		const out2: Outcome[] = [];
		const share2 = createQueueShare({
			getQueue: () => queue,
			getAudio: () => null,
			getOrigin: () => ORIGIN,
			getClipboard: () => ({ writeText: () => q.shift()!.promise }),
			onOutcome: (st, u) => void out2.push([st, u])
		});
		const a = share2.copy('a');
		const b = share2.copy('b');
		s.resolve();
		await b;
		f.reject(new Error('late'));
		await a;
		expect(out2[out2.length - 1]).toEqual(['Copied link for Title b.', null]);
		expect(out2.some(([, u]) => u !== null)).toBe(false);
	});

	it('a new copy publishes an empty outcome that clears prior state before settling', async () => {
		const d = deferred();
		let calls = 0;
		const h = harness({
			clipboard: () => ({
				writeText: () => (calls++ === 0 ? Promise.resolve() : d.promise)
			})
		});
		await h.share.copy('a');
		const p = h.share.copy('b');
		expect(h.last()).toEqual(['', null]);
		d.resolve();
		await p;
		expect(h.last()).toEqual(['Copied link for Title b.', null]);
	});

	it('a copy started after reset still works', async () => {
		const { d, h } = pending();
		const stale = h.share.copy('a');
		h.share.reset();
		d.resolve();
		await stale;
		const h2 = harness();
		h2.share.reset();
		await h2.share.copy('a');
		expect(h2.last()[0]).toBe('Copied link for Title a.');
	});

	it('reset called from inside a getter prevents publishing previous metadata', async () => {
		for (const stage of ['queue', 'audio', 'origin', 'clipboard'] as const) {
			const outcomes: Outcome[] = [];
			const writes: string[] = [];
			let share!: ReturnType<typeof createQueueShare>;
			const maybeReset = (at: string) => {
				if (at === stage) share.reset();
			};
			share = createQueueShare({
				getQueue: () => {
					maybeReset('queue');
					return { tracks: [t('a', 'Old title')], currentIndex: 0 };
				},
				getAudio: () => {
					maybeReset('audio');
					return { currentTime: 5 } as unknown as HTMLAudioElement;
				},
				getOrigin: () => {
					maybeReset('origin');
					return ORIGIN;
				},
				getClipboard: () => {
					maybeReset('clipboard');
					return { writeText: async (s) => void writes.push(s) };
				},
				onOutcome: (s, u) => void outcomes.push([s, u])
			});
			await share.copy('a');
			// Only the initial empty publish is allowed; no title, url or status after reset.
			const leaked = outcomes.slice(1).filter(([s, u]) => s !== '' || u !== null);
			expect(leaked, `stage ${stage}`).toEqual([]);
			expect(JSON.stringify(outcomes), `stage ${stage}`).not.toContain('Old title');
		}
	});
});
