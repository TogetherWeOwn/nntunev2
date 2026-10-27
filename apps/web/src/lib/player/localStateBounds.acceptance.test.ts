import { describe, expect, it, vi } from 'vitest';
import { createResumeStore, RESUME_STORAGE_KEY } from './resume.js';
import {
	createRecentlyPlayedStore,
	MAX_RECENTLY_PLAYED,
	RECENTLY_PLAYED_STORAGE_KEY
} from './recentlyPlayed.js';

/**
 * RED acceptance: bound local player-state decoding and preserve opaque resume IDs.
 *
 * OWNERSHIP: Test-Author owned. The paired build implements `resume.ts` and
 * `recentlyPlayed.ts` to satisfy this file and must not edit it; a wrong test
 * comes back to the acceptance card for this file instead of being fixed in
 * the build. Sibling protected suites (`resume.acceptance.test.ts`,
 * `recentlyPlayed.acceptance.test.ts`) keep passing unchanged.
 *
 * RED MECHANISM: static imports of the existing modules (no dynamic import,
 * so a failure here can never be an import/collection failure). Every new
 * behavior below fails today as an assertion against current production
 * source: the decoders parse before checking size, cap nothing before the
 * history limit, and read resume state through a normal object. Controls
 * (ordinary payloads, corrupt fallback, throwing storage) already pass and
 * prove the harness is sound.
 *
 * LOCKED CONTRACT (numeric bounds; the builder records the final chosen
 * limits in exactly ONE canonical source module and these tests pin the
 * numbers, never the module path, so no gratuitous path pin is created):
 * - `MAX_SERIALIZED_CHARS = 65_536` (64 KiB). Both demo payloads (resume
 *   positions, recently-played) are measured as the raw stored string,
 *   exactly as returned by storage, BEFORE `JSON.parse`. Lengths
 *   `<= 65_536` decode normally; lengths `> 65_536` are refused WITHOUT
 *   calling `JSON.parse` and fall back fail-closed (resume: every lookup
 *   null; recent: empty list). The limit is inclusive and untrimmed.
 * - `MAX_RESUME_ENTRIES = 256`. A parsed resume object with more than 256
 *   own enumerable string keys — valid or not — is refused wholesale
 *   (fail-closed empty), not truncated. 256 keys decode normally.
 * - `MAX_OPAQUE_ID_CHARS = 256` code units (`String.length` semantics).
 *   Resume saves and recent records with longer ids are ignored (no write,
 *   no throw); seeded entries with longer ids are treated as absent while
 *   valid siblings still decode.
 * - `MAX_RECENTLY_PLAYED` stays 10 with the existing retention policy
 *   (most-recent-first, replay-to-front dedupe, load cap keeps the front).
 * - Writer refusal is deterministic and state-preserving: a resume save that
 *   would exceed the entry budget, the id cap, or the serialized cap is
 *   ignored with the prior stored state byte-identical; overwriting an
 *   existing id at capacity still works. Nothing is evicted implicitly and
 *   no partial write is left behind.
 * - Fail-closed reads never throw, never call `console.*` with raw values
 *   (or at all), and never touch unrelated storage keys. Versioned keys and
 *   the JSON shapes (`{id: seconds}`, `[{trackId, playedAt}]`) are unchanged.
 * - Resume ids are opaque OWN keys: `__proto__`, `constructor`, `toString`
 *   and friends round-trip as data, inherited members never read as saved
 *   time, and no read/write/clear mutates any prototype.
 *
 * Reconciliation: every ordinary fixture in the sibling protected suites is
 * orders of magnitude under these bounds (demo ids are <= 13 chars, payloads
 * < 1 KiB), so valid behavior is preserved bit-for-bit; LSB-U-16 replays
 * those shapes. Favorites decoding, queue/speed/volume stores, auth scopes,
 * telemetry and any new data store are explicitly outside this slice.
 *
 * CASE INDEX (stable IDs; LSB-B-* live in tests/player-local-state-bounds.spec.ts):
 * LSB-U-01 resume raw at/over the serialized cap, LSB-U-02 recent raw
 * at/over the cap, LSB-U-03 resume over-cap side-effect silence, LSB-U-04
 * recent over-cap side-effect silence, LSB-U-05 resume entry budget,
 * LSB-U-06 resume writer refusal at capacity, LSB-U-07 resume id lengths,
 * LSB-U-08 recent id lengths, LSB-U-09 inherited names read as null,
 * LSB-U-10 `__proto__` save/read round-trip, LSB-U-11 seeded hostile-key
 * map, LSB-U-12 clear of an inherited-only name writes nothing, LSB-U-13
 * malformed shapes fail closed, LSB-U-14 hostile numerics skipped,
 * LSB-U-15 writer output stays readable under the caps, LSB-U-16
 * reconciliation with protected fixtures, LSB-U-17 duplicate-heavy recent
 * payload, LSB-U-18 throwing/blocked storage retained.
 */

