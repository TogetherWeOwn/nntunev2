import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	invalidateLibraryCache,
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_RECENT_FINDS_KEY,
	type SnapshotStorage
} from './cache.js';
import {
	clearRecentFindsAtAuthBoundary,
	loadRecentFinds,
	MAX_RECENT_FIND_LENGTH,
	MAX_RECENT_FINDS,
	recordRecentFind,
	recentFindsKey,
	saveRecentFinds
} from './recent-finds.js';

/**
 * Builder-owned pins for device-local recent library find queries. The
 * paired acceptance bank (`tests/library-recent-find.spec.ts`) stays
 * byte-identical; this file covers review-found gaps around the length
 * bound, stored-array filtering and the auth-boundary purge entry points.
 */

function memoryStorage(seed: Record<string, string> = {}): SnapshotStorage {
	const map = new Map(Object.entries(seed));
	return {
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => {
			map.set(key, value);
		},
		removeItem: (key) => {
			map.delete(key);
		}
	};
}

function scopedKey(context: string): string {
	return `${LIBRARY_RECENT_FINDS_KEY}.${context}`;
}

function installWindow(): { session: SnapshotStorage; local: SnapshotStorage } {
	const session = memoryStorage();
	const local = memoryStorage();
	vi.stubGlobal('window', { sessionStorage: session, localStorage: local });
	return { session, local };
}

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('bounds', () => {
	it('keeps 5 queries of at most 120 chars, the contract the banks pin', () => {
		expect(MAX_RECENT_FINDS).toBe(5);
		expect(MAX_RECENT_FIND_LENGTH).toBe(120);
	});

	it('records a 120-char query but rejects a 121-char one', () => {
		const allowed = 'q'.repeat(120);
		expect(recordRecentFind([], allowed)).toEqual([allowed]);
		const list = ['beta'];
		expect(recordRecentFind(list, 'q'.repeat(121))).toBe(list);
	});
});

describe('recordRecentFind', () => {
	it('trims, dedupes to the front and evicts the oldest past the cap', () => {
		expect(recordRecentFind([], '  beta  ')).toEqual(['beta']);
		expect(recordRecentFind(['beta', 'gamma'], 'gamma')).toEqual(['gamma', 'beta']);
		const full = ['one', 'two', 'three', 'four', 'five'];
		expect(recordRecentFind(full, 'six')).toEqual(['six', 'one', 'two', 'three', 'four']);
	});

	it('returns the same list for empty, non-string or already-first queries', () => {
		const list = ['beta'];
		expect(recordRecentFind(list, '')).toBe(list);
		expect(recordRecentFind(list, '   ')).toBe(list);
		expect(recordRecentFind(list, 42)).toBe(list);
		expect(recordRecentFind(list, 'beta')).toBe(list);
	});
});

describe('loadRecentFinds', () => {
	it('filters a short stored array to valid distinct trimmed strings', () => {
		const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: 'account-a' });
		const local = memoryStorage({
			[scopedKey('account-a')]: JSON.stringify([
				'beta',
				'beta',
				' padded ',
				'',
				42,
				'x'.repeat(121)
			])
		});

		// 6 raw entries would read as empty via the over-cap rule; the
		// filter contract below applies to arrays of 5 or fewer.
		expect(loadRecentFinds(session, local)).toEqual([]);
		local.setItem(scopedKey('account-a'), JSON.stringify(['beta', 'beta', ' padded ', '', 42]));
		expect(loadRecentFinds(session, local)).toEqual(['beta']);
		local.setItem(
			scopedKey('account-a'),
			JSON.stringify(['gamma', 'beta', 'gamma', 'x'.repeat(121)])
		);
		expect(loadRecentFinds(session, local)).toEqual(['gamma', 'beta']);
	});

	it('reads corrupt, foreign, over-cap or missing payloads as empty', () => {
		const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: 'account-a' });
		const local = memoryStorage();
		const key = scopedKey('account-a');
		expect(loadRecentFinds(session, local)).toEqual([]);
		for (const raw of ['{not-json', '{"q":"beta"}', JSON.stringify(['1', '2', '3', '4', '5', '6'])]) {
			local.setItem(key, raw);
			expect(loadRecentFinds(session, local)).toEqual([]);
		}
	});

	it('reads as empty without storage or without an auth context', () => {
		const local = memoryStorage({ [scopedKey('account-a')]: JSON.stringify(['beta']) });
		expect(loadRecentFinds(null, local)).toEqual([]);
		expect(loadRecentFinds(memoryStorage(), local)).toEqual([]);
		expect(loadRecentFinds(memoryStorage(), null)).toEqual([]);
	});
});

