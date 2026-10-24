import { describe, expect, it } from 'vitest';

/**
 * RED acceptance for the session-local demo recently-played list.
 *
 * OWNERSHIP: Test-Author owned. The paired Builder implements
 * `apps/web/src/lib/player/recentlyPlayed.ts` to satisfy this file and must
 * not edit it; a wrong test comes back to the acceptance card instead.
 *
 * RED MECHANISM: the module under test does not exist yet, so it is loaded
 * through a variable-specifier dynamic import (a static import would fail
 * `pnpm check` with TS2307 and fail vitest at collection — both are
 * import failures, not assertion RED). Every case gates on `requireModule`,
 * whose `not.toBeNull()` assertion names the case ID: on current code each
 * case fails as an assertion with expected (the locked contract) vs actual
 * (module missing). After the build the same assertions run against the
 * real module with no edits here.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/recentlyPlayed.ts`):
 * - `RECENTLY_PLAYED_STORAGE_KEY = 'nntune:demo-recently-played:v1'`.
 * - `MAX_RECENTLY_PLAYED = 10`.
 * - `interface RecentEntry { trackId: string; playedAt: number }` where
 *   `playedAt` is a unix-epoch-milliseconds timestamp.
 * - `createRecentlyPlayedStore({ storage, storageKey?, now? })` where
 *   `storage` is `Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>`,
 *   `storageKey` defaults to `RECENTLY_PLAYED_STORAGE_KEY`, and `now`
 *   defaults to `Date.now`. All storage access is fail-soft: throwing or
 *   corrupt stores degrade to "no entries", never throw.
 * - `store.entries()`: entries most-recent-first as a snapshot (mutating
 *   the returned array never corrupts the store).
 * - `store.record(trackId, playedAt = now())`: prepends
 *   `{ trackId, playedAt }`, moving an existing entry for the track to the
 *   front with the new timestamp (replay-to-front dedupe, never a
 *   duplicate), then caps to `MAX_RECENTLY_PLAYED` distinct tracks by
 *   dropping the oldest. `playedAt` is stored verbatim (no rounding).
 *   Empty/blank track ids and non-finite or negative `playedAt` values are
 *   ignored (no write, no throw).
 * - `store.replay(trackId)`: moves the existing entry for the track to the
 *   front with a refreshed `now()` timestamp. Unknown or blank ids are a
 *   no-op: replay never invents an entry (the page only renders replay
 *   actions for listed entries).
 * - `store.clear()`: empties the list and persists `[]`. Never throws.
 * - Serialized shape (pinned so the browser suite can seed deterministically
 *   and the page shares one reader): the key holds a JSON array of
 *   `{ trackId, playedAt }`, most-recent-first, e.g.
 *   `[{"trackId":"demo-track","playedAt":1729600000000}]`. Non-array JSON
 *   falls back to empty; malformed entries inside the array (non-object,
 *   blank/non-string id, non-finite/negative playedAt) are skipped;
 *   repeated ids dedupe keeping the first (most recent); loads cap at 10.
 * - `formatRecentPlayedAt(playedAt, now)`: relative label — under 60s is
 *   `'just now'` (future timestamps clamp here too), under 60min is
 *   `'<N> min ago'`, under 24h is `'<N> hr ago'`, otherwise `'<N> d ago'`.
 *
 * CASE INDEX (stable IDs; RP-B-* live in tests/demo-recently-played.spec.ts):
 * RP-U-01 storage key pin, RP-U-02 cap pin, RP-U-03 record shape/order,
 * RP-U-04 default playedAt, RP-U-05 most-recent-first, RP-U-06 re-record
 * dedupe, RP-U-07 cap eviction, RP-U-08 replay to front, RP-U-09 replay
 * unknown no-op, RP-U-10 blank id ignored, RP-U-11 bad playedAt ignored,
 * RP-U-12 clear, RP-U-13 clear empty, RP-U-14 entries snapshot, RP-U-15
 * serialized shape, RP-U-16 reload round-trip, RP-U-17 corrupt fallback,
 * RP-U-18 malformed entries skipped, RP-U-19 load dedupe, RP-U-20 load cap,
 * RP-U-21 throwing storage, RP-U-22 custom key, RP-U-23 relative labels.
 */

