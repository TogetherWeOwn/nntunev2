import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

// Protected acceptance spec: saved-provider management without re-entering
// secrets. The builder implements exactly these pinned client rules; the
// paired browser spec (`tests/account-provider.spec.ts`) pins the page.
//
// 1. `getSavedProviderSummary()` issues `GET /v1/vault` (no body) and
//    resolves a normalized `SavedProviderSummary`
//    `{ provider_configured, indexer_configured, updated_at }`: exactly
//    those three fields, so any extra field a (buggy or hostile) body
//    carries — password, API key, host, username — can never reach a
//    caller, the DOM, storage or a log line through this client.
// 2. The summary body is shape-validated before any caller can show
//    "configured": a plain object whose two flags are real booleans and
//    whose `updated_at` is a finite, non-negative number. Anything else
//    (string/number/null/missing flag, string/negative/non-finite/missing
//    `updated_at`, null/array/non-object body) rejects with a typed
//    `ApiError`: real HTTP status, code `request_failed`, not retryable,
//    no paywall, message free of raw body bytes. `{provider_configured:
//    "false"}` must never resolve, where truthiness would wrongly show a
//    configured provider.
// 3. `testSavedProviderConnection()` issues `POST /v1/vault/test` with
//    NO body at all (`init.body` is `undefined` — never `'{}'`): the API
//    tests the stored vault only for an exactly empty body. It sends no
//    password or indexer key and applies the same result validation as
//    the candidate test (real boolean `ok` required, invalid `latencyMs`
//    omitted, a string `error` name passed through, anything else
//    dropped). The endpoint is a reachability probe: `ok:true` must be
//    returned as-is, never upgraded into a provider-authentication claim.
// 4. `deleteSavedProviderConfig()` issues `DELETE /v1/vault` (no body),
//    exactly once (no automatic retry of the destructive write, including
//    after a timeout), and resolves only a validated summary whose flags
//    are BOTH `false`. A 2xx body still claiming a configured provider or
//    indexer, or any malformed body, rejects with the same typed shape
//    failure as rule 2 — so no caller can show "removed" on it.
// 5. HTTP failures keep the existing typed envelope on every method:
//    401 `reauth_required` stays `reauth_required` (not retryable), a
//    legacy 401 derives `unauthorized`, 403 `forbidden`, 404 `not_found`,
//    429 `rate_limited` (retryable). None of them resolve as success, and
//    each issues exactly one fetch.

const BODY_SENTINEL = 'SAVED-PROVIDER-SENTINEL-4d2b';
const BASE = 'https://api.example.com';

interface RecordedCall {
	url: string;
	method: string | undefined;
	body: unknown;
}

function clientReturning(body: string, status = 200, calls?: RecordedCall[]): ApiClient {
	const fetchFn = vi.fn(async (url: unknown, init?: RequestInit) => {
		calls?.push({ url: String(url), method: init?.method, body: init?.body });
		return new Response(body, {
			status,
			headers: { 'content-type': 'application/json' }
		});
	}) as unknown as typeof fetch;
	return new ApiClient({ baseUrl: BASE, fetchFn });
}

function expectTypedShapeFailure(failure: unknown, status = 200): void {
	expect(failure).toBeInstanceOf(ApiError);
	const apiError = failure as ApiError;
	expect(apiError.name).toBe('ApiError');
	expect(apiError.status).toBe(status);
	expect(apiError.code).toBe('request_failed');
	expect(apiError.retryable).toBe(false);
	expect(apiError.paywall).toBeNull();
	expect(apiError.message.length).toBeGreaterThan(0);
	expect(apiError.message).not.toContain(BODY_SENTINEL);
}

const CONFIGURED = JSON.stringify({
	provider_configured: true,
	indexer_configured: true,
	updated_at: 1_760_000_000
});
const NOT_CONFIGURED = JSON.stringify({
	provider_configured: false,
	indexer_configured: false,
	updated_at: 0
});