const MAX_SERIALIZED_CHARS = 65_536;
const MAX_RESUME_ENTRIES = 256;
const MAX_OPAQUE_ID_CHARS = 256;

type TestStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

interface FakeStore extends TestStorage {
	writeCount(): number;
	keys(): string[];
	raw(key: string): string | null;
}

function fakeStorage(initial: Record<string, string> = {}): FakeStore {
	const map = new Map<string, string>(Object.entries(initial));
	let writes = 0;
	return {
		getItem: (key: string): string | null => map.get(key) ?? null,
		setItem: (key: string, value: string): void => {
			writes += 1;
			map.set(key, value);
		},
		removeItem: (key: string): void => {
			map.delete(key);
		},
		writeCount: (): number => writes,
		keys: (): string[] => [...map.keys()],
		raw: (key: string): string | null => map.get(key) ?? null
	};
}

function throwingStorage(): TestStorage {
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

/** Storage whose `getItem` accessor itself throws (blocked-getter shape). */
function getterThrowingStorage(backing: FakeStore): TestStorage {
	return {
		get getItem(): TestStorage['getItem'] {
			throw new Error('blocked getter');
		},
		setItem: (key: string, value: string): void => backing.setItem(key, value),
		removeItem: (key: string): void => backing.removeItem(key)
	};
}

/**
 * Valid resume JSON of EXACTLY `totalChars` chars: the given numeric entries
 * plus one inert `pad` key (a string value, skipped as an invalid position)
 * sized to fill. `JSON.stringify` is deterministic here (`x` needs no
 * escaping), so the length is exact by construction.
 */
function sizedResumeRaw(totalChars: number, entries: Record<string, number>): string {
	const probe = JSON.stringify({ ...entries, pad: '' });
	const fillerLength = totalChars - probe.length;
	if (fillerLength < 0) throw new Error(`entries already exceed ${totalChars} chars`);
	return JSON.stringify({ ...entries, pad: 'x'.repeat(fillerLength) });
}

/**
 * Valid recent JSON of EXACTLY `totalChars` chars: the entry array plus
 * trailing JSON whitespace (valid input, still length-counted raw storage).
 */
function sizedRecentRaw(
	totalChars: number,
	entries: { trackId: string; playedAt: number }[]
): string {
	const body = JSON.stringify(entries);
	const padding = totalChars - body.length;
	if (padding < 0) throw new Error(`entries already exceed ${totalChars} chars`);
	return body + ' '.repeat(padding);
}

function watchJsonParse(): { called: () => number; restore: () => void } {
	const spy = vi.spyOn(JSON, 'parse');
	return {
		called: (): number => spy.mock.calls.length,
		restore: (): void => spy.mockRestore()
	};
}

function watchConsole(): { calls: () => number; restore: () => void } {
	const spies = [
		vi.spyOn(console, 'log'),
		vi.spyOn(console, 'info'),
		vi.spyOn(console, 'warn'),
		vi.spyOn(console, 'error'),
		vi.spyOn(console, 'debug')
	];
	return {
		calls: (): number => spies.reduce((sum, spy) => sum + spy.mock.calls.length, 0),
		restore: (): void => {
			for (const spy of spies) spy.mockRestore();
		}
	};
}

function resumeEntries(count: number, prefix = 'track'): Record<string, number> {
	return Object.fromEntries(Array.from({ length: count }, (_, i) => [`${prefix}-${i}`, i + 0.5]));
}

describe('LSB-U-01 resume raw serialized cap', () => {
	it('at exactly 65_536 chars the payload still decodes', () => {
		const raw = sizedResumeRaw(MAX_SERIALIZED_CHARS, { 'demo-track': 42.5 });
		expect(raw.length).toBe(MAX_SERIALIZED_CHARS);
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });

		expect(store.positionFor('demo-track')).toBe(42.5);
	});

	it('at 65_537 chars the payload is refused WITHOUT calling JSON.parse', () => {
		const raw = sizedResumeRaw(MAX_SERIALIZED_CHARS + 1, { 'demo-track': 42.5 });
		expect(raw.length).toBe(MAX_SERIALIZED_CHARS + 1);
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });
		const parse = watchJsonParse();
		try {
			expect(store.positionFor('demo-track')).toBeNull();
			expect(parse.called()).toBe(0);
		} finally {
			parse.restore();
		}
	});
});

