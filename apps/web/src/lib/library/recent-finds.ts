/**
 * Device-local recent library find queries.
 *
 * The last few applied find queries persist in `localStorage` under
 * `nntune.library.recent-finds.v1.<auth-context>` as a JSON array of at
 * most `MAX_RECENT_FINDS` distinct non-empty trimmed strings, most recent
 * first, where `<auth-context>` is the session
 * `nntune.library.auth-context.v1` value — the same scoping discipline as
 * the favorites keys in `favorites.ts`. The bare base key is the legacy
 * unscoped value, cleared (never read) at the auth boundary.
 *
 * Reads fail closed: missing storage, a missing context, corrupt JSON, a
 * non-array payload or an over-cap payload (more than `MAX_RECENT_FINDS`
 * raw entries) all read as empty. Nothing here throws, fetches or touches
 * any other key.
 */

import {
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_RECENT_FINDS_KEY,
	type SnapshotStorage
} from './cache.js';

/** Most queries kept; the next distinct query evicts the oldest. */
export const MAX_RECENT_FINDS = 5;
/** Longest query worth remembering; longer text is not recorded or read. */
export const MAX_RECENT_FIND_LENGTH = 120;

/** Scoped recent-finds key for the current auth context, or null without one. */
export function recentFindsKey(session: SnapshotStorage | null): string | null {
	if (!session) return null;
	try {
		const context = session.getItem(LIBRARY_AUTH_CONTEXT_KEY);
		return context ? `${LIBRARY_RECENT_FINDS_KEY}.${context}` : null;
	} catch {
		return null;
	}
}

function isStoredQuery(value: unknown): value is string {
	return (
		typeof value === 'string' &&
		value.length > 0 &&
		value === value.trim() &&
		value.length <= MAX_RECENT_FIND_LENGTH
	);
}

/**
 * Pure: the list after recording `query` (trimmed). An empty,
 * whitespace-only, non-string or over-long query returns the same list
 * reference; an already-first query also returns it unchanged. Otherwise
 * the query moves to the front without a duplicate and the list is capped.
 */
export function recordRecentFind(list: readonly string[], query: unknown): readonly string[] {
	if (typeof query !== 'string') return list;
	const trimmed = query.trim();
	if (!isStoredQuery(trimmed)) return list;
	if (list[0] === trimmed) return list;
	return [trimmed, ...list.filter((entry) => entry !== trimmed)].slice(0, MAX_RECENT_FINDS);
}

/** Load the current context's recent finds; every failure reads as empty. */
export function loadRecentFinds(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null
): string[] {
	const key = recentFindsKey(session);
	if (!local || !key) return [];
	try {
		const raw = local.getItem(key);
		if (!raw) return [];
		const parsed: unknown = JSON.parse(raw);
		if (!Array.isArray(parsed) || parsed.length > MAX_RECENT_FINDS) return [];
		return [...new Set(parsed.filter(isStoredQuery))];
	} catch {
		return [];
	}
}

/** Persist the current context's recent finds; a no-op without storage/context. */
export function saveRecentFinds(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null,
	queries: readonly string[]
): void {
	const key = recentFindsKey(session);
	if (!local || !key) return;
	try {
		local.setItem(key, JSON.stringify(queries.filter(isStoredQuery).slice(0, MAX_RECENT_FINDS)));
	} catch {
		// Quota or privacy mode: memory remains authoritative this visit.
	}
}

/**
 * Clear recent finds at the auth boundary: the scoped key for the outgoing
 * context plus the legacy unscoped base key. Call BEFORE the refresher's
 * transition handler revokes the session auth context — afterwards the
 * scoped key is unknowable. Never throws.
 */
export function clearRecentFindsAtAuthBoundary(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null
): void {
	if (!local) return;
	const key = recentFindsKey(session);
	try {
		if (key) local.removeItem(key);
		local.removeItem(LIBRARY_RECENT_FINDS_KEY);
	} catch {
		// Best effort; a key without its auth context is never read.
	}
}
