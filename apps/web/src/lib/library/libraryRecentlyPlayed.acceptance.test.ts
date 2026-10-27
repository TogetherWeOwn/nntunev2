import { describe, expect, it } from 'vitest';
import {
	createRecentlyPlayedStore,
	formatRecentPlayedAt,
	MAX_RECENTLY_PLAYED,
	RECENTLY_PLAYED_STORAGE_KEY
} from '../player/recentlyPlayed.js';
import { MAX_OPAQUE_ID_CHARS, MAX_SERIALIZED_CHARS } from '../player/localStateBounds.js';

/**
 * LR acceptance, unit half: session-local library recently-played semantics.
 *
 * OWNERSHIP: Test-Author owned. The paired Builder wires the library route
 * to satisfy this file plus the browser half
 * (`tests/library-recently-played.spec.ts`) and must not edit either; a
 * wrong test comes back to the acceptance card instead.
 *
 * GREEN MECHANISM: these pins reuse the SHIPPED demo store
 * (`../player/recentlyPlayed.ts`) with the locked library key below, so
 * every case passes on base. The RED half is the browser spec: the library
 * route renders no recents section and records nothing until the build
 * lands. The Builder may reuse the shipped store with this key or ship a
 * compatible reader/writer; either way the serialized shape, cap, bounds
 * and fail-soft behavior pinned here hold bit-for-bit.
 *
 * LOCKED CONTRACT:
 * - Storage key `nntune:library-recently-played:v1` in sessionStorage only
 *   (never localStorage, never the demo key). The browser half asserts the
 *   storage area; this half pins the exact string and its isolation.
 * - Serialized shape: a JSON array of `{ trackId, playedAt }`,
 *   most-recent-first, e.g.
 *   `[{"trackId":"row-alpha","playedAt":1729600000000}]` where `playedAt`
 *   is a unix-epoch-milliseconds timestamp stored verbatim (no rounding).
 * - `MAX_RECENTLY_PLAYED = 10` distinct tracks; the 11th evicts the oldest.
 * - Record moves an existing id to the front with the new timestamp (never
 *   a duplicate). Replay moves an existing entry to the front with a
 *   refreshed `now()` timestamp and never invents. Forget removes exactly
 *   the matching id (exact stable-ID match: unknown, blank and
 *   whitespace-padded ids are a no-op that never touches storage) and a
 *   later actual playback may re-add it. Clear empties and persists `[]`.
 * - Bounded decode (see `../player/localStateBounds.ts`): raw payloads
 *   over `MAX_SERIALIZED_CHARS` are refused without parsing; ids over
 *   `MAX_OPAQUE_ID_CHARS` code units are ignored on write and absent on
 *   read. All storage access is fail-soft: throwing, denied or corrupt
 *   stores degrade to empty, never throw, and never roll back the visit's
 *   known-good list once read.
 * - `formatRecentPlayedAt` thresholds are shared with the demo list (under
 *   60s `'just now'`, under 60min `'<N> min ago'`, under 24h `'<N> hr ago'`,
 *   otherwise `'<N> d ago'`).
 *
 * CASE INDEX (stable IDs; LR-B-* live in the browser spec):
 * LR-U-01 library key pin, LR-U-02 cap pin, LR-U-03 record shape/order,
 * LR-U-04 default playedAt, LR-U-05 most-recent-first, LR-U-06 re-record
 * dedupe, LR-U-07 cap eviction, LR-U-08 replay to front, LR-U-09 replay
 * unknown no-op, LR-U-10 blank id ignored, LR-U-11 bad playedAt ignored,
 * LR-U-12 forget middle, LR-U-13 forget exact-match no-ops, LR-U-14 forget
 * last then clear, LR-U-15 re-record after forget, LR-U-16 denied storage
 * retains visit, LR-U-17 write-only denial, LR-U-18 cap after forget,
 * LR-U-19 clear, LR-U-20 clear empty, LR-U-21 entries snapshot, LR-U-22
 * serialized shape + demo isolation, LR-U-23 reload round-trip, LR-U-24
 * corrupt fallback, LR-U-25 malformed skipped, LR-U-26 load dedupe,
 * LR-U-27 load cap, LR-U-28 throwing storage, LR-U-29 oversized refused,
 * LR-U-30 oversized id, LR-U-31 relative labels, LR-U-32 demo key untouched.
 */

