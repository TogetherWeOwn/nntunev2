import { afterEach, expect, it, vi } from 'vitest';
import { API_REQUEST_TIMEOUT_MS, ApiClient, ApiError } from './api/client.js';
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
	const getItem = (key: string) => entries[key] ?? null;
	const localStorage = Object.assign(entries, { getItem, setItem: vi.fn(write), removeItem: vi.fn(remove) });
	vi.stubGlobal('window', { localStorage });
	const names = new Set<string>();
	vi.stubGlobal('caches', {
		keys: async () => [...names],
		open: async (name: string) => names.add(name),
		delete: async (name: string) => names.delete(name)
	});
	const send = vi.fn(async () => ok());
	const read = () => getItem(AUTH_GENERATION_KEY);
	const api = new ApiClient({ baseUrl: '', fetchFn: send });
	return { read, ...localStorage, write, remove, send, api };
}

async function startWriter(lockDenied = false, path = '/v1/session/logout') {
	// Separate writer epoch and pending count, but the same locks and stores.
	vi.resetModules();
	const { ApiClient: WriterClient } = await import('./api/client.js');
	const started = Promise.withResolvers<void>();
	const response = Promise.withResolvers<Response>();
	const fetchFn = vi.fn(() => {
		started.resolve();
		return response.promise;
	});
	if (lockDenied) vi.spyOn(navigator.locks, 'request').mockRejectedValueOnce(denied());
	const result = new WriterClient({ baseUrl: '', fetchFn }).post(path, {});
	await Promise.race([started.promise, result]);
	return async () => {
		response.resolve(ok());
		await result;
		expect(fetchFn).toHaveBeenCalledTimes(1);
	};
}

afterEach(() => {
	vi.useRealTimers();
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

it('shares the read lock, queues logout and recovers generation from quota failure', async () => {
	const { read, setItem, write, api, send } = setup();
	const [first, second] = await Promise.all([captureAuthFence(), captureAuthFence()]);
	const initial = read();
	expect(setItem).toHaveBeenCalledExactlyOnceWith(AUTH_GENERATION_KEY, initial);
	let logout: Promise<void> | undefined;
	try {
		await first!.run(async () => {
			expect(await second!.run(() => true)).toBe(true);
			setItem.mockImplementation(fail(full));
			logout = api.logout();
			expect((await navigator.locks.query()).pending).toContainEqual(exclusive);
			expect(send).not.toHaveBeenCalled();
			expect(first!.isCurrent()).toBe(false);
		});
	} finally {
		await logout;
	}
	expect(send).toHaveBeenCalledTimes(1);
	expect(read()).toBeNull();
	setItem.mockImplementation(write);
	expect(await (await captureAuthFence())?.run(() => true)).toBe(true);
	expect(read()).not.toBe(initial);
	expect(await first!.run(() => true)).toBeUndefined();
});

it('recovers on the next capture after denied generation initialization', async () => {
	const { setItem } = setup();
	setItem.mockImplementationOnce(fail(denied));
	expect(await captureAuthFence()).toBeUndefined();
	expect((await captureAuthFence())?.isCurrent()).toBe(true);
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
	} finally {
		await first();
	}
	expect((await captureAuthFence())?.isCurrent()).toBe(true);
	expect(await caches.keys()).toHaveLength(0);
});

it.each([true, false])('guards locked writes on generation failure (recovers: %s)', async (recovers) => {
	const { setItem, write, removeItem, remove } = setup();
	const before = (await captureAuthFence())!;
	setItem.mockImplementation(fail(denied));
	removeItem.mockImplementation(fail(denied));
	const writer = await startWriter();
	if (recovers) {
		setItem.mockImplementation(write);
		removeItem.mockImplementation(remove);
	}
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
		const result = await logout;
		if (op === 'open') {
			expect(result).toBeInstanceOf(ApiError);
			expect(result).toMatchObject({ code: 'request_timeout' });
		} else expect(result).toBeUndefined();
	} finally {
		ready.resolve();
		await vi.advanceTimersByTimeAsync(0);
	}
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

it('rejects sign out everywhere rather than queuing behind sign-in', async () => {
	const { api, send } = setup();
	const writer = await startWriter(false, '/v1/auth/magic/finish');
	const result = api.revokeAllSessions().catch((error: unknown) => error);
	await new Promise<void>((resolve) => setTimeout(resolve, 0));
	await writer();
	expect(await result).toMatchObject({ code: 'conflict' });
	expect(send).not.toHaveBeenCalled();
	await api.revokeAllSessions();
	expect(send).toHaveBeenCalledTimes(1);
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
