import { describe, expect, it } from 'vitest';
import * as favorites from './favorites.js';
import { LIBRARY_AUTH_CONTEXT_KEY, type SnapshotStorage } from './cache.js';

/**
 * Acceptance-owned: implement the paired feature without editing this file.
 *
 * Minimal new pure seam on the existing favorites module:
 * selectFavoriteItems<T extends { id: string }>(
 *   orderedItems: readonly T[], favoriteIds: readonly string[]
 * ): T[]
 * Return an independent array of the loaded rows whose stable IDs are starred,
 * retaining row identity and input order; never mutate either input. No sorting,
 * storage, queue or network side effects belong in this projection.
 *
 * Reflect is only an assertion-first admission check for the absent export:
 * baseline RED is an explanatory assertion, not a module/import/compiler error.
 * Clear uses the EXISTING saveFavorites(session, local, []) boundary; no new
 * persistence schema, clearing API or test-only production method is requested.
 * Browser tests prove the UI actually wires these behaviors together.
 *
 * Breaks caught: returning all rows; matching by title/index; ordering by saved
 * IDs; mutating/sorting inputs; dropping hidden IDs during save; clearing other
 * accounts; throwing on unavailable storage; retrying from stale stored state.
 */
type Row = { id: string; title?: string; artist?: string };
type SelectFavorites = <T extends { id: string }>(
	orderedItems: readonly T[], favoriteIds: readonly string[]
) => T[];

function select(ordered: readonly Row[], ids: readonly string[]): Row[] {
	const candidate = Reflect.get(favorites, 'selectFavoriteItems');
	expect(candidate, 'favorites-only needs the pure selectFavoriteItems projection').toBeTypeOf('function');
	return (candidate as SelectFavorites)(ordered, ids);
}

function memoryStorage(seed: Record<string, string> = {}): SnapshotStorage {
	const values = new Map(Object.entries(seed));
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => { values.set(key, value); },
		removeItem: (key) => { values.delete(key); }
	};
}

const BASE = 'nntune.library.favorites.v1';
const KEY = `${BASE}.management-account`;
function storage(ids: readonly string[]) {
	const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: 'management-account' });
	const local = memoryStorage({
		[KEY]: JSON.stringify(ids),
		[`${BASE}.other-account`]: '["other-private-id"]',
		'unrelated.preference': 'unchanged'
	});
	return { session, local };
}

const ZULU = Object.freeze({ id: 'manage-zulu', title: 'Same title', artist: 'Beta' });
const ALPHA = Object.freeze({ id: 'manage-alpha', title: 'Same title', artist: 'Zulu' });
const MIKE = Object.freeze({ id: 'manage-mike', title: 'Mike Song', artist: 'Alpha' });

const cases: { id: string; label: string; rows: readonly Row[]; ids: readonly string[]; want: string[] }[] = [
	{ id: 'FM-U01', label: 'empty library and favorites', rows: [], ids: [], want: [] },
	{ id: 'FM-U02', label: 'no favorites among many loaded rows', rows: [ZULU, ALPHA, MIKE], ids: [], want: [] },
	{ id: 'FM-U03', label: 'single match uses ID, not identical title', rows: [ZULU, ALPHA], ids: ['manage-alpha'], want: ['manage-alpha'] },
	{ id: 'FM-U04', label: 'subset retains supplied title ordering, not stored-ID order', rows: [MIKE, ALPHA, ZULU], ids: ['manage-zulu', 'manage-alpha'], want: ['manage-alpha', 'manage-zulu'] },
	{ id: 'FM-U05', label: 'duplicates and absent IDs never create extra loaded rows', rows: [ZULU, ALPHA, MIKE], ids: ['absent-id', 'manage-mike', 'manage-mike', 'manage-zulu'], want: ['manage-zulu', 'manage-mike'] },
	{ id: 'FM-U06', label: 'hidden-only IDs match nothing', rows: [ZULU], ids: ['absent-id'], want: [] },
	{ id: 'FM-U07', label: 'all matches retain supplied artist ordering', rows: [MIKE, ZULU, ALPHA], ids: ['manage-alpha', 'manage-zulu', 'manage-mike'], want: ['manage-mike', 'manage-zulu', 'manage-alpha'] },
	{ id: 'FM-U08', label: 'single loaded row and duplicate favorite ID', rows: [MIKE], ids: ['manage-mike', 'manage-mike'], want: ['manage-mike'] }
];

describe('favorites-management immutable projection', () => {
	for (const { id, label, rows, ids, want } of cases) {
		it(`[${id}] ${label}`, () => {
			const ordered = Object.freeze([...rows]);
			const starred = Object.freeze([...ids]);
			const projected = select(ordered, starred);
			expect(projected.map((item) => item.id)).toEqual(want);
			expect(ordered).toEqual(rows);
			expect(starred).toEqual(ids);
			expect(projected).not.toBe(ordered);
			for (const item of projected) expect(item).toBe(rows.find((row) => row.id === item.id));
			projected.splice(0, projected.length);
			expect(ordered).toEqual(rows);
			expect(starred).toEqual(ids);
		});
	}
});

describe('favorites-management existing persistence controls', () => {
	for (const ids of [[], ['manage-zulu'], ['manage-zulu', 'hidden-id', 'manage-zulu', 'stale-id']]) {
		it(`[FM-U09:${ids.length}] confirmed clear saves the entire empty scoped state without changing auth or other keys`, () => {
			const { session, local } = storage(ids);
			const original = Object.freeze([...ids]);
			favorites.saveFavorites(session, local, []);
			expect(favorites.loadFavorites(session, local)).toEqual([]);
			expect(JSON.parse(local.getItem(KEY) as string)).toEqual([]);
			expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBe('management-account');
			expect(local.getItem(`${BASE}.other-account`)).toBe('["other-private-id"]');
			expect(local.getItem('unrelated.preference')).toBe('unchanged');
			expect(original).toEqual(ids);
		});
	}

	it('[FM-U10] saving an unstarred full state retains hidden IDs and ignores stale persisted membership', () => {
		const { session, local } = storage(['manage-zulu', 'hidden-id', 'manage-mike']);
		favorites.saveFavorites(session, local, Object.freeze(['hidden-id', 'manage-mike', 'manage-alpha']));
		expect(favorites.loadFavorites(session, local)).toEqual(['hidden-id', 'manage-mike', 'manage-alpha']);
	});

	it('[FM-U11] write refusal is best effort; a later full-state save does not resurrect cleared IDs', () => {
		const { session, local } = storage(['manage-zulu', 'hidden-id']);
		const unavailable: SnapshotStorage = {
			...local, setItem: () => { throw new Error('synthetic quota refusal'); }
		};
		expect(() => favorites.saveFavorites(session, unavailable, [])).not.toThrow();
		expect(favorites.loadFavorites(session, local)).toEqual(['manage-zulu', 'hidden-id']);
		favorites.saveFavorites(session, local, Object.freeze(['manage-alpha']));
		expect(favorites.loadFavorites(session, local)).toEqual(['manage-alpha']);
	});

	it('[FM-U12] unavailable reads fall back safely; later full-state saves still work', () => {
		const { session, local } = storage(['hidden-id']);
		const unavailable: SnapshotStorage = {
			...local, getItem: () => { throw new Error('synthetic privacy refusal'); }
		};
		expect(favorites.loadFavorites(session, unavailable)).toEqual([]);
		expect(() => favorites.saveFavorites(session, unavailable, [])).not.toThrow();
		expect(favorites.loadFavorites(session, local)).toEqual([]);
	});
});