/** Locked library-namespaced session key (the browser half asserts the area). */
const LIBRARY_KEY = 'nntune:library-recently-played:v1';

type FakeStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> & { raw: Map<string, string> };

function fakeStorage(initial: Record<string, string> = {}): FakeStorage {
	const map = new Map(Object.entries(initial));
	return {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => void map.set(key, value),
		removeItem: (key: string) => void map.delete(key),
		raw: map
	};
}

function throwingStorage(): Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> {
	return {
		getItem: (_key: string): string | null => {
			throw new Error('storage unavailable');
		},
		setItem: (_key: string, _value: string): void => {
			throw new Error('storage unavailable');
		},
		removeItem: (_key: string): void => {
			throw new Error('storage unavailable');
		}
	};
}

function libraryStore(
	storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>,
	now?: () => number
) {
	return createRecentlyPlayedStore({ storage, storageKey: LIBRARY_KEY, now });
}

describe('library recently-played contract pins', () => {
	it('LR-U-01 pins the library-namespaced session key, distinct from demo', () => {
		expect(LIBRARY_KEY).toBe('nntune:library-recently-played:v1');
		expect(LIBRARY_KEY).toContain('library');
		expect(LIBRARY_KEY).not.toBe(RECENTLY_PLAYED_STORAGE_KEY);
		expect(RECENTLY_PLAYED_STORAGE_KEY).toBe('nntune:demo-recently-played:v1');
	});

	it('LR-U-02 pins the distinct-track cap at 10', () => {
		expect(MAX_RECENTLY_PLAYED).toBe(10);
	});
});

describe('library recently-played record', () => {
	it('LR-U-03 record stores {trackId, playedAt} verbatim at the front', () => {
		const store = libraryStore(fakeStorage());

		store.record('row-alpha', 1_729_600_000_123);

		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 1_729_600_000_123 }]);
	});

	it('LR-U-04 record defaults playedAt to the injected now', () => {
		const store = libraryStore(fakeStorage(), () => 5_000);

		store.record('row-alpha');

		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 5_000 }]);
	});

	it('LR-U-05 entries list most-recent-first', () => {
		const store = libraryStore(fakeStorage());

		store.record('row-alpha', 100);
		store.record('row-mike', 200);
		store.record('row-zulu', 300);

		expect(store.entries().map((entry) => entry.trackId)).toEqual([
			'row-zulu',
			'row-mike',
			'row-alpha'
		]);
	});

	it('LR-U-06 re-recording moves the track to the front without duplicating', () => {
		const store = libraryStore(fakeStorage());
		store.record('row-alpha', 100);
		store.record('row-mike', 200);

		store.record('row-alpha', 300);

		expect(store.entries()).toEqual([
			{ trackId: 'row-alpha', playedAt: 300 },
			{ trackId: 'row-mike', playedAt: 200 }
		]);
	});

	it('LR-U-07 the 11th distinct track evicts the oldest, keeping 10', () => {
		const store = libraryStore(fakeStorage());

		for (let i = 1; i <= 11; i += 1) store.record(`row-${i}`, i);

		const entries = store.entries();
		expect(entries).toHaveLength(10);
		expect(entries[0]).toEqual({ trackId: 'row-11', playedAt: 11 });
		expect(entries[9]).toEqual({ trackId: 'row-2', playedAt: 2 });
		expect(entries.some((entry) => entry.trackId === 'row-1')).toBe(false);
	});
});

describe('library recently-played replay', () => {
	it('LR-U-08 replay moves the entry to the front with a refreshed timestamp', () => {
		let now = 100;
		const store = libraryStore(fakeStorage(), () => now);
		store.record('row-alpha', 100);
		store.record('row-mike', 200);

		now = 999;
		store.replay('row-alpha');

		expect(store.entries()).toEqual([
			{ trackId: 'row-alpha', playedAt: 999 },
			{ trackId: 'row-mike', playedAt: 200 }
		]);
	});

	it('LR-U-09 replay of an unknown id invents nothing', () => {
		const storage = fakeStorage();
		const store = libraryStore(storage);
		store.record('row-alpha', 100);

		expect(() => store.replay('never-played')).not.toThrow();
		expect(() => store.replay('')).not.toThrow();

		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);
		expect(JSON.parse(storage.getItem(LIBRARY_KEY) ?? '')).toHaveLength(1);
	});
});

