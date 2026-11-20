import { afterEach, describe, expect, it, vi } from 'vitest';
import { clearSnapshot, LIBRARY_AUTH_CONTEXT_KEY, type SnapshotStorage } from './cache.js';
import { clearFavoritesAtAuthBoundary, favoritesKey, loadFavorites, saveFavorites, updateStoredFavorites } from './favorites.js';

afterEach(() => vi.unstubAllGlobals());

function installLocks() {
	const tails = new Map<string, Promise<unknown>>();
	vi.stubGlobal('navigator', { locks: {
		request: (key: string, { mode }: LockOptions, run: LockGrantedCallback<unknown>) => {
			expect(mode).toBe('exclusive');
			const done = (tails.get(key) ?? Promise.resolve()).then(() => run({ name: key, mode: 'exclusive' }));
			tails.set(key, done.catch(() => {}));
			return done;
		}
	} });
}

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

describe('coordinated favorites mutations', () => {
	it('queues a competing tab behind retirement verification and rebases its edit', async () => {
		installLocks();
		const { session, local, key } = fixture();
		local.setItem(key, '["retired","keep"]');
		const getItem = vi.fn(local.getItem);
		expect(await updateStoredFavorites(session, { ...local, getItem }, key, () => [], async () => false)).toBeNull();
		const verification = Promise.withResolvers<boolean>();
		const retirement = updateStoredFavorites(session, { ...local, getItem }, key,
			(ids) => ids.filter((id) => id !== 'retired'), () => verification.promise);
		const add = vi.fn((ids: string[]) => [...ids, 'new']);
		const competing = updateStoredFavorites(fixture().session, local, key, add);
		await Promise.resolve();
		expect(getItem).not.toHaveBeenCalled();
		expect(add).not.toHaveBeenCalled();
		verification.resolve(true);
		expect(await retirement).toEqual(['keep']);
		expect(await competing).toEqual(['keep', 'new']);
	});

	it('revokes a holding writer before purging its outgoing scope', async () => {
		installLocks();
		const { session, local, key } = fixture();
		local.setItem(key, '["old"]');
		const verification = Promise.withResolvers<boolean>();
		const holding = updateStoredFavorites(session, local, key, () => ['unexpected'], () => verification.promise);
		await Promise.resolve(); // Enter verification before revoking the scope.
		const purge = clearFavoritesAtAuthBoundary(session, local);
		clearSnapshot(session);
		expect(local.getItem(key)).toBe('["old"]');
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'next-account');
		expect(await updateStoredFavorites(session, local, favoritesKey(session), () => ['new'])).toEqual(['new']);
		verification.resolve(true);
		expect(await holding).toBeNull();
		await purge;
		expect(local.getItem(key)).toBeNull();
		expect(loadFavorites(session, local)).toEqual(['new']);
	});

	it('leaves malformed and unreadable storage untouched while keeping reads permissive', async () => {
		installLocks();
		const { session, local, key } = fixture();
		const clear = vi.fn(() => []);
		for (const raw of ['', '{bad json', '{}', '["keep",null]', '["keep",""]']) {
			local.setItem(key, raw);
			expect(await updateStoredFavorites(session, local, key, clear)).toBeNull();
			expect(local.getItem(key)).toBe(raw);
		}
		expect(loadFavorites(session, local)).toEqual(['keep']);
		const unreadable = { ...local, getItem: () => { throw new Error('denied'); } };
		expect(await updateStoredFavorites(session, unreadable, key, clear)).toBeNull();
		expect(clear).not.toHaveBeenCalled();
		expect(local.getItem(key)).toBe('["keep",""]');
	});

	it('fails closed for edits but still purges when locks are unavailable', async () => {
		const { session, local, key } = fixture();
		for (const locks of [undefined, { request: () => Promise.reject(new Error('lock denied')) }]) {
			vi.stubGlobal('navigator', { locks });
			local.setItem(key, '["old"]');
			expect(await updateStoredFavorites(session, local, key, () => ['new'])).toBeNull();
			expect(local.getItem(key)).toBe('["old"]');
			await clearFavoritesAtAuthBoundary(session, local);
			expect(local.getItem(key)).toBeNull();
		}
	});
});
