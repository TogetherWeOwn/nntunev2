import { afterEach, expect, it, vi } from 'vitest';
import { ApiClient } from '../api/client.js';
import { AUTH_GENERATION_KEY, AUTH_LOCK, captureAuthFence } from '../auth-fence.js';
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
function setup() {
	const local = storage(), session = storage();
	vi.stubGlobal('window', { localStorage: local, sessionStorage: session });
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
afterEach(() => { vi.unstubAllGlobals(); vi.unstubAllEnvs(); });

it.each([200, 401, 403, 0])('withholds a stale result (%i) without purging its scope', async (status) => {
	const { local, session, verify, refresh, update, refresher, onAccessRefused } = setup();
	await refresh();
	const key = update().deviceScope!;
	local.setItem(key, bytes);
	const started = Promise.withResolvers<void>(), reply = Promise.withResolvers<Response>();
	const deferred = () => { started.resolve(); return reply.promise; };
	if (status < 400) verify.mockImplementationOnce(deferred);
	const pending = status < 400 ? refresh() : refresher.refresh(deferred);
	await started.promise;
	if (status) {
		local.setItem(AUTH_GENERATION_KEY, 'replacement');
		reply.resolve(status === 200 ? proof() : new Response(null, { status }));
		await pending;
	} else await navigator.locks.request(AUTH_LOCK, () => { reply.resolve(proof()); return pending; });
	expect(refresher.state).toMatchObject({ items: [], requiresSignIn: false });
	expect(update().deviceScope).toBeUndefined();
	expect(onAccessRefused).not.toHaveBeenCalled();
	expect(loadSnapshot(session)?.items).toEqual([{ id: 'track' }]);
	await refresh();
	expect([update().deviceScope, local.getItem(key)]).toEqual([key, bytes]);
});

it('allows concurrent readers and queues the cookie write, then recovers after quota failure', async () => {
	const { local, refresh, update } = setup();
	const [first, second] = await Promise.all([captureAuthFence(), captureAuthFence()]);
	await refresh();
	const { deviceScope: key, isCurrent } = update();
	expect(key).toMatch(/^nntune\.library\.favorites\.v1\./);
	local.setItem(key!, bytes);
	let logout!: Promise<void>;
	await first!.run(async () => {
		expect(await second!.run(() => true)).toBe(true);
		vi.spyOn(local, 'setItem').mockImplementationOnce(() => { throw new DOMException('full', 'QuotaExceededError'); });
		logout = new ApiClient({ baseUrl: '', fetchFn: async () => Response.json({}, { status: 500 }) }).logout();
		void logout.catch(() => {});
		expect((await navigator.locks.query()).pending).toContainEqual(expect.objectContaining({ name: AUTH_LOCK, mode: 'exclusive' }));
	});
	await expect(logout).rejects.toMatchObject({ status: 500 });
	await refresh();
	expect([isCurrent(), update().deviceScope, local.getItem(key!)]).toEqual([false, key, bytes]);
});

it('preserves exact favorite bytes across inconclusive verification and never binds a legacy list', async () => {
	const { local, session, verify, refresh, update, refresher, onAccessRefused } = setup();
	await refresh();
	const key = update().deviceScope!;
	local.setItem(key, bytes);
	for (const binding of [null, '', `${label.slice(1)}B`, label]) {
		if (binding === label) verify.mockResolvedValueOnce(Response.json({}));
		await refresh(binding);
		expect(refresher.state).toMatchObject({ items: [], requiresSignIn: false });
		expect(update().deviceScope).toBeUndefined();
	}
	expect(onAccessRefused).not.toHaveBeenCalled();
	await refresh();
	expect([update().deviceScope, local.getItem(key)]).toEqual([key, bytes]);
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'legacy');
	local.setItem('nntune.library.favorites.v1.legacy', bytes);
	await refresh();
	expect(update()).toMatchObject({ sessionBinding: label, deviceScope: undefined });
	expect(local.getItem('nntune.library.favorites.v1.legacy')).toBe(bytes);
});

it('allows auth but withholds headerless browser rows without a fence', async () => {
	const { refresh, refresher } = setup();
	vi.stubEnv('MODE', 'production');
	vi.stubGlobal('window', { get localStorage() { throw new Error('Blocked'); } });
	await new ApiClient({ baseUrl: '', fetchFn: async () => Response.json({}) }).logout();
	await refresh(null);
	expect(refresher.state).toMatchObject({ items: [], requiresSignIn: false });
});

it('recovers a new bound scope after a proof-only quota failure', async () => {
	const { refresh, session, update } = setup();
	vi.spyOn(session, 'setItem').mockImplementationOnce(() => { throw new Error('Quota'); });
	await refresh();
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	await refresh();
	expect(update().deviceScope).toBeDefined();
});

it('falls back from lock denial before dispatch, but never resends a failed mutation', async () => {
	setup();
	const send = vi.fn(async () => { throw new Error('network'); });
	const request = vi.spyOn(navigator.locks, 'request').mockRejectedValueOnce(new DOMException('Denied', 'SecurityError'));
	try {
		const api = new ApiClient({ baseUrl: '', fetchFn: send });
		await expect(api.logout()).rejects.toThrow('network');
		await expect(api.logout()).rejects.toThrow('network');
		expect(send).toHaveBeenCalledTimes(2);
	} finally { request.mockRestore(); }
});