describe('library recently-played invalid input', () => {
	it.each([[''], ['   ']])('LR-U-10 record ignores blank id %s without writing', (id) => {
		const storage = fakeStorage();
		const store = libraryStore(storage);

		expect(() => store.record(id, 100)).not.toThrow();

		expect(store.entries()).toEqual([]);
		expect(storage.getItem(LIBRARY_KEY)).toBeNull();
	});

	it.each([[Number.NaN], [Number.POSITIVE_INFINITY], [Number.NEGATIVE_INFINITY], [-1]])(
		'LR-U-11 record ignores playedAt %s without writing',
		(playedAt) => {
			const storage = fakeStorage();
			const store = libraryStore(storage);

			expect(() => store.record('row-alpha', playedAt)).not.toThrow();

			expect(store.entries()).toEqual([]);
			expect(storage.getItem(LIBRARY_KEY)).toBeNull();
		}
	);
});

describe('library recently-played forget', () => {
	const SEED = [
		{ trackId: 'row-zulu', playedAt: 300 },
		{ trackId: 'row-mike', playedAt: 200 },
		{ trackId: 'row-alpha', playedAt: 100 }
	];

	function seeded() {
		const storage = fakeStorage({ [LIBRARY_KEY]: JSON.stringify(SEED) });
		const store = libraryStore(storage, () => 999);
		expect(store.entries()).toEqual(SEED);
		return { storage, store };
	}

	it('LR-U-12 forget middle preserves remaining order, timestamps and schema on reopen', () => {
		const { storage, store } = seeded();

		store.forget('row-mike');

		const expected = [
			{ trackId: 'row-zulu', playedAt: 300 },
			{ trackId: 'row-alpha', playedAt: 100 }
		];
		expect(store.entries()).toEqual(expected);
		expect(JSON.parse(storage.getItem(LIBRARY_KEY) ?? '')).toEqual(expected);
		expect(libraryStore(storage).entries()).toEqual(expected);
	});

	it('LR-U-13 stale, blank and whitespace-padded forget never touches a different entry', () => {
		const { storage, store } = seeded();
		store.forget('row-mike');
		const raw = storage.getItem(LIBRARY_KEY);

		for (const id of ['row-mike', 'missing-id', '', '   ', ' row-zulu ']) {
			expect(() => store.forget(id)).not.toThrow();
		}

		expect(store.entries()).toEqual([
			{ trackId: 'row-zulu', playedAt: 300 },
			{ trackId: 'row-alpha', playedAt: 100 }
		]);
		expect(storage.getItem(LIBRARY_KEY)).toBe(raw);
	});

	it('LR-U-14 forget last, then clear, then forget empty remains empty', () => {
		const { storage, store } = seeded();

		store.forget('row-alpha');
		expect(store.entries()).toEqual([
			{ trackId: 'row-zulu', playedAt: 300 },
			{ trackId: 'row-mike', playedAt: 200 }
		]);
		store.clear();
		store.forget('row-zulu');

		expect(store.entries()).toEqual([]);
		expect(storage.getItem(LIBRARY_KEY)).toBe('[]');
	});

	it('LR-U-15 actual playback can record a forgotten id again without resurrecting others', () => {
		const { store } = seeded();

		store.forget('row-mike');
		store.record('row-mike', 400);

		expect(store.entries()).toEqual([
			{ trackId: 'row-mike', playedAt: 400 },
			{ trackId: 'row-zulu', playedAt: 300 },
			{ trackId: 'row-alpha', playedAt: 100 }
		]);
	});

	it('LR-U-16 failed storage never rolls back this visit or resurrects a prior forget', () => {
		const values = new Map<string, string>([[LIBRARY_KEY, JSON.stringify(SEED)]]);
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
		const store = libraryStore(storage);
		// Read the visit's baseline before storage becomes unavailable.
		expect(store.entries()).toEqual(SEED);
		blocked = true;

		expect(() => store.forget('row-mike')).not.toThrow();
		expect(store.entries()).toEqual([
			{ trackId: 'row-zulu', playedAt: 300 },
			{ trackId: 'row-alpha', playedAt: 100 }
		]);
		store.forget('row-zulu');
		store.record('row-mike', 400);
		expect(store.entries()).toEqual([
			{ trackId: 'row-mike', playedAt: 400 },
			{ trackId: 'row-alpha', playedAt: 100 }
		]);
		store.clear();
		expect(store.entries()).toEqual([]);
	});

	it('LR-U-17 write-only denial retains the current result instead of rereading stale bytes', () => {
		const storage = fakeStorage({ [LIBRARY_KEY]: JSON.stringify(SEED) });
		const store = createRecentlyPlayedStore({
			storage: {
				...storage,
				setItem: () => {
					throw new Error('synthetic quota exceeded');
				}
			},
			storageKey: LIBRARY_KEY
		});

		expect(store.entries()).toEqual(SEED);
		store.forget('row-mike');
		expect(store.entries()).toEqual([
			{ trackId: 'row-zulu', playedAt: 300 },
			{ trackId: 'row-alpha', playedAt: 100 }
		]);
		// The failed write really did leave stale storage behind.
		expect(JSON.parse(storage.getItem(LIBRARY_KEY) ?? '')).toEqual(SEED);
		store.forget('row-zulu');
		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);
	});

	it('LR-U-18 cap and distinct-id semantics survive forget followed by new plays', () => {
		const { store } = seeded();

		store.forget('row-mike');
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

describe('library recently-played clear', () => {
	it('LR-U-19 clear empties the list and persists []', () => {
		const storage = fakeStorage();
		const store = libraryStore(storage);
		store.record('row-alpha', 100);

		store.clear();

		expect(store.entries()).toEqual([]);
		expect(storage.getItem(LIBRARY_KEY)).toBe('[]');
	});

	it('LR-U-20 clear on an empty store does not throw', () => {
		const store = libraryStore(fakeStorage());

		expect(() => store.clear()).not.toThrow();
		expect(store.entries()).toEqual([]);
	});
});

describe('library recently-played persistence', () => {
	it('LR-U-21 entries() returns a snapshot that cannot corrupt the store', () => {
		const store = libraryStore(fakeStorage());
		store.record('row-alpha', 100);

		const snapshot = store.entries();
		snapshot.length = 0;
		snapshot.push({ trackId: 'forged', playedAt: 1 });

		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);
	});

	it('LR-U-22 serializes as a JSON array of {trackId, playedAt} under the library key', () => {
		const storage = fakeStorage();
		const store = libraryStore(storage);

		store.record('row-alpha', 100);
		store.record('row-mike', 200);

		expect(storage.getItem(LIBRARY_KEY)).toBe(
			'[{"trackId":"row-mike","playedAt":200},{"trackId":"row-alpha","playedAt":100}]'
		);
		expect(storage.getItem(RECENTLY_PLAYED_STORAGE_KEY)).toBeNull();
	});

	it('LR-U-23 a new instance over the same store reads entries (reload persistence)', () => {
		const storage = fakeStorage();
		libraryStore(storage).record('row-alpha', 100);

		expect(libraryStore(storage).entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);
	});

	it.each([['{oops'], ['42'], ['null'], ['"str"'], ['{"trackId":"x"}']])(
		'LR-U-24 corrupt/non-array JSON %s falls back to empty and recovers on record',
		(raw) => {
			const storage = fakeStorage({ [LIBRARY_KEY]: raw });
			const store = libraryStore(storage);

			expect(store.entries()).toEqual([]);

			store.record('row-alpha', 100);
			expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);
		}
	);

	it('LR-U-25 malformed entries inside the array are skipped', () => {
		const storage = fakeStorage({
			[LIBRARY_KEY]: JSON.stringify([
				{ trackId: 'row-alpha', playedAt: 100 },
				null,
				'nope',
				42,
				{ trackId: '', playedAt: 200 },
				{ trackId: 'no-time' },
				{ playedAt: 300 },
				{ trackId: 'bad-time', playedAt: 'soon' },
				{ trackId: 'negative-time', playedAt: -5 },
				{ trackId: 'row-mike', playedAt: 50 }
			])
		});

		expect(libraryStore(storage).entries()).toEqual([
			{ trackId: 'row-alpha', playedAt: 100 },
			{ trackId: 'row-mike', playedAt: 50 }
		]);
	});

	it('LR-U-26 repeated ids on load dedupe keeping the first (most recent)', () => {
		const storage = fakeStorage({
			[LIBRARY_KEY]: JSON.stringify([
				{ trackId: 'row-alpha', playedAt: 300 },
				{ trackId: 'row-mike', playedAt: 200 },
				{ trackId: 'row-alpha', playedAt: 100 }
			])
		});

		expect(libraryStore(storage).entries()).toEqual([
			{ trackId: 'row-alpha', playedAt: 300 },
			{ trackId: 'row-mike', playedAt: 200 }
		]);
	});

	it('LR-U-27 loads cap at 10 keeping the front', () => {
		const seeded = Array.from({ length: 12 }, (_, i) => ({
			trackId: `row-${i + 1}`,
			playedAt: 100 + i
		}));
		const storage = fakeStorage({ [LIBRARY_KEY]: JSON.stringify(seeded) });

		const entries = libraryStore(storage).entries();
		expect(entries).toHaveLength(10);
		expect(entries[0]?.trackId).toBe('row-1');
		expect(entries[9]?.trackId).toBe('row-10');
	});

	it('LR-U-28 a throwing store degrades to empty with contained writes', () => {
		const store = libraryStore(throwingStorage());

		expect(store.entries()).toEqual([]);
		expect(() => store.record('row-alpha', 100)).not.toThrow();
		expect(() => store.replay('row-alpha')).not.toThrow();
		expect(() => store.forget('row-alpha')).not.toThrow();
		expect(() => store.clear()).not.toThrow();
		expect(store.entries()).toEqual([]);
	});

	it('LR-U-29 an oversized payload is refused without parsing and recovers on record', () => {
		const valid = JSON.stringify([{ trackId: 'row-alpha', playedAt: 100 }]);
		// Trailing whitespace keeps the JSON valid, so an empty read proves
		// the byte bound refused before parse rather than a syntax fallback.
		const oversized = valid + ' '.repeat(MAX_SERIALIZED_CHARS + 1 - valid.length);
		expect(oversized.length).toBeGreaterThan(MAX_SERIALIZED_CHARS);
		const storage = fakeStorage({ [LIBRARY_KEY]: oversized });
		const store = libraryStore(storage);

		expect(store.entries()).toEqual([]);

		store.record('row-alpha', 100);
		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);
	});

	it('LR-U-30 over-long ids are ignored on write and absent on read; the bound itself is kept', () => {
		const longId = 'i'.repeat(MAX_OPAQUE_ID_CHARS + 1);
		const maxId = 'i'.repeat(MAX_OPAQUE_ID_CHARS);
		const storage = fakeStorage({
			[LIBRARY_KEY]: JSON.stringify([
				{ trackId: longId, playedAt: 50 },
				{ trackId: 'row-alpha', playedAt: 100 }
			])
		});
		const store = libraryStore(storage);

		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);

		expect(() => store.record(longId, 200)).not.toThrow();
		expect(store.entries()).toEqual([{ trackId: 'row-alpha', playedAt: 100 }]);

		store.record(maxId, 300);
		expect(store.entries()[0]).toEqual({ trackId: maxId, playedAt: 300 });
	});
});

