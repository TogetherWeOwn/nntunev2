import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

const START = { challenge: 'Y2hhbGxlbmdl', user_id: 'dXNlcg', rp_id: 'localhost' };

function response(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status, headers: { 'content-type': 'application/json' }
	});
}

describe('registerStart invite validation', () => {
	it('sends the optional invite without changing the response or caller cancellation', async () => {
		const fetchFn = vi.fn(async () => response(START));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
		const controller = new AbortController();

		await expect(client.registerStart('alice@example.com', controller.signal, 'fixture-invite'))
			.resolves.toEqual(START);
		expect(fetchFn).toHaveBeenCalledTimes(1);
		const [url, init] = (fetchFn.mock.calls as unknown as Array<[string, RequestInit]>)[0];
		expect(url).toBe('https://api.example.com/v1/auth/passkey/register/start');
		expect(init.method).toBe('POST');
		expect(init.credentials).toBe('include');
		expect(JSON.parse(String(init.body))).toEqual({
			email: 'alice@example.com', invite_code: 'fixture-invite'
		});
	});

	for (const invite of [undefined, '']) {
		it(`omits an ${invite === undefined ? 'absent' : 'empty'} optional invite`, async () => {
			const fetchFn = vi.fn(async () => response(START));
			const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
			await client.registerStart('alice@example.com', undefined, invite);
			const [, init] = (fetchFn.mock.calls as unknown as Array<[string, RequestInit]>)[0];
			expect(JSON.parse(String(init.body))).toEqual({ email: 'alice@example.com' });
		});
	}

	it('preserves generic rejection without replaying or contacting finish', async () => {
		const fetchFn = vi.fn(async () => response({ error: 'check your email and try again' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
		const failure = await client.registerStart('alice@example.com', undefined, 'invalid-invite')
			.catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect(failure).toMatchObject({ status: 401, paywall: null });
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('still aborts an invite-bearing request when the caller cancels', async () => {
		const signals: Array<AbortSignal | null | undefined> = [];
		const fetchFn = vi.fn((_url: unknown, init?: RequestInit) => {
			const signal = init?.signal;
			signals.push(signal);
			return new Promise<Response>((_resolve, reject) => {
				signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
			});
		});
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn: fetchFn as typeof fetch });
		const controller = new AbortController();
		const pending = client.registerStart('alice@example.com', controller.signal, 'fixture-invite');
		const rejected = expect(pending).rejects.toBeDefined();
		controller.abort();
		await rejected;
		expect(signals[0]?.aborted).toBe(true);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});
});
