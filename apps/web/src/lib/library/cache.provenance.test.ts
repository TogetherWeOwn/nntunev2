import { afterEach, expect, it, vi } from 'vitest';

import {
	clearSnapshot,
	establishVerifiedLibraryScope,
	libraryScopeBinding,
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_SESSION_BINDING_KEY,
	verifiedFavoritesKey
} from './cache.js';

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
