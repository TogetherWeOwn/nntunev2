import { afterEach, expect, it, vi } from 'vitest';
import { API_REQUEST_TIMEOUT_MS, ApiClient } from './api/client.js';
import { AUTH_GENERATION_KEY, AUTH_LOCK, captureAuthFence } from './auth-fence.js';
const denied = () => new DOMException('Denied', 'SecurityError');
const full = () => new DOMException('Storage full', 'QuotaExceededError');
const fail = (error: () => Error) => () => { throw error(); };
const exclusive = expect.objectContaining({ name: AUTH_LOCK, mode: 'exclusive' });
const ok = () => Response.json({});
function setup() {
	const entries: Record<string, string> = Object.create(null);
	const write = (key: string, value: string) => void Reflect.set(entries, key, value);
	const remove = (key: string) => void Reflect.deleteProperty(entries, key);
	const setItem = vi.fn(write), removeItem = vi.fn(remove);
	vi.stubGlobal('window', { localStorage: Object.assign(entries, { setItem, removeItem, getItem: (key: string) => entries[key] ?? null }) });
	const names = new Set<string>();
	vi.stubGlobal('caches', { keys: async () => [...names], open: async (name: string) => names.add(name), delete: async (name: string) => names.delete(name) });
	const send = vi.fn(async () => ok());
	return { read: () => entries[AUTH_GENERATION_KEY] ?? null, setItem, removeItem, write, remove, send, api: new ApiClient({ baseUrl: '', fetchFn: send }) };
}
async function startWriter(lockDenied = false) {
	// Separate writer epoch and pending count, but the same locks and stores.
	vi.resetModules();
	const { ApiClient: WriterClient } = await import('./api/client.js');
	const started = Promise.withResolvers<void>(), response = Promise.withResolvers<Response>();
	if (lockDenied) vi.spyOn(navigator.locks, 'request').mockRejectedValueOnce(denied());
	const client = new WriterClient({ baseUrl: '', fetchFn: () => { started.resolve(); return response.promise; } });
	const result = client.post('/v1/auth/passkey/login/finish', {});
	await Promise.race([started.promise, result]);
	return () => { response.resolve(ok()); return result; };
}
afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
it('shares the read lock, queues logout and recovers after storage errors', async () => {
	const { read, setItem, write, api, send } = setup();
	setItem.mockImplementationOnce(fail(denied));
	expect(await captureAuthFence()).toBeUndefined();
	setItem.mockClear();
	const [first, second] = await Promise.all([captureAuthFence(), captureAuthFence()]);
	const initial = read();
	expect(setItem).toHaveBeenCalledExactlyOnceWith(AUTH_GENERATION_KEY, initial);
	let logout: Promise<void> | undefined;
	const shared = await first!.run(async () => {
		expect(await second!.run(() => true)).toBe(true);
		setItem.mockImplementation(fail(full));
		logout = api.logout();
		expect((await navigator.locks.query()).pending).toContainEqual(exclusive);
		expect(send).not.toHaveBeenCalled();
		return !first!.isCurrent();
	});
	await logout;
	expect(shared).toBe(true);
	expect(send).toHaveBeenCalledTimes(1);
	expect(read()).toBeNull();
	setItem.mockImplementation(write);
	expect(await (await captureAuthFence())?.run(() => true)).toBe(true);
	expect(read()).not.toBe(initial);
	expect(await first!.run(() => true)).toBeUndefined();
});
it('fences a read-only capture with no generation when a writer hits quota', async () => {
	const { setItem, removeItem } = setup();
	const capture = await captureAuthFence({ readOnly: true });
	expect(capture?.generation).toBeNull();
	expect(setItem).not.toHaveBeenCalled();
	expect(removeItem).not.toHaveBeenCalled();
	setItem.mockImplementation(fail(full));
	await (await startWriter())();
	expect(await capture!.run(() => true)).toBeUndefined();
});
it('does not dispatch logout queued behind a second tab signing in', async () => {
	const { api, send } = setup();
	const finishSignIn = await startWriter();
	const logout = api.logout().catch((error: unknown) => error);
	expect((await navigator.locks.query().finally(finishSignIn)).pending).toContainEqual(exclusive);
	expect(await logout).toMatchObject({ code: 'conflict' });
	expect(send).not.toHaveBeenCalled();
});
it.each(['local', 'cache'])('withholds receipts on %s read denial, not logout', async (store) => {
	const { send, api } = setup();
	const before = (await captureAuthFence())!;
	if (store === 'local') Object.defineProperty(window, 'localStorage', { get: fail(denied) });
	else vi.spyOn(caches, 'keys').mockRejectedValue(denied());
	expect(await before.run(() => true)).toBeUndefined();
	expect(await captureAuthFence()).toBeUndefined();
	await api.logout();
	expect(send).toHaveBeenCalledTimes(1);
});
it.each(['local', 'cache', 'lock retry'])('withholds cross-tab receipts via %s', async (guard) => {
	const { setItem, write } = setup();
	if (guard !== 'local') setItem.mockImplementation(fail(full));
	if (guard === 'lock retry') vi.spyOn(caches, 'open').mockRejectedValue(full());
	const first = await startWriter(true);
	try {
		expect(await caches.keys()).toHaveLength(guard === 'cache' ? 1 : 0);
		setItem.mockImplementation(write);
		expect(await captureAuthFence()).toBeUndefined();
		if (guard !== 'lock retry') {
			await (await startWriter(true))();
			expect(await captureAuthFence()).toBeUndefined();
		}
	} finally { await first(); }
	expect((await captureAuthFence())?.isCurrent()).toBe(true);
});
it.each([true, false])('guards locked writes on generation failure (recovers: %s)', async (recovers) => {
	const { setItem, write, removeItem, remove } = setup();
	const before = (await captureAuthFence())!;
	setItem.mockImplementation(fail(denied));
	removeItem.mockImplementation(fail(denied));
	const writer = await startWriter();
	setItem.mockImplementation(recovers ? write : fail(denied));
	removeItem.mockImplementation(recovers ? remove : fail(denied));
	await writer();
	expect(await before.run(() => true)).toBeUndefined();
	expect((await captureAuthFence())?.isCurrent()).toBe(recovers ? true : undefined);
});
it.each(['open', 'delete'] as const)('does not wait beyond the auth deadline for cache %s', async (op) => {
	const { setItem, api, send } = setup();
	vi.useFakeTimers();
	vi.spyOn(navigator.locks, 'request').mockRejectedValueOnce(denied());
	setItem.mockImplementation(fail(full));
	const ready = Promise.withResolvers<void>();
	const open = caches.open.bind(caches), remove = caches.delete.bind(caches);
	if (op === 'open') vi.spyOn(caches, 'open').mockImplementation((key) => ready.promise.then(() => open(key)));
	else vi.spyOn(caches, 'delete').mockImplementation((key) => ready.promise.then(() => remove(key)));
	const logout = api.logout().catch((error: unknown) => error);
	try {
		await vi.advanceTimersByTimeAsync(API_REQUEST_TIMEOUT_MS);
		expect(await logout).toEqual(op === 'open' ? expect.objectContaining({ code: 'request_timeout' }) : undefined);
	} finally { ready.resolve(); await vi.advanceTimersByTimeAsync(0); }
	expect(send).toHaveBeenCalledTimes(op === 'open' ? 0 : 1);
	expect(await caches.keys()).toHaveLength(0);
});
it.each(['', '?source=settings'])('fences sign-in and does not retry after dispatch (%s)', async (suffix) => {
	const { api, send } = setup();
	send.mockImplementationOnce(async () => {
		expect((await navigator.locks.query()).held).toContainEqual(exclusive);
		throw new Error('network');
	});
	await expect(api.post(`/v1/auth/magic/finish${suffix}`, {})).rejects.toThrow('network');
	expect(send).toHaveBeenCalledTimes(1);
});
it.each(['busy', 'changed', 'missing', 'unchanged', 'cancelled'])('checks revocation at admission (%s)', async (state) => {
	const { api, send, read, write, remove } = setup();
	const unlock = Promise.withResolvers<void>(), confirmed = Promise.withResolvers<void>();
	let writer: Promise<void> | undefined;
	const result = api.revokeAllSessions(() => {
		expect(read()).toBeTruthy();
		if (state === 'busy') writer = navigator.locks.request(AUTH_LOCK, () => unlock.promise);
		if (state === 'changed') write(AUTH_GENERATION_KEY, 'new-generation');
		if (state === 'missing') remove(AUTH_GENERATION_KEY);
		confirmed.resolve();
		return state !== 'cancelled';
	}).catch((error: unknown) => error);
	try {
		if (state === 'busy') {
			await Promise.race([confirmed.promise, result]);
			expect((await navigator.locks.query()).pending).not.toContainEqual(exclusive);
		}
		expect(await result).toEqual(state === 'cancelled' ? undefined :
			state === 'unchanged' ? {} : expect.objectContaining({ code: 'conflict' }));
		expect(send).toHaveBeenCalledTimes(state === 'unchanged' ? 1 : 0);
	} finally { unlock.resolve(); await Promise.all([writer, result]); }
});
it.each([0, 1])('releases the shared lock when cache enumeration %i stalls', async (check) => {
	const { api, send } = setup();
	vi.useFakeTimers();
	const keys = vi.spyOn(caches, 'keys');
	if (check) keys.mockResolvedValueOnce([]);
	const started = Promise.withResolvers<void>(), unblock = Promise.withResolvers<string[]>();
	keys.mockImplementationOnce(() => { started.resolve(); return unblock.promise; });
	const receipt = captureAuthFence();
	await started.promise;
	const logout = api.logout().catch((error: unknown) => error);
	try {
		await vi.advanceTimersByTimeAsync(API_REQUEST_TIMEOUT_MS);
		expect(await logout).toBeUndefined();
		expect(await receipt).toBeUndefined();
		expect(send).toHaveBeenCalledTimes(1);
	} finally { unblock.resolve([]); await receipt; }
});