describe('getSavedProviderSummary request shape and valid summaries', () => {
	it('GETs /v1/vault with no body and resolves a configured summary unchanged', async () => {
		const calls: RecordedCall[] = [];
		const client = clientReturning(CONFIGURED, 200, calls);

		await expect(client.getSavedProviderSummary()).resolves.toEqual({
			provider_configured: true,
			indexer_configured: true,
			updated_at: 1_760_000_000
		});
		expect(calls).toEqual([{ url: `${BASE}/v1/vault`, method: 'GET', body: undefined }]);
	});

	it('resolves the not-configured summary (both false, updated_at 0)', async () => {
		const client = clientReturning(NOT_CONFIGURED);

		await expect(client.getSavedProviderSummary()).resolves.toEqual({
			provider_configured: false,
			indexer_configured: false,
			updated_at: 0
		});
	});

	it('resolves a provider-only summary with indexer_configured false', async () => {
		const client = clientReturning(
			JSON.stringify({ provider_configured: true, indexer_configured: false, updated_at: 42 })
		);

		await expect(client.getSavedProviderSummary()).resolves.toEqual({
			provider_configured: true,
			indexer_configured: false,
			updated_at: 42
		});
	});

	it('drops extra secret-bearing fields instead of passing them to the caller', async () => {
		const client = clientReturning(
			JSON.stringify({
				provider_configured: true,
				indexer_configured: false,
				updated_at: 7,
				password: BODY_SENTINEL,
				api_key: BODY_SENTINEL,
				host: 'news.example.com',
				username: 'alice'
			})
		);

		const summary = await client.getSavedProviderSummary();

		expect(summary).toEqual({
			provider_configured: true,
			indexer_configured: false,
			updated_at: 7
		});
		expect(JSON.stringify(summary)).not.toContain(BODY_SENTINEL);
		expect(summary).not.toHaveProperty('password');
		expect(summary).not.toHaveProperty('api_key');
	});
});

describe('getSavedProviderSummary rejects malformed summaries', () => {
	const malformed: Array<[string, string]> = [
		[
			'a string "false" provider flag',
			JSON.stringify({ provider_configured: 'false', indexer_configured: false, updated_at: 0 })
		],
		[
			'a string "true" provider flag',
			JSON.stringify({ provider_configured: 'true', indexer_configured: false, updated_at: 0 })
		],
		[
			'a numeric provider flag',
			JSON.stringify({ provider_configured: 1, indexer_configured: false, updated_at: 0 })
		],
		[
			'a null indexer flag',
			JSON.stringify({ provider_configured: true, indexer_configured: null, updated_at: 0 })
		],
		['a missing indexer flag', JSON.stringify({ provider_configured: true, updated_at: 0 })],
		['a missing provider flag', JSON.stringify({ indexer_configured: false, updated_at: 0 })],
		[
			'a string updated_at',
			JSON.stringify({ provider_configured: true, indexer_configured: false, updated_at: '1760' })
		],
		[
			'a negative updated_at',
			JSON.stringify({ provider_configured: true, indexer_configured: false, updated_at: -1 })
		],
		['a non-finite updated_at', '{"provider_configured":true,"indexer_configured":false,"updated_at":1e999}'],
		[
			'a null updated_at',
			JSON.stringify({ provider_configured: true, indexer_configured: false, updated_at: null })
		],
		['a missing updated_at', JSON.stringify({ provider_configured: true, indexer_configured: false })],
		['an empty object', JSON.stringify({})],
		['a null body', 'null'],
		[
			'an array body',
			JSON.stringify([{ provider_configured: true, indexer_configured: false, updated_at: 0 }])
		],
		['a bare string body', JSON.stringify(BODY_SENTINEL)],
		['a bare boolean body', 'true']
	];

	for (const [label, body] of malformed) {
		it(`rejects ${label} with the typed shape failure`, async () => {
			const client = clientReturning(body);

			const failure = await client.getSavedProviderSummary().catch((error: unknown) => error);

			expectTypedShapeFailure(failure);
		});
	}

	it('rejects a malformed summary at 201 with the real status, not 200', async () => {
		const client = clientReturning(
			JSON.stringify({ provider_configured: 'false', indexer_configured: false, updated_at: 0 }),
			201
		);

		const failure = await client.getSavedProviderSummary().catch((error: unknown) => error);

		expectTypedShapeFailure(failure, 201);
	});
});

