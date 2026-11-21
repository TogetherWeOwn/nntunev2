import { afterEach, expect, it, vi } from 'vitest';
import { API_REQUEST_TIMEOUT_MS, ApiClient, ApiError } from './api/client.js';
import { AUTH_GENERATION_KEY, AUTH_LOCK, captureAuthFence } from './auth-fence.js';

function setup() {
	const localStorage = {
		getItem: (key: string): string | null =>
			Object.getOwnPropertyDescriptor(localStorage, key)?.value ?? null,
		setItem: vi.fn((key: string, value: string) => {
			Object.defineProperty(localStorage, key, {
				value,
				configurable: true,
				enumerable: true
			});
		}),
		removeItem: (key: string) => {
			Reflect.deleteProperty(localStorage, key);
		}
	};
	vi.stubGlobal('window', { localStorage });
	const names = new Set<string>();
	vi.stubGlobal('caches', {
		keys: async () => [...names],
		open: async (name: string) => names.add(name),
		delete: async (name: string) => names.delete(name)
	});
	const send = vi.fn(async () => Response.json({}));
	return { localStorage, send, api: new ApiClient({ baseUrl: '', fetchFn: send }) };
}

const denied = () => new DOMException('Denied', 'SecurityError');
const full = () => new DOMException('Storage full', 'QuotaExceededError');
const refuse = (error: () => Error) => () => {
	throw error();
};

async function startLogout(lockDenied = false) {
	// Separate writer epoch and pending count, but the same browser locks and storage.
	vi.resetModules();
	const { ApiClient: WriterClient } = await import('./api/client.js');
	const started = Promise.withResolvers<void>();
	const response = Promise.withResolvers<Response>();
	const fetchFn = vi.fn(() => {
		started.resolve();
		return response.promise;
	});
	if (lockDenied) vi.spyOn(navigator.locks, 'request').mockRejectedValueOnce(denied());
	const logout = new WriterClient({ baseUrl: '', fetchFn }).logout();
	await Promise.race([started.promise, logout]);
	return {
		finish: () => {
			response.resolve(Response.json({}));
			return logout;
		},
		fetchFn
	};
}