describe('LSB-U-02 recent raw serialized cap', () => {
	it('at exactly 65_536 chars the payload still decodes', () => {
		const raw = sizedRecentRaw(MAX_SERIALIZED_CHARS, [{ trackId: 'demo-track', playedAt: 100 }]);
		expect(raw.length).toBe(MAX_SERIALIZED_CHARS);
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});

		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
	});

	it('at 65_537 chars the payload is refused WITHOUT calling JSON.parse', () => {
		const raw = sizedRecentRaw(MAX_SERIALIZED_CHARS + 1, [
			{ trackId: 'demo-track', playedAt: 100 }
		]);
		expect(raw.length).toBe(MAX_SERIALIZED_CHARS + 1);
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});
		const parse = watchJsonParse();
		try {
			expect(store.entries()).toEqual([]);
			expect(parse.called()).toBe(0);
		} finally {
			parse.restore();
		}
	});

	it('a large valid array over the cap is refused WITHOUT calling JSON.parse', () => {
		const entries = Array.from({ length: 3000 }, (_, i) => ({
			trackId: `track-${i}`,
			playedAt: i
		}));
		const raw = JSON.stringify(entries);
		expect(raw.length).toBeGreaterThan(MAX_SERIALIZED_CHARS);
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});
		const parse = watchJsonParse();
		try {
			expect(store.entries()).toEqual([]);
			expect(parse.called()).toBe(0);
		} finally {
			parse.restore();
		}
	});
});

describe('LSB-U-03 resume over-cap reads stay silent and recover', () => {
	it('no throw, no console output, unrelated keys untouched, save recovers', () => {
		const raw = sizedResumeRaw(MAX_SERIALIZED_CHARS + 100, { 'demo-track': 42.5 });
		const storage = fakeStorage({
			[RESUME_STORAGE_KEY]: raw,
			'unrelated:key': 'keep-me'
		});
		const store = createResumeStore({ storage });
		const logged = watchConsole();
		try {
			expect(() => store.positionFor('demo-track')).not.toThrow();
			expect(store.positionFor('demo-track')).toBeNull();
			expect(logged.calls()).toBe(0);
		} finally {
			logged.restore();
		}
		expect(storage.raw('unrelated:key')).toBe('keep-me');

		store.save('demo-track', 9);
		expect(store.positionFor('demo-track')).toBe(9);
		expect(storage.raw('unrelated:key')).toBe('keep-me');
	});
});

describe('LSB-U-04 recent over-cap reads stay silent and recover', () => {
	it('no throw, no console output, unrelated keys untouched, record recovers', () => {
		const raw = sizedRecentRaw(MAX_SERIALIZED_CHARS + 100, [
			{ trackId: 'demo-track', playedAt: 100 }
		]);
		const storage = fakeStorage({
			[RECENTLY_PLAYED_STORAGE_KEY]: raw,
			'unrelated:key': 'keep-me'
		});
		const store = createRecentlyPlayedStore({ storage });
		const logged = watchConsole();
		try {
			expect(() => store.entries()).not.toThrow();
			expect(store.entries()).toEqual([]);
			expect(logged.calls()).toBe(0);
		} finally {
			logged.restore();
		}
		expect(storage.raw('unrelated:key')).toBe('keep-me');

		store.record('demo-track', 100);
		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
		expect(storage.raw('unrelated:key')).toBe('keep-me');
	});
});

