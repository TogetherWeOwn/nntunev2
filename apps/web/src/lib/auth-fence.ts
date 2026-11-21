// The browser releases this origin-wide lock even if a writer tab closes.
export const AUTH_GENERATION_KEY = 'nntune.auth.generation.v1';
export const AUTH_LOCK = 'nntune.auth.cookie-transition.v1';
let localEpoch = 0;
let pending = 0;

function generation(): string | null {
	try { return window.localStorage.getItem(AUTH_GENERATION_KEY); }
	catch { return null; }
}

async function tryLock<T>(work: () => T): Promise<T | undefined> {
	try {
		return await globalThis.navigator?.locks?.request(AUTH_LOCK, { ifAvailable: true },
			(lock) => lock && !pending ? work() : undefined);
	} catch { return undefined; }
}

/** Use run() for trusted dispatch, publication and storage commits, not just a receipt check. */
export function captureAuthFence() {
	if (typeof window === 'undefined' || !globalThis.navigator?.locks) return;
	return tryLock(() => {
		let id = generation();
		if (!id) {
			id = crypto.randomUUID();
			window.localStorage.setItem(AUTH_GENERATION_KEY, id);
		}
		const epoch = localEpoch;
		const isCurrent = () => !pending && epoch === localEpoch && generation() === id;
		return { isCurrent, run: <T>(work: () => T) => tryLock(() => isCurrent() ? work() : undefined) };
	});
}

/** Mark before dispatch and hold until fetch settles (including response cookies). */
export async function runAuthMutation<T>(signal: AbortSignal, dispatch: () => Promise<T>): Promise<T> {
	if (typeof window === 'undefined') return dispatch();
	localEpoch++;
	pending++;
	const send = () => {
		signal.throwIfAborted();
		try { window.localStorage.setItem(AUTH_GENERATION_KEY, crypto.randomUUID()); }
		catch {
			// Removal doesn't need quota. A later capture must initialize a NEW generation.
			try { window.localStorage.removeItem(AUTH_GENERATION_KEY); } catch { /* Storage blocked. */ }
		}
		return dispatch();
	};
	try {
		const locks = globalThis.navigator?.locks;
		// Without Web Locks, auth remains available but trusted reads fail closed.
		return await (locks ? locks.request(AUTH_LOCK, { signal }, send) : send());
	} finally { pending--; }
}
