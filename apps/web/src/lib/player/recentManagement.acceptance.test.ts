import { describe, expect, it } from 'vitest';
import { createRecentlyPlayedStore, type RecentEntry } from './recentlyPlayed.js';
import { addTrack, currentTrack, type QueueState, type Track } from './queue.js';

/**
 * Acceptance-owned; the paired implementation must not edit this file.
 * New minimal public interface: the EXISTING recently-played store gains
 * `forget(trackId: string): void`. Remove that ID only, retain the current
 * visit's result even when persistence fails, and best-effort persist the
 * same array schema. No new module, result type, controller or transport API.
 * Queue cases exercise existing addTrack semantics, not a new wrapper.
 *
 * Breaks caught: removing the wrong row/by index, timestamp rewrites, stale
 * reads resurrecting a forgotten row, loss on storage error, duplicate queue
 * append, stealing current identity, or changing the input arrangement.
 * Missing forget is a descriptive assertion, never an import/TypeError RED.
 */
const KEY = 'nntune:demo-recently-played:v1';
const SEED: RecentEntry[] = [
	{ trackId: 'third-light', playedAt: 300 },
	{ trackId: 'second-signal', playedAt: 200 },
	{ trackId: 'demo-track', playedAt: 100 }
];

function storageFixture() {
	const values = new Map<string, string>([[KEY, JSON.stringify(SEED)]]);
	let blocked = false;
	const storage = {
		getItem(key: string) {
			if (blocked) throw new Error('synthetic storage denial');
			return values.get(key) ?? null;
		},
		setItem(key: string, value: string) {
			if (blocked) throw new Error('synthetic storage denial');
			values.set(key, value);
		},
		removeItem(key: string) {
			if (blocked) throw new Error('synthetic storage denial');
			values.delete(key);
		}
	};
	return { storage, values, block: () => { blocked = true; } };
}

function setup() {
	const fixture = storageFixture();
	const store = createRecentlyPlayedStore({ storage: fixture.storage, now: () => 999 });
	// Read the visit's current list before storage can become unavailable.
	expect(store.entries()).toEqual(SEED);
	return { ...fixture, store };
}

function forget(store: ReturnType<typeof createRecentlyPlayedStore>, id: string): void {
	const candidate = store as typeof store & { forget?: (trackId: string) => void };
	expect(candidate.forget, 'recent management: existing store supports per-ID forget').toBeTypeOf('function');
	candidate.forget!(id);
}

