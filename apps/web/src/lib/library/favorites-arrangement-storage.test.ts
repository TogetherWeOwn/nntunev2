import { describe, expect, it } from 'vitest';
import { LIBRARY_AUTH_CONTEXT_KEY, type SnapshotStorage } from './cache.js';
import { clearFavoritesAtAuthBoundary, loadFavorites, saveFavorites } from './favorites.js';

/**
 * Acceptance-owned ordered-persistence guards, not a test-local move implementation.
 * Baseline has no pure favorite-move export. Movement RED lives in the browser spec;
 * these controls protect the existing save/load seam the builder must reuse.
 */
function memoryStorage(): SnapshotStorage {
	const values = new Map<string, string>();
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => { values.set(key, value); },
		removeItem: (key) => { values.delete(key); }
	};
}
function sessionFor(context: string): SnapshotStorage {
	const session = memoryStorage();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, context);
	return session;
}
const KEY = 'nntune.library.favorites.v1.account-a';

describe('favorites arrangement ordered storage controls', () => {
	it.each([
		{ name: 'empty', ids: [] },
		{ name: 'singleton', ids: ['arr-m'] },
		{ name: 'many-with-hidden-slots', ids: ['hidden-first', 'arr-m', 'hidden-between', 'arr-z', 'arr-a', 'hidden-last'] }
	])('[FAVORDER-S01:$name] round-trips exact order without mutating a frozen caller list', ({ ids }) => {
		const session = sessionFor('account-a');
		const local = memoryStorage();
		const input = Object.freeze([...ids]);
		saveFavorites(session, local, input);
		expect(JSON.parse(local.getItem(KEY)!)).toEqual(ids);
		expect(loadFavorites(session, local)).toEqual(ids);
		expect(input).toEqual(ids);
		expect(loadFavorites(session, local)).not.toBe(input);
	});

	it('[FAVORDER-S02] a failed write followed by a full-state save replaces stale order and retains hidden membership', () => {
		const session = sessionFor('account-a');
		const local = memoryStorage();
		local.setItem(KEY, JSON.stringify(['arr-z', 'hidden', 'arr-a', 'arr-m']));
		const unavailable: SnapshotStorage = {
			getItem: local.getItem, removeItem: local.removeItem,
			setItem: () => { throw new Error('quota'); }
		};
		expect(() => saveFavorites(session, unavailable, Object.freeze(['arr-z', 'hidden', 'arr-m', 'arr-a']))).not.toThrow();
		expect(loadFavorites(session, local)).toEqual(['arr-z', 'hidden', 'arr-a', 'arr-m']);
		saveFavorites(session, local, Object.freeze(['arr-m', 'hidden', 'arr-z', 'arr-a']));
		expect(loadFavorites(session, local)).toEqual(['arr-m', 'hidden', 'arr-z', 'arr-a']);
	});

	it('[FAVORDER-S03] loading yields independent lists and never changes persisted order through returned-array mutation', () => {
		const session = sessionFor('account-a');
		const local = memoryStorage();
		local.setItem(KEY, '["arr-m","hidden","arr-z","arr-a"]');
		const first = loadFavorites(session, local);
		first.reverse();
		first.pop();
		expect(loadFavorites(session, local)).toEqual(['arr-m', 'hidden', 'arr-z', 'arr-a']);
		expect(local.getItem(KEY)).toBe('["arr-m","hidden","arr-z","arr-a"]');
	});

	it('[FAVORDER-S04] outgoing arranged order is cleared before a new context can bootstrap', () => {
		const session = sessionFor('account-a');
		const local = memoryStorage();
		saveFavorites(session, local, ['arr-m', 'hidden', 'arr-z', 'arr-a']);
		clearFavoritesAtAuthBoundary(session, local);
		expect(local.getItem(KEY)).toBeNull();
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'account-b');
		expect(loadFavorites(session, local)).toEqual([]);
		saveFavorites(session, local, ['arr-a', 'arr-z']);
		expect(local.getItem('nntune.library.favorites.v1.account-b')).toBe('["arr-a","arr-z"]');
		expect(local.getItem(KEY)).toBeNull();
	});
});