describe('testSavedProviderConnection sends an exactly empty body', () => {
	it('POSTs /v1/vault/test with body undefined (not "{}") and resolves ok:true', async () => {
		const calls: RecordedCall[] = [];
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: 31 }), 200, calls);

		await expect(client.testSavedProviderConnection()).resolves.toEqual({
			ok: true,
			latencyMs: 31
		});
		expect(calls).toHaveLength(1);
		expect(calls[0]?.url).toBe(`${BASE}/v1/vault/test`);
		expect(calls[0]?.method).toBe('POST');
		expect(calls[0]?.body).toBeUndefined();
	});

	it('resolves ok:false with the names-only error string preserved', async () => {
		const client = clientReturning(
			JSON.stringify({ ok: false, error: 'provider_auth_expired' })
		);

		await expect(client.testSavedProviderConnection()).resolves.toEqual({
			ok: false,
			error: 'provider_auth_expired'
		});
	});

	it('omits an invalid latencyMs instead of rejecting or echoing it', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: 'fast' }));

		const result = await client.testSavedProviderConnection();

		expect(result).toEqual({ ok: true });
		expect(result).not.toHaveProperty('latencyMs');
	});

	it('rejects {ok:"true"} with the typed shape failure', async () => {
		const client = clientReturning(
			JSON.stringify({ ok: 'true', latencyMs: 5, note: BODY_SENTINEL })
		);

		const failure = await client.testSavedProviderConnection().catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a null body with the typed shape failure', async () => {
		const client = clientReturning('null');

		const failure = await client.testSavedProviderConnection().catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});
});

describe('deleteSavedProviderConfig validates removal before resolving', () => {
	it('DELETEs /v1/vault with no body and resolves the removed summary', async () => {
		const calls: RecordedCall[] = [];
		const client = clientReturning(NOT_CONFIGURED, 200, calls);

		await expect(client.deleteSavedProviderConfig()).resolves.toEqual({
			provider_configured: false,
			indexer_configured: false,
			updated_at: 0
		});
		expect(calls).toEqual([{ url: `${BASE}/v1/vault`, method: 'DELETE', body: undefined }]);
	});

	it('drops extra fields from the delete summary as from the GET summary', async () => {
		const client = clientReturning(
			JSON.stringify({
				provider_configured: false,
				indexer_configured: false,
				updated_at: 0,
				password: BODY_SENTINEL
			})
		);

		const summary = await client.deleteSavedProviderConfig();

		expect(summary).toEqual({
			provider_configured: false,
			indexer_configured: false,
			updated_at: 0
		});
		expect(JSON.stringify(summary)).not.toContain(BODY_SENTINEL);
	});

	it('rejects a 2xx body still claiming a configured provider', async () => {
		const client = clientReturning(CONFIGURED);

		const failure = await client.deleteSavedProviderConfig().catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a 2xx body still claiming a configured indexer', async () => {
		const client = clientReturning(
			JSON.stringify({ provider_configured: false, indexer_configured: true, updated_at: 9 })
		);

		const failure = await client.deleteSavedProviderConfig().catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a malformed delete body instead of resolving "removed"', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, note: BODY_SENTINEL }));

		const failure = await client.deleteSavedProviderConfig().catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});
});

