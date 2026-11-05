import { describe, expect, it } from 'vitest';
import { LIBRARY_AUTH_CONTEXT_KEY, type SnapshotStorage } from './cache.js';
import { favoritesKey, loadFavorites, saveFavorites } from './favorites.js';

function memoryStorage(): SnapshotStorage {
	const map = new Map<string, string>();
	return {
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => { map.set(key, value); },
		removeItem: (key) => { map.delete(key); }
	};
}

function fixture() {
	const session = memoryStorage();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'persistence-test');
	const local = memoryStorage();
	return { session, local, key: favoritesKey(session)! };
}

describe('favorites persistence feedback', () => {
	it('reports successful saves and rejected writes', () => {
		const { session, local } = fixture();
		expect(saveFavorites(session, local, ['A'])).toBe(true);
		expect(loadFavorites(session, local)).toEqual(['A']);
		const full = { ...local, setItem: () => { throw new Error('quota'); } };
		expect(saveFavorites(session, full, [])).toBe(false);
		expect(loadFavorites(session, local)).toEqual(['A']);
	});

	it('reports missing storage and auth context as unsaved', () => {
		const { session, local } = fixture();
		expect(saveFavorites(session, null, ['A'])).toBe(false);
		expect(saveFavorites(null, local, ['A'])).toBe(false);
	});

	it('uses a detached memory fallback only when storage cannot be read', () => {
		const { session, local } = fixture();
		const fallback = Object.freeze(['A', 'B']);
		const unreadable = { ...local, getItem: () => { throw new Error('denied'); } };
		expect(loadFavorites(session, unreadable, fallback)).toEqual(fallback);
		expect(loadFavorites(session, unreadable, fallback)).not.toBe(fallback);
		expect(loadFavorites(session, null, fallback)).toEqual(fallback);
	});

	it('respects legitimate empty, missing and corrupt stored lists instead of memory', () => {
		const { session, local, key } = fixture();
		for (const raw of [null, '[]', '{bad json', '{}']) {
			if (raw === null) local.removeItem(key);
			else local.setItem(key, raw);
			expect(loadFavorites(session, local, ['A', 'B'])).toEqual([]);
		}
	});
});
