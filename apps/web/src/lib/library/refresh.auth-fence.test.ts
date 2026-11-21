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
function setup(finalAdmission?: () => Promise<void>) {
	const local = storage(), session = storage();
	vi.stubGlobal('window', { localStorage: local, sessionStorage: session });
	vi.stubGlobal('caches', { keys: async () => [] });
	let writerHeld = false, reads = 0;
	vi.stubGlobal('navigator', { locks: { async request<T>(name: string, options: LockOptions | LockGrantedCallback<T>, read: LockGrantedCallback<T>) {
		if (typeof options !== 'function') {
			if (++reads === 3) await finalAdmission?.();
			return read(writerHeld ? null : {} as Lock);
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

it('suspends a verified refresh when final lock admission exceeds the deadline', async () => {
	vi.useFakeTimers();
	const requested = Promise.withResolvers<void>(), admitted = Promise.withResolvers<void>();
	const { refresh, refresher, session, update } = setup(() => { requested.resolve(); return admitted.promise; });
	const pending = refresh();
	try {
		await Promise.race([requested.promise, pending.then(() => { throw new Error('No final lock requested'); })]);
		await vi.advanceTimersByTimeAsync(10_000);
		expect(refresher.state).toMatchObject({ loading: false, items: [], verificationSuspended: true });
	} finally {
		admitted.resolve();
		await pending;
	}
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	await refresh();
	expect(update().sessionBinding).toBe(label);
});

it.each([200, 401, 403, 0])('withholds a stale result (%i) without purging its scope', async (status) => {
	const { local, session, verify, refresh, update, refresher, onAccessRefused } = setup();
	const key = await refresh().then(() => update().deviceScope!);
	expect(key).toEqual(expect.any(String));
	local.setItem(key, bytes);
	const started = Promise.withResolvers<void>(), reply = Promise.withResolvers<Response>();
	const deferred = () => { started.resolve(); return reply.promise; };
	if (status < 400) verify.mockImplementationOnce(deferred);
	const pending = status < 400 ? refresh() : refresher.refresh(deferred);
	await Promise.race([started.promise, pending.then(() => { throw new Error('Refresh ended before the deferred request'); })]);
	if (status) {
		local.setItem(AUTH_GENERATION_KEY, 'replacement');
		reply.resolve(status === 200 ? proof() : new Response(null, { status }));
		await pending;
	} else await navigator.locks.request(AUTH_LOCK, () => { reply.resolve(proof()); return pending; });
	expect(refresher.state).toMatchObject({ items: [], requiresSignIn: false, verificationSuspended: true });
	expect(update().deviceScope).toBeUndefined();
	expect(onAccessRefused).not.toHaveBeenCalled();
	expect(loadSnapshot(session)?.items).toEqual([{ id: 'track' }]);
	await refresh();
	expect([update().deviceScope, local.getItem(key)]).toEqual([key, bytes]);
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