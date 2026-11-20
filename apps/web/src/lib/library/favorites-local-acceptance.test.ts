import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	invalidateLibraryCache,
	LIBRARY_AUTH_CONTEXT_KEY,
	type SnapshotStorage
} from './cache.js';

/**
 * Acceptance: local-only library favorites honor the library auth boundary.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * favorites implementation on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead of being fixed beside the implementation.
 *
 * Storage contract the build implements (mirrored by the offline
 * `library-favorites-local` browser spec, which pins the same keys):
 * - Favorites persist device-locally in `localStorage` under
 *   `nntune.library.favorites.v1.<auth-context>`, where `<auth-context>` is
 *   the same session `nntune.library.auth-context.v1` value the snapshot
 *   keys in `cache.ts` scope by. Same discipline: reload-safe, and a
 *   rotated/removed context makes the previous account's favorites
 *   unreachable.
 * - The value is a JSON array of non-empty track-id strings.
 * - Crossing the auth boundary (`invalidateLibraryCache`, the entry point
 *   the sign-out/revoke flows already call) clears the scoped favorites key
 *   for the current context plus the legacy unscoped base key, exactly like
 *   it clears the snapshots. A corrupt value is cleared, never thrown on.
 *
 * These tests drive only the existing public entry point
 * (`invalidateLibraryCache` with `vi.stubGlobal('window', …)`, the same
 * harness `cache.test.ts` uses), so RED here means the implementation is
 * missing — never a missing import.
 */

const FAVORITES_KEY_BASE = 'nntune.library.favorites.v1';

function scopedFavoritesKey(context: string): string {
	return `${FAVORITES_KEY_BASE}.${context}`;
}

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

function installWindow(): { session: SnapshotStorage; local: SnapshotStorage } {
	const session = memoryStorage();
	const local = memoryStorage();
	vi.stubGlobal('window', { sessionStorage: session, localStorage: local });
	return { session, local };
}

describe('favorites auth-boundary clearing', () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it('[FAV-U01] crossing the auth boundary clears the scoped favorites key', () => {
		const { session, local } = installWindow();
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'account-a');
		local.setItem(scopedFavoritesKey('account-a'), JSON.stringify(['track-a', 'track-b']));

		invalidateLibraryCache();

		expect(
			local.getItem(scopedFavoritesKey('account-a')),
			'scoped favorites must not survive the auth boundary'
		).toBeNull();
		expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	});

	it('[FAV-U02] crossing the auth boundary clears the legacy unscoped favorites key', () => {
		const { session, local } = installWindow();
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'account-a');
		local.setItem(FAVORITES_KEY_BASE, JSON.stringify(['track-legacy']));

		invalidateLibraryCache();

		expect(
			local.getItem(FAVORITES_KEY_BASE),
			'legacy unscoped favorites must not survive the auth boundary'
		).toBeNull();
		expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	});

	it('[FAV-U03] a corrupt favorites value is cleared without throwing', () => {
		const { session, local } = installWindow();
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'account-a');
		local.setItem(scopedFavoritesKey('account-a'), '{not-json');

		expect(() => invalidateLibraryCache()).not.toThrow();
		expect(
			local.getItem(scopedFavoritesKey('account-a')),
			'corrupt favorites must not survive the auth boundary'
		).toBeNull();
	});
});
