import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

/**
 * Test Author-owned companion to auth-journey-lifecycle.spec.ts.
 * Public contract: these five auth methods accept an optional trailing caller
 * AbortSignal, without changing their existing payload/result/deadline contract.
 * Reflect.apply lets the baseline RUN and fail behaviorally when it ignores the
 * added argument, rather than failing the typecheck on the missing overload.
 * No reauthentication, magic redemption, or session-policy changes are requested.
 */
const LOGIN_FINISH = {
	challenge: 'Y2hhbGxlbmdl',
	credentialId: 'AQ',
	authenticatorData: 'Aw',
	clientDataJson: 'Ag',
	signature: 'BA'
};
const REGISTER_FINISH = {
	challenge: 'Y2hhbGxlbmdl',
	credentialId: 'AQ',
	attestationObject: 'Aw',
	clientDataJson: 'Ag',
	magicToken: 'fixture-code'
};
const CASES = [
	{
		method: 'requestMagicLink',
		path: '/v1/auth/magic/start',
		input: 'alice@example.com',
		wire: { email: 'alice@example.com' },
		response: { ok: true }
	},
	{
		method: 'loginStart',
		path: '/v1/auth/passkey/login/start',
		input: 'alice@example.com',
		wire: { email: 'alice@example.com' },
		response: { challenge: 'Y2hhbGxlbmdl', rp_id: 'localhost', allow_credentials: [] }
	},
	{
		method: 'loginFinish',
		path: '/v1/auth/passkey/login/finish',
		input: LOGIN_FINISH,
		wire: {
			challenge: 'Y2hhbGxlbmdl', credential_id: 'AQ', authenticator_data: 'Aw',
			client_data_json: 'Ag', signature: 'BA'
		},
		response: { ok: true }
	},
	{
		method: 'registerStart',
		path: '/v1/auth/passkey/register/start',
		input: 'alice@example.com',
		wire: { email: 'alice@example.com' },
		response: { challenge: 'Y2hhbGxlbmdl', user_id: 'dXNlcg', rp_id: 'localhost' }
	},
	{
		method: 'registerFinish',
		path: '/v1/auth/passkey/register/finish',
		input: REGISTER_FINISH,
		wire: {
			challenge: 'Y2hhbGxlbmdl', credential_id: 'AQ', attestation_object: 'Aw',
			client_data_json: 'Ag', magic_token: 'fixture-code'
		},
		response: { ok: true }
	}
] as const;

function response(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status, headers: { 'content-type': 'application/json' }
	});
}
function invoke(client: ApiClient, item: (typeof CASES)[number], signal?: AbortSignal): Promise<unknown> {
	return Reflect.apply(client[item.method], client, signal ? [item.input, signal] : [item.input]);
}
function track(promise: Promise<unknown>) {
	const state = { settled: false, failure: undefined as unknown, value: undefined as unknown };
	void promise.then(
		(value) => { state.settled = true; state.value = value; },
		(error: unknown) => { state.settled = true; state.failure = error; }
	);
	return state;
}
function hangingFetch(signals: Array<AbortSignal | undefined>) {
	return vi.fn((_url: unknown, init?: RequestInit) => {
		const signal = init?.signal ?? undefined;
		signals.push(signal);
		return new Promise<Response>((_resolve, reject) => {
			if (signal?.aborted) reject(signal.reason);
			else signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
		});
	});
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

for (const item of CASES) {
	describe(`${item.method} caller cancellation`, () => {
		it('aborts the dispatched request promptly, without waiting for its deadline or replaying', async () => {
			const signals: Array<AbortSignal | undefined> = [];
			const fetchFn = hangingFetch(signals);
			const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn: fetchFn as typeof fetch });
			const controller = new AbortController();
			const state = track(invoke(client, item, controller.signal));
			await vi.advanceTimersByTimeAsync(0);
			expect(state.settled).toBe(false);
			expect(fetchFn).toHaveBeenCalledTimes(1);
			expect(signals[0]?.aborted).toBe(false);

			controller.abort();
			await vi.advanceTimersByTimeAsync(0);
			expect.soft(signals[0]?.aborted, 'caller cancellation did not reach fetch').toBe(true);
			expect.soft(state.settled, 'caller cancellation was ignored until the deadline').toBe(true);
			expect.soft(state.failure).toBeDefined();
			await vi.advanceTimersByTimeAsync(600_000);
			expect(fetchFn).toHaveBeenCalledTimes(1);
		});

		it('keeps the request deadline when a live caller signal is supplied', async () => {
			const signals: Array<AbortSignal | undefined> = [];
			const fetchFn = hangingFetch(signals);
			const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn: fetchFn as typeof fetch });
			const controller = new AbortController();
			const state = track(invoke(client, item, controller.signal));
			await vi.advanceTimersByTimeAsync(1_000);
			expect(state.settled).toBe(false);
			await vi.advanceTimersByTimeAsync(600_000);
			expect(state.settled).toBe(true);
			expect(signals[0]?.aborted).toBe(true);
			expect(controller.signal.aborted).toBe(false);
			expect(state.failure).toBeInstanceOf(ApiError);
			expect((state.failure as ApiError).message).toMatch(/timed out|timeout|deadline/i);
			expect((state.failure as ApiError).paywall).toBeNull();
			expect(fetchFn).toHaveBeenCalledTimes(1);
		});

		for (const withSignal of [false, true]) {
			it(`preserves success and wire payload ${withSignal ? 'with' : 'without'} a caller signal`, async () => {
				const fetchFn = vi.fn(async () => response(item.response));
				const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
				const controller = new AbortController();
				await expect(invoke(client, item, withSignal ? controller.signal : undefined)).resolves.toEqual(item.response);
				expect(fetchFn).toHaveBeenCalledTimes(1);
				const [url, init] = (fetchFn.mock.calls as unknown as Array<[string, RequestInit]>)[0];
				expect(url).toBe(`https://api.example.com${item.path}`);
				expect(init.method).toBe('POST');
				expect(init.credentials).toBe('include');
				expect(JSON.parse(String(init.body))).toEqual(item.wire);
				// Aborting a completed request cannot poison a later independent one.
				controller.abort();
				await expect(invoke(client, item)).resolves.toEqual(item.response);
				expect(fetchFn).toHaveBeenCalledTimes(2);
			});
		}

		for (const status of [401, 429]) {
			it(`preserves ${status} classification with a caller signal and never replays finish`, async () => {
				const fetchFn = vi.fn(async () => response(status === 429 ? { code: 'rate_limited' } : { error: 'check your email and try again' }, status));
				const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
				const failure = await invoke(client, item, new AbortController().signal).catch((error: unknown) => error);
				expect(failure).toBeInstanceOf(ApiError);
				expect((failure as ApiError).status).toBe(status);
				expect((failure as ApiError).paywall?.kind ?? null).toBe(status === 429 ? 'retryable' : null);
				await vi.advanceTimersByTimeAsync(600_000);
				expect(fetchFn).toHaveBeenCalledTimes(1);
			});
		}
	});
}