interface RecentEntry {
	trackId: string;
	playedAt: number;
}

interface RecentlyPlayedStore {
	entries(): RecentEntry[];
	record(trackId: string, playedAt?: number): void;
	replay(trackId: string): void;
	clear(): void;
}

interface RecentlyPlayedModule {
	RECENTLY_PLAYED_STORAGE_KEY: string;
	MAX_RECENTLY_PLAYED: number;
	createRecentlyPlayedStore(options: {
		storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
		storageKey?: string;
		now?: () => number;
	}): RecentlyPlayedStore;
	formatRecentPlayedAt(playedAt: number, now: number): string;
}

/**
 * Deliberately typed `string` (not a literal): TypeScript resolves
 * literal dynamic imports at typecheck time, which would fail `pnpm check`
 * while the module does not exist yet. A `string` specifier defers to the
 * runtime, where vitest throws a catchable resolution error on RED.
 */
const MODULE_SPECIFIER: string = './recentlyPlayed.js';

async function loadModule(): Promise<RecentlyPlayedModule | null> {
	try {
		return (await import(MODULE_SPECIFIER)) as RecentlyPlayedModule;
	} catch {
		return null;
	}
}

async function requireModule(caseId: string): Promise<RecentlyPlayedModule> {
	const mod = await loadModule();
	expect(
		mod,
		`${caseId}: recentlyPlayed.ts ships the locked contract (see this file's header)`
	).not.toBeNull();
	if (mod === null) throw new Error(`${caseId}: recentlyPlayed module missing`);
	return mod;
}

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

describe('recently-played contract pins', () => {
	it('RP-U-01 pins the sessionStorage key', async () => {
		const mod = await requireModule('RP-U-01');

		expect(mod.RECENTLY_PLAYED_STORAGE_KEY).toBe('nntune:demo-recently-played:v1');
	});

	it('RP-U-02 pins the distinct-track cap at 10', async () => {
		const mod = await requireModule('RP-U-02');

		expect(mod.MAX_RECENTLY_PLAYED).toBe(10);
	});
});

describe('recently-played record', () => {
	it('RP-U-03 record stores {trackId, playedAt} verbatim at the front', async () => {
		const mod = await requireModule('RP-U-03');
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage() });

		store.record('demo-track', 1_729_600_000_123);

		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 1_729_600_000_123 }]);
	});

	it('RP-U-04 record defaults playedAt to the injected now', async () => {
		const mod = await requireModule('RP-U-04');
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage(), now: () => 5_000 });

		store.record('demo-track');

		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 5_000 }]);
	});

	it('RP-U-05 entries list most-recent-first', async () => {
		const mod = await requireModule('RP-U-05');
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage() });

		store.record('demo-track', 100);
		store.record('second-signal', 200);
		store.record('third-light', 300);

		expect(store.entries().map((entry) => entry.trackId)).toEqual([
			'third-light',
			'second-signal',
			'demo-track'
		]);
	});

	it('RP-U-06 re-recording moves the track to the front without duplicating', async () => {
		const mod = await requireModule('RP-U-06');
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage() });
		store.record('demo-track', 100);
		store.record('second-signal', 200);

		store.record('demo-track', 300);

		expect(store.entries()).toEqual([
			{ trackId: 'demo-track', playedAt: 300 },
			{ trackId: 'second-signal', playedAt: 200 }
		]);
	});

	it('RP-U-07 the 11th distinct track evicts the oldest, keeping 10', async () => {
		const mod = await requireModule('RP-U-07');
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage() });

		for (let i = 1; i <= 11; i += 1) store.record(`track-${i}`, i);

		const entries = store.entries();
		expect(entries).toHaveLength(10);
		expect(entries[0]).toEqual({ trackId: 'track-11', playedAt: 11 });
		expect(entries[9]).toEqual({ trackId: 'track-2', playedAt: 2 });
		expect(entries.some((entry) => entry.trackId === 'track-1')).toBe(false);
	});
});

