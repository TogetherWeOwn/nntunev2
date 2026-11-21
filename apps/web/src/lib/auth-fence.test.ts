import { afterEach, expect, it, vi } from 'vitest';
import { ApiClient } from './api/client.js';
import { AUTH_GENERATION_KEY, AUTH_LOCK, captureAuthFence } from './auth-fence.js';

function setup() {
	const values = new Map<string, string>();
	const localStorage = {
		getItem: (key: string) => values.get(key) ?? null,
		setItem: vi.fn((key: string, value: string) => {
			values.set(key, value);
		}),
		removeItem: (key: string) => {
			values.delete(key);
		}
	};
	vi.stubGlobal('window', { localStorage });
	const send = vi.fn(async () => Response.json({}));
	return { localStorage, send, api: new ApiClient({ baseUrl: '', fetchFn: send }) };
}

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

it('shares the read lock, queues logout and recovers a new generation after a quota error', async () => {
	const { localStorage, send, api } = setup();
	const [first, second] = await Promise.all([captureAuthFence(), captureAuthFence()]);
	const initial = localStorage.getItem(AUTH_GENERATION_KEY);
	expect(initial).not.toBeNull();
	expect(localStorage.setItem).toHaveBeenCalledTimes(1);
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
	localStorage.setItem.mockImplementationOnce(() => {
		throw new DOMException('Storage denied', 'SecurityError');
	});
	expect(await captureAuthFence()).toBeUndefined();
	expect((await captureAuthFence())?.isCurrent()).toBe(true);
});

it('keeps logout available but withholds receipts when access to storage is blocked', async () => {
	const { api, send } = setup();
	vi.stubGlobal('window', {
		get localStorage() { throw new DOMException('Storage denied', 'SecurityError'); }
	});
	expect(await captureAuthFence()).toBeUndefined();
	await api.logout();
	expect(send).toHaveBeenCalledTimes(1);
	expect(await captureAuthFence()).toBeUndefined();
});

it('falls back once when the browser denies the lock before dispatch', async () => {
	const { api, send } = setup();
	vi.spyOn(navigator.locks, 'request').mockRejectedValueOnce(new DOMException('Denied', 'SecurityError'));
	await api.logout();
	expect(send).toHaveBeenCalledTimes(1);
});

it('does not resend a cookie mutation when dispatch rejects under the lock', async () => {
	const { api, send } = setup();
	send.mockRejectedValueOnce(new Error('network'));
	await expect(api.logout()).rejects.toThrow('network');
	expect(send).toHaveBeenCalledTimes(1);
});
