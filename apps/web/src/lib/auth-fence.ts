export const AUTH_GENERATION_KEY = 'nntune.auth.generation.v1';
// The browser releases this origin-wide lock even if a writer tab closes.
export const AUTH_LOCK = 'nntune.auth.cookie-transition.v1';
// An abandoned unlocked write can leave its marker; don't guess when its cookie settled.
const UNLOCKED_PREFIX = 'nntune.auth.pending.v1.';
let localEpoch = 0;
let pending = 0;

function generation(): string | null {
	try {
		return window.localStorage.getItem(AUTH_GENERATION_KEY);
	} catch {
		return null;
	}
}

function unlockedMutationPending(): boolean {
	try {
		return Object.keys(window.localStorage).some((key) =>
			key.startsWith(UNLOCKED_PREFIX)
		);
	} catch {
		return true;
	}
}

function markUnlockedMutation(): string | undefined {
	try {
		const storage = window.localStorage;
		// Each fallback owns its marker, so one response can't clear another writer's guard.
		const key = `${UNLOCKED_PREFIX}${crypto.randomUUID()}`;
		try {
			storage.setItem(key, '');
		} catch (error) {
			if (
				!(error instanceof DOMException) ||
				error.name !== 'QuotaExceededError'
			)
				return;
			storage.removeItem(AUTH_GENERATION_KEY);
			storage.setItem(key, '');
		}
		return key;
	} catch {
		return undefined;
	}
}

function invalidateGeneration(): boolean {
	try {
		window.localStorage.setItem(AUTH_GENERATION_KEY, crypto.randomUUID());
	} catch {
		// Removal doesn't need quota. A later capture must initialize a NEW generation.
		try {
			window.localStorage.removeItem(AUTH_GENERATION_KEY);
		} catch {
			return false;
		}
	}
	return true;
}

async function readLock<T>(work: () => T | Promise<T>): Promise<T | undefined> {
	try {
		return await globalThis.navigator?.locks?.request(
			AUTH_LOCK,
			{ mode: 'shared', ifAvailable: true },
			(lock) => (lock && !pending ? work() : undefined)
		);
	} catch {
		return undefined;
	}
}

/** Use run() for trusted dispatch, publication and storage commits, not just a receipt check. */
export function captureAuthFence() {
	const locks = globalThis.navigator?.locks;
	if (!locks || typeof window === 'undefined' || !('localStorage' in window))
		return;
	const epoch = localEpoch;
	return readLock(async () => {
		if (unlockedMutationPending()) return;
		// Initialize once, while the outer shared lock excludes locked writes.
		if (!generation())
			await locks.request(`${AUTH_LOCK}.init`, () => {
				try {
					if (!generation() && !unlockedMutationPending()) {
						window.localStorage.setItem(
							AUTH_GENERATION_KEY,
							crypto.randomUUID()
						);
					}
				} catch {
					/* No receipt while generation storage is unavailable. */
				}
			});
		const id = generation();
		const isCurrent = () =>
			!!id &&
			!pending &&
			epoch === localEpoch &&
			!unlockedMutationPending() &&
			generation() === id;
		if (isCurrent())
			return {
				isCurrent,
				run: <T>(work: () => T) =>
					readLock(() => (isCurrent() ? work() : undefined))
			};
	});
}

/** Mark before dispatch and hold until fetch settles (including response cookies). */
export async function runAuthMutation<T>(
	signal: AbortSignal,
	dispatch: () => Promise<T>
): Promise<T> {
	if (typeof window === 'undefined') return dispatch();
	localEpoch++;
	pending++;
	let dispatched = false;
	const send = async (unlocked = false) => {
		signal.throwIfAborted();
		const marker = unlocked ? markUnlockedMutation() : undefined;
		const invalidated = invalidateGeneration();
		dispatched = true;
		try {
			return await dispatch();
		} finally {
			// If the generation couldn't be invalidated, don't revive old receipts.
			if (marker && invalidated) {
				try {
					window.localStorage.removeItem(marker);
				} catch {
					/* Keep receipts withheld if cleanup is denied. */
				}
			}
		}
	};
	try {
		const locks = globalThis.navigator?.locks;
		// Without Web Locks, auth remains available but trusted reads fail closed.
		if (!locks) return await send(true);
		try {
			return await locks.request(AUTH_LOCK, { signal }, () => send());
		} catch (error) {
			if (dispatched || signal.aborted) throw error;
			return await send(true);
		}
	} finally {
		pending--;
	}
}