describe('LSB-U-05 resume entry budget', () => {
	it('256 entries decode normally', () => {
		const storage = fakeStorage({
			[RESUME_STORAGE_KEY]: JSON.stringify(resumeEntries(MAX_RESUME_ENTRIES))
		});
		const store = createResumeStore({ storage });

		expect(store.positionFor('track-0')).toBe(0.5);
		expect(store.positionFor('track-255')).toBe(255.5);
	});

	it('257 valid entries refuse the whole payload fail-closed', () => {
		const raw = JSON.stringify(resumeEntries(MAX_RESUME_ENTRIES + 1));
		expect(raw.length).toBeLessThan(MAX_SERIALIZED_CHARS);
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });

		expect(store.positionFor('track-0')).toBeNull();
		expect(store.positionFor('track-256')).toBeNull();
	});

	it('the budget counts every own key, valid or not', () => {
		const entries = resumeEntries(MAX_RESUME_ENTRIES) as Record<string, unknown>;
		entries['junk'] = 'not-a-number';
		const raw = JSON.stringify(entries);
		expect(raw.length).toBeLessThan(MAX_SERIALIZED_CHARS);
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });

		expect(store.positionFor('track-0')).toBeNull();
	});
});

describe('LSB-U-06 resume writer refusal at capacity', () => {
	function fillToCapacity(): { store: ReturnType<typeof createResumeStore>; storage: FakeStore } {
		const storage = fakeStorage();
		const store = createResumeStore({ storage });
		for (let i = 0; i < MAX_RESUME_ENTRIES; i += 1) store.save(`track-${i}`, i + 0.5);
		return { store, storage };
	}

	it('a full store of 256 entries stays readable', () => {
		const { store } = fillToCapacity();

		expect(store.positionFor('track-0')).toBe(0.5);
		expect(store.positionFor('track-255')).toBe(255.5);
	});

	it('a 257th distinct id is refused with prior state preserved', () => {
		const { store, storage } = fillToCapacity();
		const before = storage.raw(RESUME_STORAGE_KEY);
		const writesBefore = storage.writeCount();

		expect(() => store.save('track-overflow', 1)).not.toThrow();

		expect(store.positionFor('track-overflow')).toBeNull();
		expect(store.positionFor('track-0')).toBe(0.5);
		expect(storage.writeCount()).toBe(writesBefore);
		expect(storage.raw(RESUME_STORAGE_KEY)).toBe(before);
	});

	it('overwriting an existing id at capacity still works', () => {
		const { store } = fillToCapacity();

		store.save('track-0', 99);

		expect(store.positionFor('track-0')).toBe(99);
		expect(store.positionFor('track-255')).toBe(255.5);
	});
});

describe('LSB-U-07 resume opaque id lengths', () => {
	it('a 256-code-unit id saves and reads', () => {
		const id = 't'.repeat(MAX_OPAQUE_ID_CHARS);
		const store = createResumeStore({ storage: fakeStorage() });

		store.save(id, 7);

		expect(store.positionFor(id)).toBe(7);
	});

	it('a 257-code-unit id save is ignored without writing', () => {
		const id = 't'.repeat(MAX_OPAQUE_ID_CHARS + 1);
		const storage = fakeStorage();
		const store = createResumeStore({ storage });

		expect(() => store.save(id, 7)).not.toThrow();

		expect(store.positionFor(id)).toBeNull();
		expect(storage.writeCount()).toBe(0);
		expect(storage.raw(RESUME_STORAGE_KEY)).toBeNull();
	});

	it('a seeded over-long id is absent while valid siblings decode', () => {
		const longId = 't'.repeat(MAX_OPAQUE_ID_CHARS + 1);
		const raw = JSON.stringify({ 'demo-track': 42.5, [longId]: 1 });
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });

		expect(store.positionFor('demo-track')).toBe(42.5);
		expect(store.positionFor(longId)).toBeNull();
	});

	it('id length is counted in code units, not code points', () => {
		const okId = 't'.repeat(254) + '\u{1F600}';
		const overId = 't'.repeat(255) + '\u{1F600}';
		expect(okId.length).toBe(MAX_OPAQUE_ID_CHARS);
		expect(overId.length).toBe(MAX_OPAQUE_ID_CHARS + 1);
		const store = createResumeStore({ storage: fakeStorage() });

		store.save(okId, 1);
		store.save(overId, 2);

		expect(store.positionFor(okId)).toBe(1);
		expect(store.positionFor(overId)).toBeNull();
	});
});

