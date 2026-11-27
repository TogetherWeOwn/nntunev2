import { browserStorage, clearLibraryQueue, libraryContextKey, LIBRARY_QUEUE_KEY, type SnapshotStorage } from '../library/cache.js';
import { decodeQueue, serializeQueue, type QueueState, type Track } from './queue.js';

const MAX_QUEUE_LENGTH = 65_536; // UTF-16 code units, as reported by Web Storage strings.

export function createLibraryQueuePersistence() {
	type Scope = { storage: SnapshotStorage; key: string; saved: string | null };
	let scope: Scope | undefined;
	const owns = (receipt: Scope) =>
		scope === receipt && libraryContextKey(receipt.storage, LIBRARY_QUEUE_KEY) === receipt.key && scope === receipt;

	function restore(storage: SnapshotStorage, live: QueueState, catalogue: () => Track[]): QueueState | null {
		const key = libraryContextKey(storage, LIBRARY_QUEUE_KEY);
		if (!key) {
			scope = undefined;
			return null;
		}
		if (scope?.key === key) {
			if (scope.storage !== storage) scope = { ...scope, storage };
			return null;
		}
		const receipt: Scope = { storage, key, saved: null };
		scope = receipt;
		if (live.tracks.length) {
			save(live);
			return null;
		}
		try {
			const raw = storage.getItem(key);
			if (raw === null || !owns(receipt)) return null;
			const restored = raw.length <= MAX_QUEUE_LENGTH ? decodeQueue(raw, catalogue()) : null;
			if (!owns(receipt)) return null;
			if (restored) receipt.saved = raw;
			else storage.removeItem(key);
			return restored;
		} catch {
			return null;
		}
	}

	function save(state: QueueState): void {
		const receipt = scope;
		if (!receipt || !owns(receipt)) return;
		const { storage, key, saved } = receipt;
		try {
			const raw = serializeQueue(state);
			if (raw === saved || !owns(receipt)) return;
			if (raw.length > MAX_QUEUE_LENGTH) storage.removeItem(key);
			else storage.setItem(key, raw);
			receipt.saved = raw.length > MAX_QUEUE_LENGTH ? null : raw;
		} catch {
			// Avoid restoring an older order after a storage or quota failure.
			receipt.saved = null;
			try { if (owns(receipt)) storage.removeItem(key); } catch { /* Best effort. */ }
		}
	}

	function clear(): void {
		scope = undefined;
		clearLibraryQueue(browserStorage('session'));
	}

	return { restore, save, clear };
}