describe('saved-provider methods keep the typed HTTP error envelope', () => {
	const errors: Array<[string, number, string, string, boolean]> = [
		[
			'401 reauth_required',
			401,
			JSON.stringify({ code: 'reauth_required', message: 'reauth required', retryable: false }),
			'reauth_required',
			false
		],
		['401 legacy', 401, JSON.stringify({ error: 'unauthorized' }), 'unauthorized', false],
		[
			'403 forbidden',
			403,
			JSON.stringify({ code: 'forbidden', message: 'forbidden', retryable: false }),
			'forbidden',
			false
		],
		[
			'404 not_found',
			404,
			JSON.stringify({ code: 'not_found', message: 'not found', retryable: false }),
			'not_found',
			false
		],
		[
			'429 rate_limited',
			429,
			JSON.stringify({ code: 'rate_limited', message: 'slow down', retryable: true }),
			'rate_limited',
			true
		]
	];

	for (const [label, status, body, code, retryable] of errors) {
		it(`deleteSavedProviderConfig surfaces ${label} once, never as success`, async () => {
			const calls: RecordedCall[] = [];
			const client = clientReturning(body, status, calls);

			const failure = await client.deleteSavedProviderConfig().catch((error: unknown) => error);

			expect(failure).toBeInstanceOf(ApiError);
			expect((failure as ApiError).status).toBe(status);
			expect((failure as ApiError).code).toBe(code);
			expect((failure as ApiError).retryable).toBe(retryable);
			expect(calls).toHaveLength(1);
			expect(calls[0]?.method).toBe('DELETE');
		});

		it(`getSavedProviderSummary surfaces ${label} once, never as success`, async () => {
			const calls: RecordedCall[] = [];
			const client = clientReturning(body, status, calls);

			const failure = await client.getSavedProviderSummary().catch((error: unknown) => error);

			expect(failure).toBeInstanceOf(ApiError);
			expect((failure as ApiError).status).toBe(status);
			expect((failure as ApiError).code).toBe(code);
			expect((failure as ApiError).retryable).toBe(retryable);
			expect(calls).toHaveLength(1);
		});

		it(`testSavedProviderConnection surfaces ${label} once, never as success`, async () => {
			const calls: RecordedCall[] = [];
			const client = clientReturning(body, status, calls);

			const failure = await client.testSavedProviderConnection().catch((error: unknown) => error);

			expect(failure).toBeInstanceOf(ApiError);
			expect((failure as ApiError).status).toBe(status);
			expect((failure as ApiError).code).toBe(code);
			expect((failure as ApiError).retryable).toBe(retryable);
			expect(calls).toHaveLength(1);
			expect(calls[0]?.body).toBeUndefined();
		});
	}
});

describe('saved-provider methods settle on the request deadline without retrying', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	function neverResolvingClient(calls: RecordedCall[]): ApiClient {
		const fetchFn = vi.fn((_url: unknown, init?: RequestInit) => {
			calls.push({ url: String(_url), method: init?.method, body: init?.body });
			const signal = init?.signal ?? undefined;
			return new Promise<Response>((_resolve, reject) => {
				if (signal?.aborted) {
					reject(signal.reason);
					return;
				}
				signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
			});
		}) as unknown as typeof fetch;
		return new ApiClient({ baseUrl: BASE, fetchFn });
	}

	it('a hung DELETE times out as a typed retryable ApiError after exactly one fetch', async () => {
		const calls: RecordedCall[] = [];
		const client = neverResolvingClient(calls);

		let settled = false;
		let failure: unknown;
		void client.deleteSavedProviderConfig().then(
			() => {
				settled = true;
			},
			(error: unknown) => {
				settled = true;
				failure = error;
			}
		);

		await vi.advanceTimersByTimeAsync(1_000);
		expect(settled).toBe(false);

		await vi.advanceTimersByTimeAsync(600_000);
		expect(settled).toBe(true);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).retryable).toBe(true);
		expect((failure as ApiError).paywall).toBeNull();
		expect((failure as ApiError).message).toMatch(/timed out|timeout|deadline/i);
		// The destructive write is never repeated automatically.
		expect(calls).toHaveLength(1);
		expect(calls[0]?.method).toBe('DELETE');
	});

	it('a hung stored-config test times out after exactly one body-less POST', async () => {
		const calls: RecordedCall[] = [];
		const client = neverResolvingClient(calls);

		let settled = false;
		let failure: unknown;
		void client.testSavedProviderConnection().then(
			() => {
				settled = true;
			},
			(error: unknown) => {
				settled = true;
				failure = error;
			}
		);

		await vi.advanceTimersByTimeAsync(600_000);
		expect(settled).toBe(true);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).message).toMatch(/timed out|timeout|deadline/i);
		expect(calls).toHaveLength(1);
		expect(calls[0]?.method).toBe('POST');
		expect(calls[0]?.body).toBeUndefined();
	});
});
