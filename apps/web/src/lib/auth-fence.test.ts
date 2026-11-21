import { afterEach, expect, it, vi } from 'vitest';
import { ApiClient } from './api/client.js';
import {
	AUTH_GENERATION_KEY,
	AUTH_LOCK,
	captureAuthFence
} from './auth-fence.js';

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
	const send = vi.fn(async () => Response.json({}));
	return {
		localStorage,
		send,
		api: new ApiClient({ baseUrl: '', fetchFn: send })
	};
}

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

it('shares the read lock, queues logout and recovers a new generation after a quota error', async () => {
	const { localStorage, send, api } = setup();
	const [first, second] = await Promise.all([
		captureAuthFence(),
		captureAuthFence()
	]);
	const initial = localStorage.getItem(AUTH_GENERATION_KEY);
	expect(initial).not.toBeNull();
	expect(
		localStorage.setItem.mock.calls.filter(
			([key]) => key === AUTH_GENERATION_KEY
		)
	).toHaveLength(1);
	let logout: Promise<void> | undefined;
	try {
		const read = await first!.run(async () => {
			expect(await second!.run(() => true)).toBe(true);
			localStorage.setItem.mockImplementationOnce(() => {
				throw new DOMException('Storage full', 'QuotaExceededError');
			});
			send.mockImplementationOnce(async () => {
				expect((await navigator.locks.query()).held).toContainEqual(
					expect.objectContaining({ name: AUTH_LOCK, mode: 'exclusive' })
				);
				return Response.json({});
			});
			logout = api.logout();
			expect((await navigator.locks.query()).pending).toContainEqual(
				expect.objectContaining({ name: AUTH_LOCK, mode: 'exclusive' })
			);
			expect(send).not.toHaveBeenCalled();
			expect(first!.isCurrent()).toBe(false);
			return true;
		});
		expect(read).toBe(true);
	} finally {
		await logout;
	}
	expect(send).toHaveBeenCalledTimes(1);
	expect(localStorage.getItem(AUTH_GENERATION_KEY)).toBeNull();
	const recovered = await captureAuthFence();
	expect(localStorage.getItem(AUTH_GENERATION_KEY)).not.toBe(initial);
	expect(await recovered!.run(() => true)).toBe(true);
	const stale = vi.fn();
	expect(await first!.run(stale)).toBeUndefined();
	expect(stale).not.toHaveBeenCalled();
});

it('withholds receipts when generation initialization cannot write, and retries on the next capture', async () => {
	const { localStorage } = setup();
	localStorage.setItem
		.mockImplementationOnce(localStorage.setItem.getMockImplementation()!)
		.mockImplementationOnce(() => {
			throw new DOMException('Storage denied', 'SecurityError');
		});
	expect(await captureAuthFence()).toBeUndefined();
	expect((await captureAuthFence())?.isCurrent()).toBe(true);
});

it('keeps logout available but withholds receipts when access to storage is blocked', async () => {
	const { api, send } = setup();
	vi.stubGlobal('window', {
		get localStorage() {
			throw new DOMException('Storage denied', 'SecurityError');
		}
	});
	expect(await captureAuthFence()).toBeUndefined();
	await api.logout();
	expect(send).toHaveBeenCalledTimes(1);
	expect(await captureAuthFence()).toBeUndefined();
});

it.each(['normal', 'retry', 'exhausted', 'exhausted from empty storage'])(
	'withholds cross-tab receipts through overlapping fallback writes (%s)',
	async (quota) => {
		const exhausted = quota.startsWith('exhausted');
		const { localStorage } = setup();
		const before = await captureAuthFence();
		expect(before?.isCurrent()).toBe(true);
		const lock = vi.spyOn(navigator.locks, 'request');
		async function startLogout() {
			// Re-evaluate the modules to give each writer independent epoch and pending state.
			vi.resetModules();
			const { ApiClient: WriterClient } = await import('./api/client.js');
			const started = Promise.withResolvers<void>();
			const response = Promise.withResolvers<Response>();
			const fetchFn = vi.fn(() => {
				started.resolve();
				return response.promise;
			});
			const writer = new WriterClient({ baseUrl: '', fetchFn });
			lock.mockRejectedValueOnce(new DOMException('Denied', 'SecurityError'));
			const logout = writer.logout();
			await Promise.race([started.promise, logout]);
			return { response, logout, fetchFn };
		}

		if (quota === 'retry')
			localStorage.setItem.mockImplementationOnce(() => {
				throw new DOMException('Storage full', 'QuotaExceededError');
			});
		if (exhausted) {
			const size = (items: object) =>
				Object.entries(items).reduce(
					(sum, [key, value]) =>
						sum + (typeof value === 'string' ? key.length + value.length : 0),
					0
				);
			const limit = size(localStorage);
			if (quota === 'exhausted from empty storage')
				for (const [key, value] of Object.entries(localStorage))
					if (typeof value === 'string') localStorage.removeItem(key);
			const write = localStorage.setItem.getMockImplementation()!;
			localStorage.setItem.mockImplementation((key, value) => {
				if (size({ ...localStorage, [key]: value }) > limit)
					throw new DOMException('Storage full', 'QuotaExceededError');
				write(key, value);
			});
		}
		const first = await startLogout();
		try {
			expect(before?.isCurrent()).toBe(false);
			const trusted = vi.fn();
			expect(await before?.run(trusted)).toBeUndefined();
			expect(trusted).not.toHaveBeenCalled();
			expect(await captureAuthFence()).toBeUndefined();
			const second = await startLogout();
			try {
				first.response.resolve(Response.json({}));
				await first.logout;
				expect(await captureAuthFence()).toBeUndefined();
			} finally {
				second.response.resolve(Response.json({}));
				await second.logout;
			}
			expect(second.fetchFn).toHaveBeenCalledTimes(1);
		} finally {
			first.response.resolve(Response.json({}));
			await first.logout;
		}
		expect(first.fetchFn).toHaveBeenCalledTimes(1);
		expect(before?.isCurrent()).toBe(false);
		expect((await captureAuthFence())?.isCurrent()).toBe(
			exhausted ? undefined : true
		);
	}
);

it('does not resend a cookie mutation when dispatch rejects under the lock', async () => {
	const { api, send } = setup();
	send.mockRejectedValueOnce(new Error('network'));
	await expect(api.logout()).rejects.toThrow('network');
	expect(send).toHaveBeenCalledTimes(1);
});
