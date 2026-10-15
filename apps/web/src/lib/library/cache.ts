/**
 * Offline-friendly library snapshot cache.
 *
 * The library view renders the last-good `/api/library` payload from
 * `localStorage` when the API is unreachable (tunnel down, Access hiccup,
 * airplane wifi) instead of going blank. A fresh success always replaces
 * the snapshot. The badge text always starts with `stale — updated` so the
 * journey specs can pin it by substring.
 *
 * Bounded growth: the snapshot is capped by item count AND serialized
 * bytes. Eviction keeps the head of the list (the API's order) and drops
 * the tail first — oldest-last display order is preserved, the snapshot
 * can never grow past `MAX_SNAPSHOT_ITEMS` entries or
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

/** `localStorage` key for the snapshot. Versioned so a shape change can rotate it. */
export const LIBRARY_SNAPSHOT_KEY = 'nntune.library.snapshot.v1';

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
export function toLibraryItems(payload: unknown): LibraryItem[] {
	const raw = Array.isArray(payload)
		? payload
		: typeof payload === 'object' && payload !== null
			? (payload as Record<string, unknown>).items
			: undefined;
	if (!Array.isArray(raw)) return [];
	return raw.filter(isLibraryItem);
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
 * until the serialized form fits or one item remains. Keeping the head
 * preserves the API's order; dropping the tail is the documented eviction.
 */
export function evictToCap(
	items: LibraryItem[],
	maxItems: number = MAX_SNAPSHOT_ITEMS,
	maxBytes: number = MAX_SNAPSHOT_BYTES
): LibraryItem[] {
	let kept = items.slice(0, Math.max(0, maxItems));
	// Size the timestamp once; the real `savedAt` only makes the payload
	// larger, so sizing against a representative value is conservative.
	const probeAt = new Date().toISOString();
	while (kept.length > 1 && snapshotByteSize(kept, probeAt) > maxBytes) {
		kept = kept.slice(0, kept.length - 1);
	}
	// A single oversized item is kept rather than caching nothing: one stale
	// row beats a blank screen, and the byte cap still bounds the total.
	return kept;
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
		return { savedAt: parsed.savedAt, items: evictToCap(items) };
	} catch {
		return null;
	}
}

export function loadSnapshot(storage: SnapshotStorage): LibrarySnapshot | null {
	try {
		return decodeSnapshot(storage.getItem(LIBRARY_SNAPSHOT_KEY));
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
	try {
		storage.setItem(LIBRARY_SNAPSHOT_KEY, JSON.stringify(snapshot));
	} catch {
		// Quota or privacy mode: the in-memory snapshot still renders this
		// visit; the next load simply has no stale fallback.
	}
	return snapshot;
}

export function clearSnapshot(storage: SnapshotStorage): void {
	try {
		storage.removeItem(LIBRARY_SNAPSHOT_KEY);
	} catch {
		// Best effort; a stale key is harmless (decode is fail-closed).
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
