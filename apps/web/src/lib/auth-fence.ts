export const AUTH_GENERATION_KEY = 'nntune.auth.generation.v1';
// The browser releases this origin-wide lock even if a writer tab closes.
export const AUTH_LOCK = 'nntune.auth.cookie-transition.v1';
// An abandoned write can leave its marker; don't guess when its cookie settled.
const PENDING_PREFIX = 'nntune.auth.pending.v1.';
let localEpoch = 0;
let pending = 0;

function generation(): string | null {
	try {
		return window.localStorage.getItem(AUTH_GENERATION_KEY);
	} catch {
		return null;
	}
}

const pendingMarker = (key: string) => key.startsWith(PENDING_PREFIX);

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

type ClearMarker = () => unknown;
const clearLater = (clear?: ClearMarker) => {
	void Promise.resolve()
		.then(clear)
		.catch(() => {});
};

async function markMutation(): Promise<ClearMarker | undefined> {
	try {
		const key = `${PENDING_PREFIX}${crypto.randomUUID()}`;
		try {
			const storage = window.localStorage;
			storage.setItem(key, '');
			return () => storage.removeItem(key);
		} catch {
			// Empty cache name only: no cached data.
			const storage = globalThis.caches;
			await storage.open(key);
			return () => storage.delete(key);
		}
	} catch {
		/* Both stores denied the write. */
	}
}

async function markBeforeDispatch(signal: AbortSignal) {
	signal.throwIfAborted();
	const marker = markMutation();
	let onAbort!: () => void;
	const aborted = new Promise<never>((_, reject) => {
		onAbort = () => reject(signal.reason);
		signal.addEventListener('abort', onAbort, { once: true });
	});
	try {
		return await Promise.race([marker, aborted]);
	} catch (error) {
		void marker.then(clearLater); // Cache creation itself cannot be aborted.
		throw error;
	} finally {
		signal.removeEventListener('abort', onAbort);
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
				if (!lock || pending || (await cacheReceiptsBlocked()) || pending) return;
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
	if (!locks || typeof window === 'undefined' || !('localStorage' in window)) return;
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
			!!id && !pending && epoch === localEpoch && !receiptsBlocked() && generation() === id;
		if (!(await cacheReceiptsBlocked()) && isCurrent())
			return {
				isCurrent,
				run: <T>(work: () => T) => readLock(() => (isCurrent() ? work() : undefined))
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
	const send = async (locked: boolean, clear?: ClearMarker) => {
		let invalidated = false;
		try {
			signal.throwIfAborted();
			invalidated = invalidateGeneration();
			if (locked && !invalidated) clear = await markBeforeDispatch(signal);
			signal.throwIfAborted();
			dispatched = true;
			return await dispatch();
		} finally {
			// Retry before lock release or marker removal. An unguarded write always retries.
			if (!dispatched || ((locked || clear) && invalidated) || invalidateGeneration())
				clearLater(clear);
		}
	};
	try {
		const locks = globalThis.navigator?.locks;
		if (!locks) return await send(false, await markBeforeDispatch(signal));
		for (let attempt = 0; attempt < 2; attempt++) {
			try {
				return await locks.request(AUTH_LOCK, { signal }, () => send(true));
			} catch (error) {
				if (dispatched || signal.aborted) throw error;
			}
			if (!attempt) {
				const clear = await markBeforeDispatch(signal);
				if (clear) return await send(false, clear);
			}
		}
		// No shared primitive was writable. Preserve ordinary auth's once-only fallback.
		return await send(false);
	} finally {
		pending--;
	}
}