describe('recently-played replay', () => {
	it('RP-U-08 replay moves the entry to the front with a refreshed timestamp', async () => {
		const mod = await requireModule('RP-U-08');
		let now = 100;
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage(), now: () => now });
		store.record('demo-track', 100);
		store.record('second-signal', 200);

		now = 999;
		store.replay('demo-track');

		expect(store.entries()).toEqual([
			{ trackId: 'demo-track', playedAt: 999 },
			{ trackId: 'second-signal', playedAt: 200 }
		]);
	});

	it('RP-U-09 replay of an unknown id invents nothing', async () => {
		const mod = await requireModule('RP-U-09');
		const storage = fakeStorage();
		const store = mod.createRecentlyPlayedStore({ storage });
		store.record('demo-track', 100);

		expect(() => store.replay('never-played')).not.toThrow();
		expect(() => store.replay('')).not.toThrow();

		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
		expect(JSON.parse(storage.getItem(mod.RECENTLY_PLAYED_STORAGE_KEY) ?? '')).toHaveLength(1);
	});
});

describe('recently-played invalid input', () => {
	it.each([[''], ['   ']])('RP-U-10 record ignores blank id %s without writing', async (id) => {
		const mod = await requireModule('RP-U-10');
		const storage = fakeStorage();
		const store = mod.createRecentlyPlayedStore({ storage });

		expect(() => store.record(id, 100)).not.toThrow();

		expect(store.entries()).toEqual([]);
		expect(storage.getItem(mod.RECENTLY_PLAYED_STORAGE_KEY)).toBeNull();
	});

	it.each([[Number.NaN], [Number.POSITIVE_INFINITY], [Number.NEGATIVE_INFINITY], [-1]])(
		'RP-U-11 record ignores playedAt %s without writing',
		async (playedAt) => {
			const mod = await requireModule('RP-U-11');
			const storage = fakeStorage();
			const store = mod.createRecentlyPlayedStore({ storage });

			expect(() => store.record('demo-track', playedAt)).not.toThrow();

			expect(store.entries()).toEqual([]);
			expect(storage.getItem(mod.RECENTLY_PLAYED_STORAGE_KEY)).toBeNull();
		}
	);
});

describe('recently-played clear', () => {
	it('RP-U-12 clear empties the list and persists []', async () => {
		const mod = await requireModule('RP-U-12');
		const storage = fakeStorage();
		const store = mod.createRecentlyPlayedStore({ storage });
		store.record('demo-track', 100);

		store.clear();

		expect(store.entries()).toEqual([]);
		expect(storage.getItem(mod.RECENTLY_PLAYED_STORAGE_KEY)).toBe('[]');
	});

	it('RP-U-13 clear on an empty store does not throw', async () => {
		const mod = await requireModule('RP-U-13');
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage() });

		expect(() => store.clear()).not.toThrow();
		expect(store.entries()).toEqual([]);
	});
});

