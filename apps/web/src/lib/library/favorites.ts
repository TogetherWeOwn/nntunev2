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
 * corrupt values fall back to empty on display reads.
 */

import { favoritesKey, withFavoritesLock, type SnapshotStorage } from './cache.js';

export { clearFavoritesAtAuthBoundary, favoritesKey } from './cache.js';

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

/**
 * Apply an intent to the latest scoped list under its exclusive Web Lock.
 * Unlike the display reader, reject malformed storage rather than repairing it.
 * Verification runs inside the lock, before reading, and may not reacquire it.
 */
export async function updateStoredFavorites(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null,
	key: string | null,
	update: (latest: string[]) => readonly string[],
	verify?: () => Promise<boolean>
): Promise<string[] | null> {
	if (!local || !key || favoritesKey(session) !== key) return null;
	return withFavoritesLock(key, async () => {
		if (favoritesKey(session) !== key) return null;
		if (verify && !(await verify())) return null;
		if (favoritesKey(session) !== key) return null;

		const raw = local.getItem(key);
		const parsed: unknown = raw === null ? [] : JSON.parse(raw);
		if (!Array.isArray(parsed) || !parsed.every(isStoredId)) return null;
		const latest = [...new Set<string>(parsed)];
		const committed = [...new Set(update([...latest]))];
		if (!committed.every(isStoredId) || favoritesKey(session) !== key) return null;
		const encoded = JSON.stringify(committed);
		if (encoded !== JSON.stringify(latest)) local.setItem(key, encoded);
		return committed;
	});
}

/** Legacy Liked route bridge. New writers must use updateStoredFavorites. */
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
