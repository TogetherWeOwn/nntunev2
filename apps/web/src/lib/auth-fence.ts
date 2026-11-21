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

const pendingMarker = (key: string) => key.startsWith(UNLOCKED_PREFIX);

function receiptsBlocked(): boolean {
	try {
		return Object.keys(window.localStorage).some(pendingMarker);
	} catch {
		return true;
	}
}

async function cacheReceiptsBlocked(): Promise<boolean> {
	try {
		return (await caches.keys()).some(pendingMarker);
	} catch {
		return true;
	}
}

async function markUnlockedMutation(): Promise<
	(() => Promise<void>) | undefined
> {
	try {
		const key = `${UNLOCKED_PREFIX}${crypto.randomUUID()}`;
		try {
			const storage = window.localStorage;
			storage.setItem(key, '');
			return async () => storage.removeItem(key);
		} catch {
			// Empty cache name only: separate from localStorage quota, no cached data.
			const storage = globalThis.caches;
			await storage.open(key);
			return async () => {
				await storage.delete(key);
			};
		}
	} catch {
		/* A context denied both shared stores cannot announce its transition. */
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
			async (lock) => {
				if (!lock || pending || (await cacheReceiptsBlocked()) || pending)
					return;
				return work();
			}
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
		if (receiptsBlocked()) return;
		const storage = window.localStorage;
		// Initialize once, while the outer shared lock excludes locked writes.
		if (!generation())
			await locks.request(`${AUTH_LOCK}.init`, () => {
				try {
					if (!generation() && !receiptsBlocked())
						storage.setItem(AUTH_GENERATION_KEY, crypto.randomUUID());
				} catch {
					/* No receipt while generation storage is unavailable. */
				}
			});
		const id = generation();
		const isCurrent = () =>
			!!id &&
			!pending &&
			epoch === localEpoch &&
			!receiptsBlocked() &&
			generation() === id;
		if (!(await cacheReceiptsBlocked()) && isCurrent())
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
		const clearMarker = unlocked ? await markUnlockedMutation() : undefined;
		let invalidated = false;
		try {
			signal.throwIfAborted();
			invalidated = invalidateGeneration();
			dispatched = true;
			return await dispatch();
		} finally {
			// Invalidate again if an unlocked write could not publish a guard.
			if (unlocked && !clearMarker && dispatched) invalidateGeneration();
			// Retry on recovery; never remove a guard that still protects old receipts.
			if (
				clearMarker &&
				(!dispatched || invalidated || invalidateGeneration())
			) {
				try {
					await clearMarker();
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
