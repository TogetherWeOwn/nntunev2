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
	function read(): RecentEntry[] {
		let raw: string | null;
		try {
			raw = storage.getItem(storageKey);
		} catch {
			return [];
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

	return {
		entries() {
			return read();
		},
		record(trackId, playedAt = now()) {
			if (!isValidId(trackId) || !isValidPlayedAt(playedAt)) return;
			const rest = read().filter((entry) => entry.trackId !== trackId);
			write([{ trackId, playedAt }, ...rest].slice(0, MAX_RECENTLY_PLAYED));
		},
		replay(trackId) {
			if (!isValidId(trackId)) return;
			const current = read();
			if (!current.some((entry) => entry.trackId === trackId)) return;
			const rest = current.filter((entry) => entry.trackId !== trackId);
			write([{ trackId, playedAt: now() }, ...rest].slice(0, MAX_RECENTLY_PLAYED));
		},
		clear() {
			write([]);
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
