import { afterEach, expect, it, vi } from 'vitest';
import { AUTH_GENERATION_KEY, AUTH_LOCK } from '../auth-fence.js';
import { LIBRARY_AUTH_CONTEXT_KEY, loadSnapshot } from './cache.js';
import { createLibraryRefresher, type LibraryRefreshUpdate } from './refresh.js';

const label = 'A'.repeat(43);
const bytes = ' [ "track" ] ';
const proof = () => Response.json({ session_binding: label });
function storage() {
	const map = new Map<string, string>();
	return { getItem: (key: string) => map.get(key) ?? null, setItem: (key: string, value: string) => { map.set(key, value); },
		removeItem: (key: string) => { map.delete(key); } };
}
function setup(delayRead?: { number: number; wait: Promise<void>; requested: () => void; finished: () => void }) {
	const local = storage(), session = storage();
	vi.stubGlobal('window', { localStorage: local, sessionStorage: session });
	vi.stubGlobal('caches', { keys: async () => [] });
	let writerHeld = false, reads = 0;
	vi.stubGlobal('navigator', { locks: { async request<T>(name: string, options: LockOptions | LockGrantedCallback<T>, read: LockGrantedCallback<T>) {
		if (typeof options !== 'function') {
			const delay = ++reads === delayRead?.number ? delayRead : undefined;
			delay?.requested();
			await delay?.wait;
			try { return await read(writerHeld ? null : {} as Lock); } finally { delay?.finished(); }
		}
		writerHeld = name === AUTH_LOCK;
		try { return await options({} as Lock); } finally { writerHeld = false; }
	} } });
	const verify = vi.fn(async () => proof());
	vi.stubGlobal('fetch', verify);
	let update: LibraryRefreshUpdate;
	const onAccessRefused = vi.fn();
	const refresher = createLibraryRefresher({ sessionStorage: () => session, invalidateLibraryCache: vi.fn(),
		onAccessRefused, onStateChange: (_, next) => { update = next; } });
	const refresh = (binding: string | null = label) => refresher.refresh(async () => Response.json([{ id: 'track' }], {
		headers: binding === null ? {} : { 'x-nntune-session-binding': binding }
	}));
	return { local, session, verify, refresh, refresher, update: () => update, onAccessRefused };
}
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.unstubAllEnvs(); });

it.each([2, 3])('suspends when verification lock admission %i exceeds the deadline', async (delayedRead) => {
	vi.useFakeTimers();
	const requested = Promise.withResolvers<void>(), admitted = Promise.withResolvers<void>(), finished = Promise.withResolvers<void>();
	const { refresh, refresher, session, update, verify } = setup({
		number: delayedRead, wait: admitted.promise, requested: requested.resolve, finished: finished.resolve
	});
	const pending = refresh();
	try {
		await Promise.race([requested.promise, pending.then(() => { throw new Error('No verification lock requested'); })]);
		await vi.advanceTimersByTimeAsync(10_000);
		expect(refresher.state).toMatchObject({ loading: false, items: [], verificationSuspended: true });
	} finally {
		admitted.resolve();
		await pending;
	}
	await finished.promise;
	expect(verify).toHaveBeenCalledTimes(delayedRead - 2);
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	expect(update()).toMatchObject({ authBoundary: true });
	expect(refresher.state.items).toEqual([]);
	await refresh();
	expect(update().sessionBinding).toBe(label);
});

const deferredResults = [200, 401, 403, 'verifier 401', 'writer held', 'listing timeout', 'revoked listing timeout'] as const;
it.each(deferredResults)('handles deferred refresh (%s) without rewriting favorites', async (result) => {
	const timeout = result === 'listing timeout' || result === 'revoked listing timeout';
	const canRestore = result === 'listing timeout';
	if (timeout) vi.useFakeTimers();
	const { local, session, verify, refresh, update, refresher, onAccessRefused } = setup();
	await refresh();
	const key = update().deviceScope!;
	expect(update()).toMatchObject({ sessionBinding: label, deviceScope: expect.any(String) });
	local.setItem(key, bytes);
	const started = Promise.withResolvers<void>(), reply = Promise.withResolvers<Response>();
	const deferred = () => { started.resolve(); return reply.promise; };
	const refused = result === 'verifier 401';
	const verifying = refused || result === 200 || result === 'writer held';
	if (verifying) verify.mockImplementationOnce(deferred);
	const pending = verifying ? refresh() : refresher.refresh(deferred);
	await Promise.race([started.promise, pending.then(() => { throw new Error('Refresh ended before the deferred request'); })]);
	if (result === 'writer held') await navigator.locks.request(AUTH_LOCK, () => { reply.resolve(proof()); return pending; });
	else {
		if (!canRestore && !refused) local.setItem(AUTH_GENERATION_KEY, 'replacement');
		if (timeout) await vi.advanceTimersByTimeAsync(10_000);
		else reply.resolve(result === 200 ? proof() : new Response(null, { status: refused ? 401 : result }));
		await pending;
	}
	expect(refresher.state).toMatchObject({ requiresSignIn: refused, verificationSuspended: !canRestore && !refused,
		items: canRestore ? [{ id: 'track' }] : [], staleAt: canRestore ? expect.any(String) : null });
	expect(update()).toMatchObject({ authoritative: !canRestore, authBoundary: !canRestore });
	expect(update().deviceScope).toBeUndefined();
	expect(onAccessRefused).toHaveBeenCalledTimes(refused ? 1 : 0);
	expect(loadSnapshot(session)?.items).toEqual(refused ? undefined : [{ id: 'track' }]);
	if (refused) expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	else await refresh();
	expect([update().deviceScope, local.getItem(key)]).toEqual([refused ? undefined : key, bytes]);
});

it.each([false, true])('withholds headerless browser rows (storage blocked: %s)', async (blocked) => {
	const { refresh, refresher, session, verify } = setup();
	vi.stubEnv('MODE', 'production');
	if (blocked) vi.stubGlobal('window', { get localStorage() { throw new Error('Blocked'); } });
	await refresh(null);
	expect(refresher.state).toMatchObject({ items: [], requiresSignIn: false, verificationSuspended: true });
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	expect(verify).not.toHaveBeenCalled();
});