describe('saveRecentFinds', () => {
	it('persists valid queries and round-trips through load', () => {
		const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: 'account-a' });
		const local = memoryStorage();
		saveRecentFinds(session, local, ['gamma', 'beta']);
		expect(loadRecentFinds(session, local)).toEqual(['gamma', 'beta']);
	});

	it('drops invalid entries instead of persisting them', () => {
		const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: 'account-a' });
		const local = memoryStorage();
		saveRecentFinds(session, local, ['beta', ' padded ', '', 'x'.repeat(121)]);
		expect(JSON.parse(local.getItem(scopedKey('account-a')) ?? 'null')).toEqual(['beta']);
	});

	it('is a no-op without storage or without an auth context', () => {
		const local = memoryStorage();
		expect(() => saveRecentFinds(null, local, ['beta'])).not.toThrow();
		expect(() => saveRecentFinds(memoryStorage(), local, ['beta'])).not.toThrow();
		expect(() => saveRecentFinds(memoryStorage(), null, ['beta'])).not.toThrow();
		expect(local.getItem(scopedKey('account-a'))).toBeNull();
	});
});

describe('recentFindsKey', () => {
	it('scopes to the session auth context and nulls without one', () => {
		expect(recentFindsKey(null)).toBeNull();
		expect(recentFindsKey(memoryStorage())).toBeNull();
		expect(
			recentFindsKey(memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: 'account-a' }))
		).toBe(scopedKey('account-a'));
	});
});

describe('auth-boundary purge', () => {
	it('clearRecentFindsAtAuthBoundary removes the scoped and legacy keys only', () => {
		const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: 'account-a' });
		const local = memoryStorage({
			[scopedKey('account-a')]: JSON.stringify(['beta']),
			[LIBRARY_RECENT_FINDS_KEY]: JSON.stringify(['stale-legacy']),
			'test.unrelated': 'keep'
		});

		clearRecentFindsAtAuthBoundary(session, local);

		expect(local.getItem(scopedKey('account-a'))).toBeNull();
		expect(local.getItem(LIBRARY_RECENT_FINDS_KEY)).toBeNull();
		expect(local.getItem('test.unrelated')).toBe('keep');
	});

	it('clearRecentFindsAtAuthBoundary never throws', () => {
		expect(() => clearRecentFindsAtAuthBoundary(null, null)).not.toThrow();
		expect(() => clearRecentFindsAtAuthBoundary(null, memoryStorage())).not.toThrow();
	});

	it('invalidateLibraryCache purges recents when Library is not mounted', () => {
		const { session, local } = installWindow();
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'account-a');
		local.setItem(scopedKey('account-a'), JSON.stringify(['beta']));
		local.setItem(LIBRARY_RECENT_FINDS_KEY, JSON.stringify(['stale-legacy']));
		local.setItem('test.unrelated', 'keep');

		invalidateLibraryCache();

		expect(
			local.getItem(scopedKey('account-a')),
			'scoped recents must not survive the auth boundary'
		).toBeNull();
		expect(local.getItem(LIBRARY_RECENT_FINDS_KEY)).toBeNull();
		expect(local.getItem('test.unrelated')).toBe('keep');
		expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	});
});
