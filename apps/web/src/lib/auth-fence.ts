export const AUTH_GENERATION_KEY = 'nntune.auth.generation.v1';
// The browser releases this origin-wide lock even if a writer tab closes.
export const AUTH_LOCK = 'nntune.auth.cookie-transition.v1';
// An abandoned write can leave its marker; don't guess when its cookie settled.
const PENDING_PREFIX = 'nntune.auth.pending.v1.';
let localEpoch = 0;
let pending = 0;

function withStorage<T>(read: (storage: Storage) => T, fallback: T): T {
	try {
		return read(window.localStorage);
	} catch {
		return fallback;
	}
}
const generation = () => withStorage((storage) => storage.getItem(AUTH_GENERATION_KEY), null);
const pendingMarker = (key: string) => key.startsWith(PENDING_PREFIX);
const receiptsBlocked = () => withStorage((storage) => Object.keys(storage).some(pendingMarker), true);
async function cacheReceiptsBlocked(): Promise<boolean> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		const blocked = new Promise<true>((resolve) => (timer = setTimeout(resolve, 500, true)));
		return await Promise.race([caches.keys().then((keys) => keys.some(pendingMarker)), blocked]);
	} catch { return true; } finally { clearTimeout(timer); }
}

type ClearMarker = () => unknown;
const clearLater = (clear?: ClearMarker) => void Promise.resolve().then(clear).catch(() => undefined);
async function markMutation(): Promise<ClearMarker | undefined> {
	try {
		const key = `${PENDING_PREFIX}${crypto.randomUUID()}`;
		try {
			const storage = window.localStorage;
			storage.setItem(key, '');
			return () => storage.removeItem(key);
		} catch {} // Try the fallback store.
		await caches.open(key); // Empty cache name only: no cached data.
		return () => caches.delete(key);
	} catch {} // Neither store recorded a marker.
}
function markBeforeDispatch(signal: AbortSignal) {
	return new Promise<ClearMarker | undefined>((resolve, reject) => {
		signal.throwIfAborted();
		const onAbort = () => reject(signal.reason);
		signal.addEventListener('abort', onAbort, { once: true });
		void markMutation().then((clear) => {
			signal.removeEventListener('abort', onAbort);
			// Cache creation itself cannot be aborted.
			if (signal.aborted) clearLater(clear);
			else resolve(clear);
		});
	});
}

function invalidateGeneration(removeOnFailure = true): boolean {
	try {
		window.localStorage.setItem(AUTH_GENERATION_KEY, crypto.randomUUID());
		return true;
	} catch {
		// Removal doesn't need quota. A later capture must initialize a new generation.
		return removeOnFailure && withStorage((storage) => {
			storage.removeItem(AUTH_GENERATION_KEY);
			return true;
		}, false);
	}
}

async function readLock<T>(work: () => T | Promise<T>): Promise<T | undefined> {
	try {
		return await globalThis.navigator?.locks?.request(AUTH_LOCK, { mode: 'shared', ifAvailable: true }, async (lock) => {
			if (lock && !pending && !(await cacheReceiptsBlocked()) && !pending) return work();
		});
	} catch {
		return undefined;
	}
}
/** Use run() for trusted dispatch, publication and storage commits, not just a receipt check. */
export function captureAuthFence() {
	const locks = globalThis.navigator?.locks;
	if (!locks || typeof window === 'undefined') return;
	const epoch = localEpoch;
	return readLock(async () => {
		if (receiptsBlocked()) return;
		// Initialize once, while the outer shared lock excludes locked writes.
		if (!generation()) await locks.request(`${AUTH_LOCK}.init`, () => {
			if (!generation() && !receiptsBlocked()) invalidateGeneration(false);
		});
		const id = generation();
		const isCurrent = () => !pending && epoch === localEpoch && !receiptsBlocked() && generation() === id;
		if (!id || (await cacheReceiptsBlocked()) || !isCurrent()) return;
		return { generation: id, isCurrent, run: <T>(work: () => T) => readLock(() => isCurrent() ? work() : undefined) };
	});
}

type AuthIntent = { stale: Error; revocation?: { generation: string | undefined } };
/** Mark before dispatch and hold until fetch settles (including response cookies). */
export async function runAuthMutation<T>(signal: AbortSignal, dispatch: () => Promise<T>, intent?: AuthIntent): Promise<T> {
	if (typeof window === 'undefined') return dispatch();
	const { stale, revocation } = intent ?? {};
	if (revocation && (!revocation.generation || pending)) throw stale;
	// Snapshot local sign-out before queuing, including an absent generation.
	const original = stale && !revocation ? generation() : undefined;
	localEpoch++;
	pending++;
	let dispatched = false;
	const send = async (locked: boolean, clear?: ClearMarker) => {
		if (locked && original !== undefined && original !== generation()) throw stale;
		try {
			signal.throwIfAborted();
			if (!invalidateGeneration() && locked) {
				if (revocation) throw stale;
				clear = await markBeforeDispatch(signal);
			}
			signal.throwIfAborted();
			dispatched = true;
			return await dispatch();
		} finally {
			// Rotate at settlement too so an intent captured mid-write cannot survive it.
			if (!dispatched || invalidateGeneration()) clearLater(clear);
		}
	};
	try {
		const locks = globalThis.navigator?.locks;
		if (revocation) {
			// Account-wide revocation requires immediate exclusive admission.
			if (!locks) throw stale;
			return await locks.request(AUTH_LOCK, { ifAvailable: true }, async (lock) => {
				if (!lock || (await cacheReceiptsBlocked()) || pending !== 1 || receiptsBlocked() || revocation.generation !== generation()) throw stale;
				return send(true);
			});
		}
		if (!locks) return await send(false, await markBeforeDispatch(signal));
		for (let attempt = 0; attempt < 2; attempt++) {
			try {
				return await locks.request(AUTH_LOCK, { signal }, () => send(true));
			} catch (error) {
				if (dispatched || signal.aborted || (stale && error === stale)) throw error;
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