describe('library recently-played relative labels', () => {
	it.each([
		['LR-U-31a just now', 0, 59_999, 'just now'],
		['LR-U-31b future clamps to just now', 120_000, 60_000, 'just now'],
		['LR-U-31c minutes', 0, 5 * 60_000, '5 min ago'],
		['LR-U-31d hours', 0, 3 * 3_600_000, '3 hr ago'],
		['LR-U-31e days', 0, 2 * 86_400_000, '2 d ago']
	])('%s', (_label, playedAt, now, expected) => {
		expect(formatRecentPlayedAt(playedAt, now)).toBe(expected);
	});
});

describe('library/demo history isolation', () => {
	it('LR-U-32 library plays never touch the demo key', () => {
		const storage = fakeStorage({
			[RECENTLY_PLAYED_STORAGE_KEY]: JSON.stringify([{ trackId: 'demo-track', playedAt: 7 }])
		});
		const store = libraryStore(storage);

		store.record('row-alpha', 100);
		store.replay('row-alpha');
		store.forget('missing-id');
		store.clear();
		store.record('row-mike', 200);

		expect(storage.getItem(RECENTLY_PLAYED_STORAGE_KEY)).toBe(
			'[{"trackId":"demo-track","playedAt":7}]'
		);
		expect(JSON.parse(storage.getItem(LIBRARY_KEY) ?? '')).toEqual([
			{ trackId: 'row-mike', playedAt: 200 }
		]);
	});
});
