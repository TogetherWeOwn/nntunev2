/**
 * Session-local recently-played list for the demo player. Serialized as a
 * JSON array of `{ trackId, playedAt }` (most-recent-first) under one
 * sessionStorage key; all storage access is fail-soft. Demo synthetic
 * tracks only — never library or private history.
 */

export const RECENTLY_PLAYED_STORAGE_KEY = 'nntune:demo-recently-played:v1';
export const MAX_RECENTLY_PLAYED = 10;

export interface RecentEntry {
	trackId: string;
	/** Unix-epoch-milliseconds timestamp, stored verbatim (no rounding). */
	playedAt: number;
}

export interface RecentlyPlayedStoreDeps {
	storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
	storageKey?: string;
	now?: () => number;
}

export interface RecentlyPlayedStore {
	/** Entries most-recent-first as a snapshot (safe to mutate). */
	entries(): RecentEntry[];
	/**
	 * Prepend `{ trackId, playedAt }`, moving an existing entry for the
	 * track to the front with the new timestamp, then cap to
	 * `MAX_RECENTLY_PLAYED` distinct tracks. Blank ids and non-finite or
	 * negative `playedAt` values are ignored (no write, no throw).
	 */
	record(trackId: string, playedAt?: number): void;
	/**
	 * Move the existing entry for the track to the front with a refreshed
	 * `now()` timestamp. Unknown or blank ids are a no-op (never invents).
	 */
	replay(trackId: string): void;
	/**
	 * Remove exactly the entry for `trackId`, preserving the remaining
	 * order and timestamps, then best-effort persist the same array
	 * schema. Unknown, blank or whitespace-padded ids are a no-op that
	 * never touches storage (exact stable-ID match only). A later actual
	 * playback may legitimately re-add the track via `record`.
	 */
	forget(trackId: string): void;
	/** Empty the list and persist `[]`. */
	clear(): void;
}

function isValidId(trackId: unknown): trackId is string {
	return typeof trackId === 'string' && trackId.trim() !== '';
}

function isValidPlayedAt(playedAt: unknown): playedAt is number {
	return typeof playedAt === 'number' && Number.isFinite(playedAt) && playedAt >= 0;
}

function isValidEntry(value: unknown): value is RecentEntry {
	if (typeof value !== 'object' || value === null) return false;
	const { trackId, playedAt } = value as Record<string, unknown>;
	return isValidId(trackId) && isValidPlayedAt(playedAt);
}

export function createRecentlyPlayedStore({
	storage,
	storageKey = RECENTLY_PLAYED_STORAGE_KEY,
	now = Date.now
}: RecentlyPlayedStoreDeps): RecentlyPlayedStore {
	/**
	 * The visit's known-good list. `null` until the first successful
	 * storage read establishes the baseline; afterwards every operation
	 * runs against the cache and persistence is best-effort, so a later
	 * quota/read denial can neither resurrect a forgotten entry nor roll
	 * back this visit's result. A store that never reads successfully
	 * keeps the legacy degrade-to-empty behavior instead.
	 */
	let cached: RecentEntry[] | null = null;

	/**
	 * Sanitized storage contents, or `null` when the key is unreadable.
	 * Missing/corrupt/non-array payloads read as empty (a successful read
	 * of nothing known), matching the legacy fallback.
	 */
	function readStorage(): RecentEntry[] | null {
		let raw: string | null;
		try {
			raw = storage.getItem(storageKey);
		} catch {
			return null;
		}
		if (raw === null) return [];
		let parsed: unknown;
		try {
			parsed = JSON.parse(raw);
		} catch {
			return [];
		}
		if (!Array.isArray(parsed)) return [];
		const seen = new Set<string>();
		const entries: RecentEntry[] = [];
		for (const item of parsed) {
			if (!isValidEntry(item)) continue;
			if (seen.has(item.trackId)) continue;
			seen.add(item.trackId);
			entries.push({ trackId: item.trackId, playedAt: item.playedAt });
			if (entries.length >= MAX_RECENTLY_PLAYED) break;
		}
		return entries;
	}

	function write(entries: RecentEntry[]): void {
		try {
			storage.setItem(storageKey, JSON.stringify(entries));
		} catch {
			// Fail-soft: the list is a demo nicety, never load-bearing.
		}
	}

	/** The current list: the cached visit result once known, else a fresh read. */
	function current(): RecentEntry[] {
		if (cached === null) {
			const fresh = readStorage();
			if (fresh === null) return [];
			cached = fresh;
		}
		return cached;
	}

	function commit(next: RecentEntry[]): void {
		// Retain the visit's result even when persistence below fails; a
		// store with no readable baseline stays degrade-to-empty instead.
		if (cached !== null) cached = next;
		write(next);
	}

	function snapshot(entries: RecentEntry[]): RecentEntry[] {
		return entries.map((entry) => ({ trackId: entry.trackId, playedAt: entry.playedAt }));
	}

	return {
		entries() {
			return snapshot(current());
		},
		record(trackId, playedAt = now()) {
			if (!isValidId(trackId) || !isValidPlayedAt(playedAt)) return;
			const rest = current().filter((entry) => entry.trackId !== trackId);
			commit([{ trackId, playedAt }, ...rest].slice(0, MAX_RECENTLY_PLAYED));
		},
		replay(trackId) {
			if (!isValidId(trackId)) return;
			const list = current();
			if (!list.some((entry) => entry.trackId === trackId)) return;
			const rest = list.filter((entry) => entry.trackId !== trackId);
			commit([{ trackId, playedAt: now() }, ...rest].slice(0, MAX_RECENTLY_PLAYED));
		},
		forget(trackId) {
			if (!isValidId(trackId)) return;
			const list = current();
			if (!list.some((entry) => entry.trackId === trackId)) return;
			commit(list.filter((entry) => entry.trackId !== trackId));
		},
		clear() {
			commit([]);
		}
	};
}

/**
 * Relative played-at label: under 60s is `'just now'` (future timestamps
 * clamp here too), under 60min is `'<N> min ago'`, under 24h is
 * `'<N> hr ago'`, otherwise `'<N> d ago'`.
 */
export function formatRecentPlayedAt(playedAt: number, now: number): string {
	const elapsed = now - playedAt;
	if (elapsed < 60_000) return 'just now';
	if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)} min ago`;
	if (elapsed < 86_400_000) return `${Math.floor(elapsed / 3_600_000)} hr ago`;
	return `${Math.floor(elapsed / 86_400_000)} d ago`;
}
