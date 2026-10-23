import { describe, expect, it } from 'vitest';
import { createResumeStore, RESUME_STORAGE_KEY } from './resume.js';

/**
 * Test-Author owned RED acceptance for per-track resume positions. The
 * Builder implements `resume.ts` to satisfy this file and must not edit it.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/resume.ts`):
 * - `RESUME_STORAGE_KEY = 'nntune:resume:v1'`.
 * - `createResumeStore({ storage, storageKey? })` where `storage` is
 *   `Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>`; `storageKey`
 *   defaults to `RESUME_STORAGE_KEY`. All storage access is fail-soft:
 *   throwing or corrupt stores degrade to "no positions", never throw.
 * - `store.save(trackId, seconds)`: persists `seconds` for `trackId`.
 *   Non-finite or negative `seconds`, and empty track ids, are ignored
 *   (no write, no throw). A later save for the same track overwrites.
 *   Values are stored verbatim (no rounding by the store).
 * - `store.positionFor(trackId)`: the saved seconds, or null when absent,
 *   corrupt, or non-finite. Never throws.
 * - `store.clear(trackId)`: drops the entry (no-op when absent). The page
 *   calls it when a track ends, so a finished track never resumes mid-way.
 * - Serialized shape (pinned so the browser suite can seed deterministically
 *   and the page can share one reader): the key holds a JSON object mapping
 *   track id -> seconds, e.g. `{"demo-track":42.5}`. Non-object JSON and
 *   non-finite/negative values inside the map are treated as absent.
 */

function fakeStorage(initial: Record<string, string> = {}) {
	const map = new Map(Object.entries(initial));
	return {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => void map.set(key, value),
		removeItem: (key: string) => void map.delete(key)
	};
}

function throwingStorage() {
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

describe('resume round-trip', () => {
	it('pins the storage key', () => {
		expect(RESUME_STORAGE_KEY).toBe('nntune:resume:v1');
	});

	it('saves and returns the position verbatim', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage });

		resume.save('demo-track', 42.5);
		expect(resume.positionFor('demo-track')).toBe(42.5);
	});

	it('keeps positions isolated per track id', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage });

		resume.save('demo-track', 10);
		resume.save('second-signal', 20);

		expect(resume.positionFor('demo-track')).toBe(10);
		expect(resume.positionFor('second-signal')).toBe(20);
	});

	it('returns null for unknown tracks', () => {
		const resume = createResumeStore({ storage: fakeStorage() });

		expect(resume.positionFor('never-played')).toBeNull();
	});

	it('a later save for the same track overwrites', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage });

		resume.save('demo-track', 10);
		resume.save('demo-track', 33.25);

		expect(resume.positionFor('demo-track')).toBe(33.25);
	});

	it('survives a reload: a new instance over the same store reads positions', () => {
		const storage = fakeStorage();
		createResumeStore({ storage }).save('demo-track', 7);

		expect(createResumeStore({ storage }).positionFor('demo-track')).toBe(7);
	});

	it('serializes as a JSON object mapping track id to seconds', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage });

		resume.save('demo-track', 42.5);
		resume.save('second-signal', 3);

		expect(JSON.parse(storage.getItem(RESUME_STORAGE_KEY) ?? '')).toEqual({
			'demo-track': 42.5,
			'second-signal': 3
		});
	});

	it('honours a custom storage key', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage, storageKey: 'custom:resume' });

		resume.save('demo-track', 5);
		expect(storage.getItem('custom:resume')).toBe('{"demo-track":5}');
		expect(storage.getItem(RESUME_STORAGE_KEY)).toBeNull();
	});
});

describe('resume clearing', () => {
	it('clear drops the entry so the track restarts', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage });
		resume.save('demo-track', 42.5);

		resume.clear('demo-track');

		expect(resume.positionFor('demo-track')).toBeNull();
		expect(JSON.parse(storage.getItem(RESUME_STORAGE_KEY) ?? '')).toEqual({});
	});

	it('clear keeps other tracks and tolerates absent entries', () => {
		const resume = createResumeStore({ storage: fakeStorage() });
		resume.save('demo-track', 1);
		resume.save('second-signal', 2);

		expect(() => resume.clear('never-played')).not.toThrow();
		resume.clear('demo-track');

		expect(resume.positionFor('demo-track')).toBeNull();
		expect(resume.positionFor('second-signal')).toBe(2);
	});
});

describe('resume invalid input', () => {
	it.each([[Number.NaN], [Number.POSITIVE_INFINITY], [Number.NEGATIVE_INFINITY], [-1], [-0.5]])(
		'ignores invalid seconds %s without writing',
		(seconds) => {
			const storage = fakeStorage();
			const resume = createResumeStore({ storage });

			expect(() => resume.save('demo-track', seconds)).not.toThrow();
			expect(resume.positionFor('demo-track')).toBeNull();
			expect(storage.getItem(RESUME_STORAGE_KEY)).toBeNull();
		}
	);

	it('ignores empty track ids without writing', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage });

		expect(() => resume.save('', 10)).not.toThrow();
		expect(storage.getItem(RESUME_STORAGE_KEY)).toBeNull();
	});

	it('accepts zero (explicit restart marker)', () => {
		const resume = createResumeStore({ storage: fakeStorage() });

		resume.save('demo-track', 0);
		expect(resume.positionFor('demo-track')).toBe(0);
	});
});

describe('resume corrupt storage', () => {
	it.each([['{oops'], ['[1,2]'], ['"str"'], ['42'], ['null']])(
		'treats corrupt/non-object JSON %s as empty and recovers on save',
		(raw) => {
			const storage = fakeStorage({ [RESUME_STORAGE_KEY]: raw });
			const resume = createResumeStore({ storage });

			expect(resume.positionFor('demo-track')).toBeNull();

			resume.save('demo-track', 9);
			expect(resume.positionFor('demo-track')).toBe(9);
		}
	);

	it('skips non-finite or negative values inside the map', () => {
		const storage = fakeStorage({
			[RESUME_STORAGE_KEY]: '{"demo-track":"soon","second-signal":-4,"third-light":12}'
		});
		const resume = createResumeStore({ storage });

		expect(resume.positionFor('demo-track')).toBeNull();
		expect(resume.positionFor('second-signal')).toBeNull();
		expect(resume.positionFor('third-light')).toBe(12);
	});

	it('a throwing store degrades to nulls and contained writes', () => {
		const resume = createResumeStore({ storage: throwingStorage() });

		expect(resume.positionFor('demo-track')).toBeNull();
		expect(() => resume.save('demo-track', 5)).not.toThrow();
		expect(() => resume.clear('demo-track')).not.toThrow();
	});
});