describe('LSB-U-08 recent opaque id lengths', () => {
	it('a 256-code-unit id records and reads', () => {
		const id = 't'.repeat(MAX_OPAQUE_ID_CHARS);
		const store = createRecentlyPlayedStore({ storage: fakeStorage() });

		store.record(id, 100);

		expect(store.entries()).toEqual([{ trackId: id, playedAt: 100 }]);
	});

	it('a 257-code-unit id record is ignored without writing', () => {
		const id = 't'.repeat(MAX_OPAQUE_ID_CHARS + 1);
		const storage = fakeStorage();
		const store = createRecentlyPlayedStore({ storage });

		expect(() => store.record(id, 100)).not.toThrow();

		expect(store.entries()).toEqual([]);
		expect(storage.writeCount()).toBe(0);
		expect(storage.raw(RECENTLY_PLAYED_STORAGE_KEY)).toBeNull();
	});

	it('a seeded over-long id is skipped while valid siblings decode', () => {
		const longId = 't'.repeat(MAX_OPAQUE_ID_CHARS + 1);
		const raw = JSON.stringify([
			{ trackId: 'demo-track', playedAt: 100 },
			{ trackId: longId, playedAt: 50 }
		]);
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});

		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 100 }]);
	});
});

describe('LSB-U-09 inherited resume names read as null', () => {
	it.each([['toString'], ['constructor'], ['hasOwnProperty'], ['valueOf'], ['isPrototypeOf'], ['__proto__']])(
		'positionFor(%s) on an empty store is null, never an inherited member',
		(name) => {
			const store = createResumeStore({ storage: fakeStorage() });

			expect(store.positionFor(name)).toBeNull();
		}
	);
});

describe('LSB-U-10 __proto__ save/read round-trip', () => {
	it('stores the value as own data without touching prototypes', () => {
		const store = createResumeStore({ storage: fakeStorage() });

		expect(() => store.save('__proto__', 42.5)).not.toThrow();

		expect(store.positionFor('__proto__')).toBe(42.5);
		expect(Object.getPrototypeOf({})).toBe(Object.prototype);
		expect(({} as Record<string, unknown>)['polluted']).toBeUndefined();
		expect(typeof ({} as Record<string, unknown>)['toString']).toBe('function');
	});
});

describe('LSB-U-11 seeded hostile-key resume map', () => {
	const HOSTILE_RAW = '{"__proto__":1,"constructor":2,"toString":3,"demo-track":4}';

	it('reads hostile names as own values with prototypes intact', () => {
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: HOSTILE_RAW }) });

		expect(store.positionFor('__proto__')).toBe(1);
		expect(store.positionFor('constructor')).toBe(2);
		expect(store.positionFor('toString')).toBe(3);
		expect(store.positionFor('demo-track')).toBe(4);
		expect(Object.getPrototypeOf({})).toBe(Object.prototype);
		expect(typeof ({} as Record<string, unknown>)['toString']).toBe('function');
	});

	it('a write cycle keeps hostile keys as data, never prototype writes', () => {
		const storage = fakeStorage({ [RESUME_STORAGE_KEY]: HOSTILE_RAW });
		const store = createResumeStore({ storage });

		store.save('demo-track', 9);

		expect(store.positionFor('demo-track')).toBe(9);
		expect(store.positionFor('__proto__')).toBe(1);
		expect(Object.getPrototypeOf({})).toBe(Object.prototype);
		expect(({} as Record<string, unknown>)['polluted']).toBeUndefined();
	});
});

describe('LSB-U-12 clear of an inherited-only name writes nothing', () => {
	it.each([['toString'], ['constructor'], ['__proto__']])('clear(%s) is a no-op write', (name) => {
		const storage = fakeStorage({ [RESUME_STORAGE_KEY]: '{"demo-track":5}' });
		const store = createResumeStore({ storage });

		expect(() => store.clear(name)).not.toThrow();

		expect(storage.writeCount()).toBe(0);
		expect(store.positionFor(name)).toBeNull();
		expect(store.positionFor('demo-track')).toBe(5);
	});
});

