// Non-credential state shared by tabs on the app origin. Storage events are
// useful for UI resets, but neither starting nor checking a receipt uses them.
export const AUTH_GENERATION_KEY = 'nntune.auth.generation.v1';
const pendingPrefix = `${AUTH_GENERATION_KEY}.pending.`;
const blockedKey = `${AUTH_GENERATION_KEY}.blocked`;
let localPending = 0;
let storageFailed = false;

const rejected = () => false;

/** Capture before starting trusted work; check again at its commit point. */
export function captureAuthFence(): () => boolean {
	function idleGeneration(initialize: boolean): string | null {
		if (localPending || storageFailed) return null;
		try {
			const storage = window.localStorage;
			let generation = storage.getItem(AUTH_GENERATION_KEY);
			if (generation === null && initialize) {
				generation = crypto.randomUUID();
				storage.setItem(AUTH_GENERATION_KEY, generation);
			}
			if (!generation || Object.keys(storage).some((key) => key.startsWith(pendingPrefix))) {
				return null;
			}
			// A writer might have started while we were scanning the pending keys.
			return storage.getItem(AUTH_GENERATION_KEY) === generation && storage.getItem(blockedKey) === null
				? generation : null;
		} catch {
			return null;
		}
	}

	const generation = idleGeneration(true);
	return generation ? () => idleGeneration(false) === generation : rejected;
}

/** Mark synchronously before dispatching a cookie-mutating browser request. */
export function beginAuthMutation(): () => void {
	if (typeof window === 'undefined') return () => {};
	localPending++;
	let storage: Storage | undefined;
	let pending: string | undefined;
	let published = false;
	try {
		storage = window.localStorage;
		const generation = crypto.randomUUID();
		pending = `${pendingPrefix}${generation}`;
		storage.setItem(pending, '');
		// Even a failed HTTP request retires old receipts. Separate pending
		// keys let overlapping requests settle without clearing each other.
		if (storage.getItem(AUTH_GENERATION_KEY) !== '') {
			storage.setItem(AUTH_GENERATION_KEY, generation);
		}
		published = true;
	} catch {
		storageFailed = true;
		// Quota can refuse a new key even though shrinking the generation
		// value works. Quarantine instead of leaving an old receipt valid.
		try { storage?.setItem(AUTH_GENERATION_KEY, ''); } catch { /* Storage is blocked. */ }
		// A separate flag survives a generation write already underway in another tab.
		try { storage?.setItem(blockedKey, ''); } catch { /* Generation remains closed. */ }
	}
	return () => {
		localPending--;
		// Without a published generation, don't make any written marker disappear.
		try { if (published && pending) storage?.removeItem(pending); } catch { /* Leave the fence closed. */ }
	};
}