describe('recent entry removal', () => {
	it('RM-U-01 forget middle preserves remaining order, timestamps and schema on reopen', () => {
		const { store, storage } = setup();
		forget(store, 'second-signal');
		const expected = [
			{ trackId: 'third-light', playedAt: 300 },
			{ trackId: 'demo-track', playedAt: 100 }
		];
		expect(store.entries()).toEqual(expected);
		expect(JSON.parse(storage.getItem(KEY)!)).toEqual(expected);
		expect(createRecentlyPlayedStore({ storage }).entries()).toEqual(expected);
	});

	it('RM-U-02 stale, blank and whitespace forget never delete a different entry', () => {
		const { store, storage } = setup();
		forget(store, 'second-signal');
		const raw = storage.getItem(KEY);
		for (const id of ['second-signal', 'missing-id', '', '   ', ' third-light ']) {
			expect(() => forget(store, id)).not.toThrow();
		}
		expect(store.entries()).toEqual([
			{ trackId: 'third-light', playedAt: 300 },
			{ trackId: 'demo-track', playedAt: 100 }
		]);
		expect(storage.getItem(KEY)).toBe(raw);
	});

	it('RM-U-03 forget last, then clear, then forget empty remains empty', () => {
		const { store, storage } = setup();
		forget(store, 'demo-track');
		expect(store.entries()).toEqual([
			{ trackId: 'third-light', playedAt: 300 },
			{ trackId: 'second-signal', playedAt: 200 }
		]);
		store.clear();
		forget(store, 'third-light');
		expect(store.entries()).toEqual([]);
		expect(storage.getItem(KEY)).toBe('[]');
	});

	it('RM-U-04 actual playback can record a forgotten ID again without resurrecting others', () => {
		const { store } = setup();
		forget(store, 'second-signal');
		store.record('second-signal', 400);
		expect(store.entries()).toEqual([
			{ trackId: 'second-signal', playedAt: 400 },
			{ trackId: 'third-light', playedAt: 300 },
			{ trackId: 'demo-track', playedAt: 100 }
		]);
	});

	it('RM-U-05 failed storage never rolls back this visit or resurrects a prior forget', () => {
		const { store, block } = setup();
		block();
		expect(() => forget(store, 'second-signal')).not.toThrow();
		expect(store.entries()).toEqual([
			{ trackId: 'third-light', playedAt: 300 },
			{ trackId: 'demo-track', playedAt: 100 }
		]);
		forget(store, 'third-light');
		store.record('second-signal', 400);
		expect(store.entries()).toEqual([
			{ trackId: 'second-signal', playedAt: 400 },
			{ trackId: 'demo-track', playedAt: 100 }
		]);
		store.clear();
		expect(store.entries()).toEqual([]);
	});

	it('RM-U-06 write-only storage denial retains the current result instead of rereading stale bytes', () => {
		const { values, storage } = storageFixture();
		const store = createRecentlyPlayedStore({ storage: {
			...storage,
			setItem: () => { throw new Error('synthetic quota exceeded'); }
		} });
		expect(store.entries()).toEqual(SEED);
		forget(store, 'second-signal');
		expect(store.entries()).toEqual([
			{ trackId: 'third-light', playedAt: 300 },
			{ trackId: 'demo-track', playedAt: 100 }
		]);
		// The failed write really did leave stale storage behind.
		expect(JSON.parse(values.get(KEY)!)).toEqual(SEED);
		forget(store, 'third-light');
		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
	});

	it('RM-U-07 cap and distinct-ID semantics survive forget followed by new plays', () => {
		const { store } = setup();
		forget(store, 'second-signal');
		for (let i = 1; i <= 10; i += 1) store.record(`synthetic-${i}`, 400 + i);
		store.record('synthetic-5', 500);
		expect(store.entries()).toEqual([
			{ trackId: 'synthetic-5', playedAt: 500 },
			{ trackId: 'synthetic-10', playedAt: 410 },
			{ trackId: 'synthetic-9', playedAt: 409 },
			{ trackId: 'synthetic-8', playedAt: 408 },
			{ trackId: 'synthetic-7', playedAt: 407 },
			{ trackId: 'synthetic-6', playedAt: 406 },
			{ trackId: 'synthetic-4', playedAt: 404 },
			{ trackId: 'synthetic-3', playedAt: 403 },
			{ trackId: 'synthetic-2', playedAt: 402 },
			{ trackId: 'synthetic-1', playedAt: 401 }
		]);
	});
});

// Literal synthetic tracks: no live providers or remote audio.
const first: Track = { id: 'a', title: 'A', artist: 'Fixture', streamUrl: '/fixture/a' };
const second: Track = { id: 'b', title: 'B', artist: 'Fixture', streamUrl: '/fixture/b' };
const third: Track = { id: 'c', title: 'C', artist: 'Fixture', streamUrl: '/fixture/c' };

describe('existing queue append controls', () => {
	it('RM-U-08 append preserves a custom arrangement, current identity and the input', () => {
		const input: QueueState = { tracks: [third, first], currentIndex: 1 };
		const result = addTrack(input, second);
		expect(result.tracks.map((track) => track.id)).toEqual(['c', 'a', 'b']);
		expect(result.currentIndex).toBe(1);
		expect(currentTrack(result)?.id).toBe('a');
		expect(input).toEqual({ tracks: [third, first], currentIndex: 1 });
	});

	it('RM-U-09 duplicate append is idempotent, including after a first append', () => {
		const input: QueueState = { tracks: [third, first], currentIndex: 1 };
		const added = addTrack(input, second);
		expect(addTrack(added, second)).toEqual({ tracks: [third, first, second], currentIndex: 1 });
		expect(addTrack(input, { ...first, title: 'wrong replacement' })).toEqual(input);
	});

	it('RM-U-10 empty queue append follows existing first-selection semantics', () => {
		const result = addTrack({ tracks: [], currentIndex: -1 }, second);
		expect(result).toEqual({ tracks: [second], currentIndex: 0 });
	});
});
