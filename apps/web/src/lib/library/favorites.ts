/**
 * Device-local library favorites.
 *
 * Starred track ids persist in `localStorage` under
 * `nntune.library.favorites.v1.<auth-context>` as a JSON array of non-empty
 * strings, where `<auth-context>` is the session
 * `nntune.library.auth-context.v1` value — the same scoping discipline as
 * the snapshot keys in `cache.ts`. Reload-safe (the context is tab-scoped
 * session state), never shared across accounts (a rotated or removed
 * context makes the previous account's favorites unreachable), and
 * corrupt values fall back to empty without throwing. No network use:
 * every helper touches Web Storage only.
 */

import {
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_FAVORITES_KEY,
	type SnapshotStorage
} from './cache.js';

/** Scoped favorites key for the current auth context, or null without one. */
export function favoritesKey(session: SnapshotStorage | null): string | null {
	if (!session) return null;
	try {
		const context = session.getItem(LIBRARY_AUTH_CONTEXT_KEY);
		return context ? `${LIBRARY_FAVORITES_KEY}.${context}` : null;
	} catch {
		return null;
	}
}

function isStoredId(value: unknown): value is string {
	return typeof value === 'string' && value.length > 0;
}

/**
 * Load the current context's favorites. Missing or corrupt stored values
 * yield an empty list. Unavailable storage/context also defaults to empty;
 * a caller may supply in-memory state to use when storage cannot be read.
 */
export function loadFavorites(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null,
	fallback: readonly string[] = []
): string[] {
	const key = favoritesKey(session);
	if (!local || !key) return [...fallback];
	let raw: string | null;
	try {
		raw = local.getItem(key);
	} catch {
		return [...fallback];
	}
	try {
		if (!raw) return [];
		const parsed: unknown = JSON.parse(raw);
		if (!Array.isArray(parsed)) return [];
		return [...new Set(parsed.filter(isStoredId))];
	} catch {
		return [];
	}
}

/** Persist favorites, returning whether storage accepted the current set. */
export function saveFavorites(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null,
	ids: readonly string[]
): boolean {
	const key = favoritesKey(session);
	if (!local || !key) return false;
	try {
		local.setItem(key, JSON.stringify(ids.filter(isStoredId)));
		return true;
	} catch {
		// Quota or privacy mode: memory remains authoritative this visit.
		// A later toggle retries the full set; reload reads the last saved set.
		return false;
	}
}

/**
 * Project the loaded rows to the starred subset for the transient
 * Favorites-only view. Pure: matches by stable id against the already
 * ordered rows, keeps input order and row identity, and returns an
 * independent array. Never mutates, sorts, stores or fetches.
 */
export function selectFavoriteItems<T extends { id: string }>(
	orderedItems: readonly T[],
	favoriteIds: readonly string[]
): T[] {
	const starred = new Set(favoriteIds);
	return orderedItems.filter((item) => starred.has(item.id));
}

/**
 * Clear favorites at the auth boundary: the scoped key for the outgoing
 * context plus the legacy unscoped base key. Call BEFORE the refresher's
 * transition handler revokes the session auth context — afterwards the
 * scoped key is unknowable. Never throws.
 */
export function clearFavoritesAtAuthBoundary(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null
): void {
	if (!local) return;
	let key: string | null = null;
	try {
		const context = session?.getItem(LIBRARY_AUTH_CONTEXT_KEY) ?? null;
		key = context ? `${LIBRARY_FAVORITES_KEY}.${context}` : null;
	} catch {
		key = null;
	}
	try {
		if (key) local.removeItem(key);
		local.removeItem(LIBRARY_FAVORITES_KEY);
	} catch {
		// Best effort; a key without its auth context is never read.
	}
}