afterEach(() => {
	vi.useRealTimers();
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

it('shares the read lock, queues logout and recovers a new generation after a quota error', async () => {
	const { localStorage, send, api } = setup();
	const [first, second] = await Promise.all([captureAuthFence(), captureAuthFence()]);
	const initial = localStorage.getItem(AUTH_GENERATION_KEY);
	expect(localStorage.setItem).toHaveBeenCalledExactlyOnceWith(AUTH_GENERATION_KEY, initial);
	let logout: Promise<void> | undefined;
	const exclusive = expect.objectContaining({ name: AUTH_LOCK, mode: 'exclusive' });
	send.mockImplementationOnce(async () => {
		expect((await navigator.locks.query()).held).toContainEqual(exclusive);
		return Response.json({});
	});
	try {
		const result = await first!.run(async () => {
			expect(await second!.run(() => true)).toBe(true);
			localStorage.setItem.mockImplementationOnce(refuse(full));
			logout = api.logout();
			expect((await navigator.locks.query()).pending).toContainEqual(exclusive);
			expect(send).not.toHaveBeenCalled();
			return first!.isCurrent();
		});
		expect(result).toBe(false);
	} finally {
		await logout;
	}
	expect(send).toHaveBeenCalledTimes(1);
	expect(localStorage.getItem(AUTH_GENERATION_KEY)).toBeNull();
	expect(await (await captureAuthFence())?.run(() => true)).toBe(true);
	expect(localStorage.getItem(AUTH_GENERATION_KEY)).not.toBe(initial);
	expect(await first!.run(() => true)).toBeUndefined();
});

it('withholds receipts when generation initialization cannot write, and retries on the next capture', async () => {
	const { localStorage } = setup();
	localStorage.setItem.mockImplementationOnce(refuse(denied));
	expect(await captureAuthFence()).toBeUndefined();
	expect((await captureAuthFence())?.isCurrent()).toBe(true);
});

it.each(['local', 'cache'])(
	'withholds receipts on %s storage read denial, not ordinary auth',
	async (store) => {
		const { send, api } = setup();
		const before = (await captureAuthFence())!;
		if (store === 'local') Object.defineProperty(window, 'localStorage', { get: refuse(denied) });
		else vi.spyOn(caches, 'keys').mockRejectedValue(denied());
		expect(await before.run(() => true)).toBeUndefined();
		expect(await captureAuthFence()).toBeUndefined();
		await api.logout();
		expect(send).toHaveBeenCalledTimes(1);
	}
);

it.each(['local', 'cache', 'lock retry'])(
	'withholds fresh cross-tab receipts through fallback writes (%s)',
	async (guard) => {
		const { localStorage } = setup();
		const write = localStorage.setItem.getMockImplementation()!;
		if (guard !== 'local') localStorage.setItem.mockImplementation(refuse(full));
		if (guard === 'lock retry') vi.spyOn(caches, 'open').mockRejectedValue(full());
		const first = await startLogout(true);
		try {
			expect(await caches.keys()).toHaveLength(guard === 'cache' ? 1 : 0);
			localStorage.setItem.mockImplementation(write);
			expect(await captureAuthFence()).toBeUndefined();
			if (guard !== 'lock retry') {
				const second = await startLogout(true);
				await second.finish();
				expect(second.fetchFn).toHaveBeenCalledTimes(1);
				expect(await captureAuthFence()).toBeUndefined();
			}
		} finally {
			await first.finish();
		}
		expect(first.fetchFn).toHaveBeenCalledTimes(1);
		expect((await captureAuthFence())?.isCurrent()).toBe(true);
		expect(await caches.keys()).toHaveLength(0);
	}
);

it.each([true, false])(
	'guards locked writes when generation storage fails (recovers: %s)',
	async (recovers) => {
		const { localStorage } = setup();
		const before = await captureAuthFence();
		expect(before?.isCurrent()).toBe(true);
		const write = localStorage.setItem.getMockImplementation()!;
		localStorage.setItem.mockImplementation(refuse(denied));
		const removal = vi.spyOn(localStorage, 'removeItem').mockImplementation(refuse(denied));
		const writer = await startLogout();
		if (recovers) {
			localStorage.setItem.mockImplementation(write);
			removal.mockRestore();
		}
		await writer.finish();
		expect(writer.fetchFn).toHaveBeenCalledTimes(1);
		expect(await before?.run(() => true)).toBeUndefined();
		expect((await captureAuthFence())?.isCurrent()).toBe(recovers ? true : undefined);
	}
);

it.each(['open', 'delete'] as const)(
	'does not wait beyond the auth deadline for cache %s',
	async (operation) => {
		const { localStorage, api, send } = setup();
		vi.useFakeTimers();
		vi.spyOn(navigator.locks, 'request').mockRejectedValueOnce(denied());
		localStorage.setItem.mockImplementation(refuse(full));
		const unblock = Promise.withResolvers<void>();
		const delay =
			<T>(method: (name: string) => Promise<T>) =>
			(name: string) =>
				unblock.promise.then(() => method(name));
		const open = caches.open.bind(caches);
		const remove = caches.delete.bind(caches);
		if (operation === 'open') vi.spyOn(caches, 'open').mockImplementation(delay(open));
		else vi.spyOn(caches, 'delete').mockImplementation(delay(remove));
		const result = vi.fn();
		const logout = api.logout().then(
			() => result('ok'),
			(error: unknown) => result(error)
		);
		try {
			await vi.advanceTimersByTimeAsync(API_REQUEST_TIMEOUT_MS);
			expect(result).toHaveBeenCalledWith(
				operation === 'open' ? expect.objectContaining({ code: 'request_timeout' }) : 'ok'
			);
			if (operation === 'open') expect(result.mock.calls[0][0]).toBeInstanceOf(ApiError);
		} finally {
			unblock.resolve();
			await logout;
			await vi.advanceTimersByTimeAsync(0);
		}
		expect(send).toHaveBeenCalledTimes(operation === 'open' ? 0 : 1);
		expect(await caches.keys()).toHaveLength(0);
	}
);

it('does not resend a cookie mutation when dispatch rejects under the lock', async () => {
	const { api, send } = setup();
	send.mockRejectedValueOnce(new Error('network'));
	await expect(api.logout()).rejects.toThrow('network');
	expect(send).toHaveBeenCalledTimes(1);
});
