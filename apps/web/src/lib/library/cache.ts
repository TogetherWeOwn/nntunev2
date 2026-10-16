/**
 * Offline-friendly library snapshot cache.
 *
 * The library view renders a last-good `/api/library` payload from
 * tab-scoped `sessionStorage` after authentication succeeds in the current
 * page. A fresh success always replaces the snapshot. The badge text always
 * starts with `stale — updated` so the journey specs can pin it by substring.
 *
 * Bounded growth: the snapshot is capped by item count AND serialized
 * bytes. Eviction keeps the head of the list (the API's order) and drops
 * the tail first — oldest-last display order is preserved, the snapshot
 * never keeps more than `MAX_SNAPSHOT_ITEMS` entries or
 * `MAX_SNAPSHOT_BYTES` bytes, and `evictToCap` is the single place that
 * enforces both limits.
 */

export interface LibraryItem {
	id: string;
	title?: string;
	artist?: string;
	[key: string]: unknown;
}

export interface LibrarySnapshot {
	/** ISO timestamp of the last successful fetch that produced this snapshot. */
	savedAt: string;
	items: LibraryItem[];
}

/** Base key for snapshots; the tab's authentication context scopes each value. */
export const LIBRARY_SNAPSHOT_KEY = 'nntune.library.snapshot.v1';
export const LIBRARY_AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
export const LIBRARY_AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';

