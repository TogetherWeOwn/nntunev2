import { browserStorage, clearLibraryQueue, libraryContextKey, LIBRARY_QUEUE_KEY, type SnapshotStorage } from '../library/cache.js';
import { decodeQueue, serializeQueue, type QueueState, type Track } from './queue.js';

const MAX_QUEUE_LENGTH = 65_536; // UTF-16 code units, as reported by Web Storage strings.

export function createLibraryQueuePersistence() {
	let scope: { storage: SnapshotStorage; key: string; saved: string | null } | undefined;

	function restore(storage: SnapshotStorage, live: QueueState, catalogue: () => Track[]): QueueState | null {
		const key = libraryContextKey(storage, LIBRARY_QUEUE_KEY);
		if (!key) return null;
		if (scope?.key === key) {
			scope.storage = storage;
			return null;
		}
		scope = { storage, key, saved: null };
		if (live.tracks.length) {
			save(live);
			return null;
		}
		try {
			const raw = storage.getItem(key);
			if (raw === null) return null;
			const restored = raw.length <= MAX_QUEUE_LENGTH ? decodeQueue(raw, catalogue()) : null;
			if (restored) scope.saved = raw;
			else storage.removeItem(key);
			return restored;
		} catch {
			return null;
		}
	}

	function save(state: QueueState): void {
		if (!scope) return;
		const { storage, key, saved } = scope;
		const raw = serializeQueue(state);
		if (raw === saved) return;
		try {
			if (raw.length > MAX_QUEUE_LENGTH) storage.removeItem(key);
			else storage.setItem(key, raw);
			scope.saved = raw.length > MAX_QUEUE_LENGTH ? null : raw;
		} catch {
			// Avoid restoring an older order after a storage or quota failure.
			scope.saved = null;
			try { storage.removeItem(key); } catch { /* Best effort. */ }
		}
	}

	function clear(): void {
		scope = undefined;
		clearLibraryQueue(browserStorage('session'));
	}

	return { restore, save, clear };
}
