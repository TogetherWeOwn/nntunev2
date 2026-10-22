import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError, type ProviderTestInput } from './client.js';

// Protected acceptance spec: `testProviderConnection` must validate the
// vault test result before any caller can show "Connected".
//
// Pinned rules (the builder implements exactly these):
// 1. The success body must be a plain object whose `ok` is a real boolean.
//    A string/number/null/missing `ok`, an empty object, `null`, an array or
//    any other non-object body is NOT a successful result: it surfaces as a
//    typed `ApiError` with code `request_failed`, the real HTTP status, not
//    retryable, no paywall, and a message that does not leak the raw body.
//    In particular `{ ok: "false" }` must never reach a caller, where a
//    truthiness check on the string would wrongly show "Connected".
// 2. `latencyMs` is display-only, so an invalid one is OMITTED, not
//    rejected: when `latencyMs` is present but is not a finite, non-negative
//    number (non-numeric, negative, or non-finite), the result resolves with
//    no `latencyMs` property at all, so the caller renders plain
//    "Connected." instead of the bogus value. `ok` itself is untouched.
// 3. Valid results pass through unchanged: `{ ok: true | false }` with no
//    `latencyMs`, or with a finite, non-negative numeric `latencyMs`
//    (including `0`), resolves exactly as received.

const BODY_SENTINEL = 'VAULT-TEST-SHAPE-7c1a';

const INPUT: ProviderTestInput = {
	host: 'news.example.com',
	port: 563,
	tls: true,
	allow_insecure_plaintext: false,
	username: 'alice',
	password: 's3cret',
	max_connections: 4
};

function clientReturning(body: string, status = 200): ApiClient {
	const fetchFn = vi.fn(
		async () =>
			new Response(body, {
				status,
				headers: { 'content-type': 'application/json' }
			})
	);
	return new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
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

describe('testProviderConnection rejects malformed ok / non-object bodies', () => {
	it('rejects {ok:"false"} instead of resolving a truthy string ok', async () => {
		const client = clientReturning(
			JSON.stringify({ ok: 'false', latencyMs: 12, note: BODY_SENTINEL })
		);

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects {ok:"true"} instead of resolving a string ok', async () => {
		const client = clientReturning(JSON.stringify({ ok: 'true', latencyMs: 12 }));

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a numeric ok', async () => {
		const client = clientReturning(JSON.stringify({ ok: 1, latencyMs: 12 }));

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a null ok', async () => {
		const client = clientReturning(JSON.stringify({ ok: null, latencyMs: 12 }));

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects {} with ok missing', async () => {
		const client = clientReturning(JSON.stringify({}));

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects an object with latencyMs but no ok', async () => {
		const client = clientReturning(JSON.stringify({ latencyMs: 12 }));

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a null body', async () => {
		const client = clientReturning('null');

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects an array body', async () => {
		const client = clientReturning(JSON.stringify([{ ok: true, latencyMs: 12 }]));

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a bare string body without leaking it', async () => {
		const client = clientReturning(JSON.stringify(BODY_SENTINEL));

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a bare boolean body', async () => {
		const client = clientReturning('true');

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a bare number body', async () => {
		const client = clientReturning('42');

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects {ok:"false"} at 201 with the real status, not 200', async () => {
		const client = clientReturning(JSON.stringify({ ok: 'false' }), 201);

		const failure = await client
			.testProviderConnection(INPUT)
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure, 201);
	});
});

describe('testProviderConnection omits an invalid latencyMs (pinned rule)', () => {
	it('omits a non-numeric string latencyMs and keeps ok:true', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: '120' }));

		const result = await client.testProviderConnection(INPUT);

		expect(result).toEqual({ ok: true });
		expect(result.latencyMs).toBeUndefined();
		expect(result).not.toHaveProperty('latencyMs');
	});

	it('omits a negative latencyMs', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: -1 }));

		const result = await client.testProviderConnection(INPUT);

		expect(result).toEqual({ ok: true });
		expect(result).not.toHaveProperty('latencyMs');
	});

	it('omits a non-finite latencyMs', async () => {
		// 1e999 is valid JSON text that parses to Infinity.
		const client = clientReturning('{"ok":true,"latencyMs":1e999}');

		const result = await client.testProviderConnection(INPUT);

		expect(result).toEqual({ ok: true });
		expect(result).not.toHaveProperty('latencyMs');
	});

	it('omits a null latencyMs', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: null }));

		const result = await client.testProviderConnection(INPUT);

		expect(result).toEqual({ ok: true });
		expect(result).not.toHaveProperty('latencyMs');
	});

	it('omits a boolean latencyMs', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: true }));

		const result = await client.testProviderConnection(INPUT);

		expect(result).toEqual({ ok: true });
		expect(result).not.toHaveProperty('latencyMs');
	});

	it('omits an object latencyMs', async () => {
		const client = clientReturning(
			JSON.stringify({ ok: true, latencyMs: { ms: 120 } })
		);

		const result = await client.testProviderConnection(INPUT);

		expect(result).toEqual({ ok: true });
		expect(result).not.toHaveProperty('latencyMs');
	});

	it('omits an invalid latencyMs on an ok:false result too', async () => {
		const client = clientReturning(JSON.stringify({ ok: false, latencyMs: -5 }));

		const result = await client.testProviderConnection(INPUT);

		expect(result).toEqual({ ok: false });
		expect(result).not.toHaveProperty('latencyMs');
	});
});

describe('testProviderConnection valid-result controls', () => {
	it('passes {ok:true, latencyMs:120} through unchanged', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: 120 }));

		await expect(client.testProviderConnection(INPUT)).resolves.toEqual({
			ok: true,
			latencyMs: 120
		});
	});

	it('passes {ok:false, latencyMs:45} through unchanged', async () => {
		const client = clientReturning(JSON.stringify({ ok: false, latencyMs: 45 }));

		await expect(client.testProviderConnection(INPUT)).resolves.toEqual({
			ok: false,
			latencyMs: 45
		});
	});

	it('passes {ok:true} with no latencyMs through unchanged', async () => {
		const client = clientReturning(JSON.stringify({ ok: true }));

		await expect(client.testProviderConnection(INPUT)).resolves.toEqual({ ok: true });
	});

	it('passes {ok:false} with no latencyMs through unchanged', async () => {
		const client = clientReturning(JSON.stringify({ ok: false }));

		await expect(client.testProviderConnection(INPUT)).resolves.toEqual({ ok: false });
	});

	it('keeps a zero latencyMs (finite and non-negative)', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: 0 }));

		await expect(client.testProviderConnection(INPUT)).resolves.toEqual({
			ok: true,
			latencyMs: 0
		});
	});

	it('keeps a fractional latencyMs', async () => {
		const client = clientReturning(JSON.stringify({ ok: true, latencyMs: 12.5 }));

		await expect(client.testProviderConnection(INPUT)).resolves.toEqual({
			ok: true,
			latencyMs: 12.5
		});
	});
});