function snapshotKey(storage: SnapshotStorage, createContext = false): string | null {
	let context: string | null;
	try {
		context = storage.getItem(LIBRARY_AUTH_CONTEXT_KEY);
		if (!context && createContext) {
			context = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`;
			storage.setItem(LIBRARY_AUTH_CONTEXT_KEY, context);
		}
	} catch {
		return null;
	}
	return context ? `${LIBRARY_SNAPSHOT_KEY}.${context}` : null;
}

/** Maximum tracks kept in the snapshot (count cap). */
export const MAX_SNAPSHOT_ITEMS = 200;

/** Maximum serialized snapshot size in bytes (byte cap). */
export const MAX_SNAPSHOT_BYTES = 200 * 1024;

export interface SnapshotStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
	removeItem(key: string): void;
}

export function isLibraryItem(value: unknown): value is LibraryItem {
	if (typeof value !== 'object' || value === null) return false;
	const id = (value as Record<string, unknown>).id;
	return typeof id === 'string' && id.length > 0;
}

/**
 * Normalize an unknown API payload into a list of items. Accepts either a
 * bare array (the documented `/api/library` shape) or `{ items: [...] }`
 * and drops entries without a string `id` so one malformed row can never
 * blank the list.
 */
export function toLibraryItems(payload: unknown): LibraryItem[] | null {
	const raw = Array.isArray(payload)
		? payload
		: typeof payload === 'object' && payload !== null
			? (payload as Record<string, unknown>).items
			: undefined;
	if (!Array.isArray(raw)) return null;
	const items = raw.filter(isLibraryItem);
	return raw.length > 0 && items.length === 0 ? null : items;
}

function utf8Bytes(text: string): number {
	return new TextEncoder().encode(text).length;
}

/** Serialized byte size of a snapshot candidate (items + timestamp). */
export function snapshotByteSize(items: LibraryItem[], savedAt: string): number {
	return utf8Bytes(JSON.stringify({ savedAt, items }));
}

/**
 * Enforce both caps. Count first, then bytes: repeatedly drop the tail
 * until the serialized form fits. Keeping the head preserves the API's
 * order; dropping the tail is the documented eviction.
 */
export function evictToCap(
	items: LibraryItem[],
	maxItems: number = MAX_SNAPSHOT_ITEMS,
	maxBytes: number = MAX_SNAPSHOT_BYTES
): LibraryItem[] {
	const kept = items.slice(0, Math.max(0, maxItems));
	const probeAt = new Date().toISOString();
	if (snapshotByteSize(kept, probeAt) <= maxBytes) return kept;

	// Find the largest head prefix that fits without repeatedly serializing each tail drop.
	let low = 0;
	let high = kept.length;
	while (low < high) {
		const middle = Math.ceil((low + high) / 2);
		if (snapshotByteSize(kept.slice(0, middle), probeAt) <= maxBytes) low = middle;
		else high = middle - 1;
	}
	// An oversized single item is dropped rather than persisting over the cap.
	return kept.slice(0, low);
}

export function encodeSnapshot(items: LibraryItem[], nowMs: number = Date.now()): LibrarySnapshot {
	return { savedAt: new Date(nowMs).toISOString(), items: evictToCap(items) };
}

/** Parse a raw stored value; returns null for missing/corrupt/foreign shapes. */
export function decodeSnapshot(raw: string | null): LibrarySnapshot | null {
	if (!raw) return null;
	try {
		const parsed = JSON.parse(raw) as Partial<LibrarySnapshot>;
		if (typeof parsed.savedAt !== 'string' || Number.isNaN(Date.parse(parsed.savedAt))) {
			return null;
		}
		if (!Array.isArray(parsed.items)) return null;
		const items = parsed.items.filter(isLibraryItem);
		if (parsed.items.length > 0 && items.length === 0) return null;
		return { savedAt: parsed.savedAt, items: evictToCap(items) };
	} catch {
		return null;
	}
}

export function loadSnapshot(storage: SnapshotStorage): LibrarySnapshot | null {
	const key = snapshotKey(storage);
	if (!key) return null;
	try {
		return decodeSnapshot(storage.getItem(key));
	} catch {
		return null;
	}
}

/** Persist a fresh fetch result, enforcing caps; returns what was stored. */
export function saveSnapshot(
	storage: SnapshotStorage,
	items: LibraryItem[],
	nowMs: number = Date.now()
): LibrarySnapshot {
	const snapshot = encodeSnapshot(items, nowMs);
	const key = snapshotKey(storage, true);
	try {
		if (key) storage.setItem(key, JSON.stringify(snapshot));
	} catch {
		// Quota or privacy mode: the in-memory snapshot still renders this
		// visit; the next load simply has no stale fallback.
	}
	return snapshot;
}

export function clearSnapshot(storage: SnapshotStorage): void {
	try {
		const key = snapshotKey(storage);
		// Revoke access to the snapshot before attempting to remove its bytes.
		storage.removeItem(LIBRARY_AUTH_CONTEXT_KEY);
		if (key) storage.removeItem(key);
		storage.removeItem(LIBRARY_SNAPSHOT_KEY);
	} catch {
		// Best effort; a stale key is never read without its auth context.
	}
}

/** Notify other open tabs that the browser is crossing an auth boundary. */
export function broadcastAuthTransition(storage: SnapshotStorage): void {
	try {
		const transitionId = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`;
		storage.setItem(LIBRARY_AUTH_TRANSITION_KEY, transitionId);
		storage.removeItem(LIBRARY_AUTH_TRANSITION_KEY);
	} catch {
		// Storage may be unavailable in privacy-restricted browser contexts.
	}
}

/**
 * Badge copy for a stale snapshot. Always prefixed with `stale — updated`
 * (em dash) so specs can assert the prefix without depending on locale
 * date formatting.
 */
export function staleLabel(savedAtIso: string, nowMs: number = Date.now()): string {
	const saved = new Date(savedAtIso);
	if (Number.isNaN(saved.getTime())) return 'stale — updated recently';
	const diffMs = Math.max(0, nowMs - saved.getTime());
	const minutes = Math.floor(diffMs / 60_000);
	let when: string;
	if (minutes < 1) when = 'just now';
	else if (minutes < 60) when = `${minutes}m ago`;
	else {
		const hours = Math.floor(minutes / 60);
		when = hours < 24 ? `${hours}h ago` : saved.toLocaleString();
	}
	return `stale — updated ${when}`;
}