describe('recently-played persistence', () => {
	it('RP-U-14 entries() returns a snapshot that cannot corrupt the store', async () => {
		const mod = await requireModule('RP-U-14');
		const store = mod.createRecentlyPlayedStore({ storage: fakeStorage() });
		store.record('demo-track', 100);

		const snapshot = store.entries();
		snapshot.length = 0;
		snapshot.push({ trackId: 'forged', playedAt: 1 });

		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
	});

	it('RP-U-15 serializes as a JSON array of {trackId, playedAt} under the key', async () => {
		const mod = await requireModule('RP-U-15');
		const storage = fakeStorage();
		const store = mod.createRecentlyPlayedStore({ storage });

		store.record('demo-track', 100);
		store.record('second-signal', 200);

		expect(storage.getItem(mod.RECENTLY_PLAYED_STORAGE_KEY)).toBe(
			'[{"trackId":"second-signal","playedAt":200},{"trackId":"demo-track","playedAt":100}]'
		);
	});

	it('RP-U-16 a new instance over the same store reads entries (reload persistence)', async () => {
		const mod = await requireModule('RP-U-16');
		const storage = fakeStorage();
		mod.createRecentlyPlayedStore({ storage }).record('demo-track', 100);

		const reopened = mod.createRecentlyPlayedStore({ storage });

		expect(reopened.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
	});

	it.each([['{oops'], ['42'], ['null'], ['"str"'], ['{"trackId":"x"}']])(
		'RP-U-17 corrupt/non-array JSON %s falls back to empty and recovers on record',
		async (raw) => {
			const mod = await requireModule('RP-U-17');
			const storage = fakeStorage({ [mod.RECENTLY_PLAYED_STORAGE_KEY]: raw });
			const store = mod.createRecentlyPlayedStore({ storage });

			expect(store.entries()).toEqual([]);

			store.record('demo-track', 100);
			expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
		}
	);

	it('RP-U-18 malformed entries inside the array are skipped', async () => {
		const mod = await requireModule('RP-U-18');
		const storage = fakeStorage({
			[mod.RECENTLY_PLAYED_STORAGE_KEY]: JSON.stringify([
				{ trackId: 'demo-track', playedAt: 100 },
				null,
				'nope',
				42,
				{ trackId: '', playedAt: 200 },
				{ trackId: 'no-time' },
				{ playedAt: 300 },
				{ trackId: 'bad-time', playedAt: 'soon' },
				{ trackId: 'negative-time', playedAt: -5 },
				{ trackId: 'second-signal', playedAt: 50 }
			])
		});
		const store = mod.createRecentlyPlayedStore({ storage });

		expect(store.entries()).toEqual([
			{ trackId: 'demo-track', playedAt: 100 },
			{ trackId: 'second-signal', playedAt: 50 }
		]);
	});

	it('RP-U-19 repeated ids on load dedupe keeping the first (most recent)', async () => {
		const mod = await requireModule('RP-U-19');
		const storage = fakeStorage({
			[mod.RECENTLY_PLAYED_STORAGE_KEY]: JSON.stringify([
				{ trackId: 'demo-track', playedAt: 300 },
				{ trackId: 'second-signal', playedAt: 200 },
				{ trackId: 'demo-track', playedAt: 100 }
			])
		});
		const store = mod.createRecentlyPlayedStore({ storage });

		expect(store.entries()).toEqual([
			{ trackId: 'demo-track', playedAt: 300 },
			{ trackId: 'second-signal', playedAt: 200 }
		]);
	});

	it('RP-U-20 loads cap at 10 keeping the front', async () => {
		const mod = await requireModule('RP-U-20');
		const seeded = Array.from({ length: 12 }, (_, i) => ({
			trackId: `track-${i + 1}`,
			playedAt: 100 + i
		}));
		const storage = fakeStorage({ [mod.RECENTLY_PLAYED_STORAGE_KEY]: JSON.stringify(seeded) });
		const store = mod.createRecentlyPlayedStore({ storage });

		const entries = store.entries();
		expect(entries).toHaveLength(10);
		expect(entries[0]?.trackId).toBe('track-1');
		expect(entries[9]?.trackId).toBe('track-10');
	});

	it('RP-U-21 a throwing store degrades to empty with contained writes', async () => {
		const mod = await requireModule('RP-U-21');
		const store = mod.createRecentlyPlayedStore({ storage: throwingStorage() });

		expect(store.entries()).toEqual([]);
		expect(() => store.record('demo-track', 100)).not.toThrow();
		expect(() => store.replay('demo-track')).not.toThrow();
		expect(() => store.clear()).not.toThrow();
		expect(store.entries()).toEqual([]);
	});

	it('RP-U-22 a custom storage key is honoured, leaving the default untouched', async () => {
		const mod = await requireModule('RP-U-22');
		const storage = fakeStorage();
		const store = mod.createRecentlyPlayedStore({ storage, storageKey: 'custom:recent' });

		store.record('demo-track', 100);

		expect(storage.getItem('custom:recent')).toBe('[{"trackId":"demo-track","playedAt":100}]');
		expect(storage.getItem(mod.RECENTLY_PLAYED_STORAGE_KEY)).toBeNull();
	});
});

describe('recently-played relative labels', () => {
	it.each([
		['RP-U-23a just now', 0, 59_999, 'just now'],
		['RP-U-23b future clamps to just now', 120_000, 60_000, 'just now'],
		['RP-U-23c minutes', 0, 5 * 60_000, '5 min ago'],
		['RP-U-23d hours', 0, 3 * 3_600_000, '3 hr ago'],
		['RP-U-23e days', 0, 2 * 86_400_000, '2 d ago']
	])('%s', async (_label, playedAt, now, expected) => {
		const mod = await requireModule('RP-U-23');

		expect(mod.formatRecentPlayedAt(playedAt, now)).toBe(expected);
	});
});