describe('LSB-U-13 malformed shapes fail closed', () => {
	it.each([['[1,2,3]'], ['"str"'], ['42'], ['null'], ['{"a":{"nested":true}}'], ['{"a":[1]}'], ['{"a":null,"b":true}']])(
		'resume shape %s reads as absent',
		(raw) => {
			const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });

			expect(store.positionFor('demo-track')).toBeNull();
			expect(store.positionFor('a')).toBeNull();
		}
	);

	it.each([['{"not":"array"}'], ['"str"'], ['42'], ['null'], ['[{"trackId":42,"playedAt":{}}]']])(
		'recent shape %s reads as empty',
		(raw) => {
			const store = createRecentlyPlayedStore({
				storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
			});

			expect(store.entries()).toEqual([]);
		}
	);

	it('an under-cap valid-JSON wrong shape still parses, then falls back', () => {
		const raw = `"${'q'.repeat(60_000)}"`;
		expect(raw.length).toBeLessThan(MAX_SERIALIZED_CHARS);
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});
		const parse = watchJsonParse();
		try {
			expect(store.entries()).toEqual([]);
			expect(parse.called()).toBeGreaterThan(0);
		} finally {
			parse.restore();
		}
	});

	it('over-cap malformed resume bytes are refused WITHOUT calling JSON.parse', () => {
		const raw = `{oops${'z'.repeat(MAX_SERIALIZED_CHARS)}`;
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });
		const parse = watchJsonParse();
		try {
			expect(store.positionFor('demo-track')).toBeNull();
			expect(parse.called()).toBe(0);
		} finally {
			parse.restore();
		}
	});

	it('over-cap malformed recent bytes are refused WITHOUT calling JSON.parse', () => {
		const raw = `[{oops${'z'.repeat(MAX_SERIALIZED_CHARS)}`;
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});
		const parse = watchJsonParse();
		try {
			expect(store.entries()).toEqual([]);
			expect(parse.called()).toBe(0);
		} finally {
			parse.restore();
		}
	});
});

describe('LSB-U-14 hostile numerics are skipped deterministically', () => {
	it('resume keeps finite non-negative values and skips the rest', () => {
		const raw =
			'{"ok":5,"zero":0,"inf":1e400,"ninf":-1e400,"neg":-1,"str":"x",' +
			'"obj":{},"arr":[],"nil":null,"bool":true,"huge":1e308}';
		const store = createResumeStore({ storage: fakeStorage({ [RESUME_STORAGE_KEY]: raw }) });

		expect(store.positionFor('ok')).toBe(5);
		expect(store.positionFor('zero')).toBe(0);
		expect(store.positionFor('huge')).toBe(1e308);
		for (const name of ['inf', 'ninf', 'neg', 'str', 'obj', 'arr', 'nil', 'bool']) {
			expect(store.positionFor(name)).toBeNull();
		}
	});

	it('resume duplicate JSON keys keep deterministic last-wins semantics', () => {
		const store = createResumeStore({
			storage: fakeStorage({ [RESUME_STORAGE_KEY]: '{"dup":1,"dup":2}' })
		});

		expect(store.positionFor('dup')).toBe(2);
	});

	it('recent keeps valid entries and skips hostile playedAt values', () => {
		const raw = JSON.stringify([
			{ trackId: 'demo-track', playedAt: 100 },
			{ trackId: 'inf-time', playedAt: 1e400 },
			{ trackId: 'neg-time', playedAt: -5 },
			{ trackId: 'str-time', playedAt: 'soon' },
			{ trackId: 'nil-time', playedAt: null },
			{ trackId: 'missing-time' },
			{ playedAt: 50 },
			{ trackId: 'second-signal', playedAt: 50 }
		]);
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});

		expect(store.entries()).toEqual([
			{ trackId: 'demo-track', playedAt: 100 },
			{ trackId: 'second-signal', playedAt: 50 }
		]);
	});
});

