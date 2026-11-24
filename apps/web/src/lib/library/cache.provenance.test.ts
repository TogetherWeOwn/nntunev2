import { afterEach, expect, it, vi } from 'vitest';

import {
	clearSnapshot,
	establishVerifiedLibraryScope,
	libraryScopeBinding,
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_SESSION_BINDING_KEY,
	loadSnapshot,
	saveSnapshot,
	scopeLibraryStorage,
	verifiedFavoritesKey
} from './cache.js';
import { favoritesKey, loadFavorites } from './favorites.js';

const context = '9cd1c49f-eda8-44e8-8a35-edabed800ce0';
const key = `nntune.library.favorites.v1.${context}`;
const binding = 'A'.repeat(43);
const otherBinding = 'E'.repeat(43);
const bytes = ' [ "track" ] ';

function memoryStorage() {
	const values = new Map<string, string>();
	return {
		getItem: (key: string) => values.get(key) ?? null,
		setItem: (key: string, value: string) => {
			values.set(key, value);
		},
		removeItem: (key: string) => {
			values.delete(key);
		}
	};
}

function fixture() {
	const local = memoryStorage();
	const session = memoryStorage();
	vi.stubGlobal('window', { localStorage: local });
	vi.stubGlobal('crypto', { randomUUID: () => context });
	return { local, session };
}

afterEach(() => vi.unstubAllGlobals());

it('returns an in-memory snapshot when context creation is disabled and no scope exists', () => {
	const { session } = fixture();
	const items = [{ id: 'track' }];
	const savedAt = '2026-01-01T00:00:00.000Z';

	expect(saveSnapshot(session, items, Date.parse(savedAt), false)).toEqual({ savedAt, items });
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	expect(loadSnapshot(session)).toBeNull();
});

it.each([
	{ name: 'a different auth context', tuple: ['ae44027c-2735-4fca-8e49-500579db79d1', binding] },
	{ name: 'an extra tuple element', tuple: [context, binding, 'extra'] }
])('rejects stored provenance with $name even when the binding matches', ({ tuple }) => {
	const { session } = fixture();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, context);
	session.setItem(LIBRARY_SESSION_BINDING_KEY, JSON.stringify(tuple));

	expect(libraryScopeBinding(session)).toBeNull();
	expect(verifiedFavoritesKey(session, binding)).toBeNull();
});

it('never binds an existing unbound scope or changes its exact favorites bytes', () => {
	const { local, session } = fixture();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, context);
	local.setItem(key, bytes);

	expect(establishVerifiedLibraryScope(session, binding)).toBeUndefined();
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBe(context);
	expect(session.getItem(LIBRARY_SESSION_BINDING_KEY)).toBeNull();
	expect(libraryScopeBinding(session)).toBeNull();
	expect(verifiedFavoritesKey(session, binding)).toBeNull();
	expect(local.getItem(key)).toBe(bytes);
});

it.each(['account retirement', 'context replacement'])('does not rederive a storage key after %s', (boundary) => {
	const { local, session } = fixture();
	expect(establishVerifiedLibraryScope(session, binding)).toBe(key);
	local.setItem(key, bytes);
	const controller = new AbortController();
	const { signal } = controller;
	const owner = { generation: 'fixture', signal, isCurrent: () => !signal.aborted, run: async <T>(work: () => T) => work() };
	const scoped = scopeLibraryStorage(session, owner)!;
	expect(favoritesKey(scoped)).toBe(key);
	expect(loadFavorites(scoped, local)).toEqual(['track']);

	if (boundary === 'account retirement') controller.abort();
	else session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'replacement');
	expect(favoritesKey(scoped)).toBeNull();
	expect(verifiedFavoritesKey(scoped, binding)).toBeNull();
	expect(loadFavorites(scoped, local)).toEqual([]);
	expect(() => scoped.setItem(LIBRARY_AUTH_CONTEXT_KEY, context)).toThrow('Library storage scope ended');
	expect(local.getItem(key)).toBe(bytes);
});

it.each([LIBRARY_SESSION_BINDING_KEY, LIBRARY_AUTH_CONTEXT_KEY])(
	'fails closed when writing %s, then recovers and retires a verified scope',
	(failedKey) => {
		const { local, session } = fixture();
		const setItem = session.setItem;
		const storageFault = vi.spyOn(session, 'setItem').mockImplementation((key, value) => {
			if (key === failedKey) throw new DOMException('Storage full', 'QuotaExceededError');
			setItem(key, value);
		});

		expect(establishVerifiedLibraryScope(session, binding)).toBeUndefined();
		expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
		expect(libraryScopeBinding(session)).toBeNull();
		expect(verifiedFavoritesKey(session, binding)).toBeNull();

		storageFault.mockRestore();
		expect(establishVerifiedLibraryScope(session, binding)).toBe(key);
		expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBe(context);
		expect(session.getItem(LIBRARY_SESSION_BINDING_KEY)).toBe(JSON.stringify([context, binding]));
		expect(libraryScopeBinding(session)).toBe(binding);

		local.setItem(key, bytes);
		expect(verifiedFavoritesKey(session, otherBinding)).toBeNull();
		expect(establishVerifiedLibraryScope(session, otherBinding)).toBeUndefined();
		expect(verifiedFavoritesKey(session, binding)).toBe(key);
		expect(establishVerifiedLibraryScope(session, binding)).toBe(key);
		expect(local.getItem(key)).toBe(bytes);

		clearSnapshot(session);
		expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
		expect(session.getItem(LIBRARY_SESSION_BINDING_KEY)).toBeNull();
		expect(libraryScopeBinding(session)).toBeNull();
		expect(verifiedFavoritesKey(session, binding)).toBeNull();
		expect(establishVerifiedLibraryScope(session, binding)).toBeUndefined();
		expect(local.getItem(key)).toBe(bytes);
	}
);