describe('LSB-U-15 writer output stays readable under the caps', () => {
	it('256 small resume saves round-trip within the serialized cap', () => {
		const storage = fakeStorage();
		const store = createResumeStore({ storage });
		for (let i = 0; i < MAX_RESUME_ENTRIES; i += 1) store.save(`track-${i}`, i + 0.5);

		const raw = storage.raw(RESUME_STORAGE_KEY) ?? '';
		expect(raw.length).toBeLessThanOrEqual(MAX_SERIALIZED_CHARS);
		const reopened = createResumeStore({ storage });
		expect(reopened.positionFor('track-0')).toBe(0.5);
		expect(reopened.positionFor('track-255')).toBe(255.5);
	});

	it('resume saves refuse before the serialized output could exceed the cap', () => {
		const storage = fakeStorage();
		const store = createResumeStore({ storage });
		const ids = Array.from({ length: MAX_RESUME_ENTRIES }, (_, i) => `w${i}${'y'.repeat(248)}`);
		for (const id of ids) expect(id.length).toBeLessThanOrEqual(MAX_OPAQUE_ID_CHARS);

		for (const id of ids) store.save(id, 1);

		const raw = storage.raw(RESUME_STORAGE_KEY) ?? '';
		expect(raw.length).toBeLessThanOrEqual(MAX_SERIALIZED_CHARS);
		const saved = ids.filter((id) => store.positionFor(id) === 1);
		expect(saved.length).toBeGreaterThan(0);
		expect(saved.length).toBeLessThan(ids.length);
		const reopened = createResumeStore({ storage });
		for (const id of saved) expect(reopened.positionFor(id)).toBe(1);
	});

	it('10 max-length recent records round-trip within the serialized cap', () => {
		const storage = fakeStorage();
		const store = createRecentlyPlayedStore({ storage });
		for (let i = 0; i < MAX_RECENTLY_PLAYED; i += 1) {
			store.record(`${i}${'r'.repeat(MAX_OPAQUE_ID_CHARS - 1)}`, i);
		}

		const entries = store.entries();
		expect(entries).toHaveLength(MAX_RECENTLY_PLAYED);
		expect((storage.raw(RECENTLY_PLAYED_STORAGE_KEY) ?? '').length).toBeLessThanOrEqual(
			MAX_SERIALIZED_CHARS
		);
	});
});

describe('LSB-U-16 reconciliation with protected fixtures', () => {
	it('ordinary valid payloads decode exactly as the sibling suites pin', () => {
		const resume = createResumeStore({
			storage: fakeStorage({
				[RESUME_STORAGE_KEY]: '{"demo-track":"soon","second-signal":-4,"third-light":12}'
			})
		});
		expect(resume.positionFor('demo-track')).toBeNull();
		expect(resume.positionFor('second-signal')).toBeNull();
		expect(resume.positionFor('third-light')).toBe(12);

		const recentSeeded = Array.from({ length: 12 }, (_, i) => ({
			trackId: `track-${i + 1}`,
			playedAt: 100 + i
		}));
		const recent = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: JSON.stringify(recentSeeded) })
		});
		const entries = recent.entries();
		expect(entries).toHaveLength(10);
		expect(entries[0]?.trackId).toBe('track-1');
		expect(entries[9]?.trackId).toBe('track-10');

		const corrupt = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: '{oops' })
		});
		expect(corrupt.entries()).toEqual([]);
	});

	it('versioned keys and the history cap are preserved', () => {
		expect(RESUME_STORAGE_KEY).toBe('nntune:resume:v1');
		expect(RECENTLY_PLAYED_STORAGE_KEY).toBe('nntune:demo-recently-played:v1');
		expect(MAX_RECENTLY_PLAYED).toBe(10);
	});
});

describe('LSB-U-17 duplicate-heavy recent payload', () => {
	it('300 copies of one id load as a single entry without repeats', () => {
		const raw = JSON.stringify(
			Array.from({ length: 300 }, (_, i) => ({ trackId: 'demo-track', playedAt: 300 - i }))
		);
		expect(raw.length).toBeLessThan(MAX_SERIALIZED_CHARS);
		const store = createRecentlyPlayedStore({
			storage: fakeStorage({ [RECENTLY_PLAYED_STORAGE_KEY]: raw })
		});

		expect(store.entries()).toEqual([{ trackId: 'demo-track', playedAt: 300 }]);
	});
});

describe('LSB-U-18 throwing and blocked storage retained', () => {
	it('resume degrades to nulls with contained writes and no throw', () => {
		for (const storage of [throwingStorage(), getterThrowingStorage(fakeStorage())]) {
			const store = createResumeStore({ storage });

			expect(store.positionFor('demo-track')).toBeNull();
			expect(() => store.save('demo-track', 5)).not.toThrow();
			expect(() => store.clear('demo-track')).not.toThrow();
		}
	});

	it('recent degrades to empty with contained writes and no throw', () => {
		for (const storage of [throwingStorage(), getterThrowingStorage(fakeStorage())]) {
			const store = createRecentlyPlayedStore({ storage });

			expect(store.entries()).toEqual([]);
			expect(() => store.record('demo-track', 100)).not.toThrow();
			expect(() => store.replay('demo-track')).not.toThrow();
			expect(() => store.clear()).not.toThrow();
			expect(store.entries()).toEqual([]);
		}
	});
});
