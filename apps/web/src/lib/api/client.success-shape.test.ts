import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';
import { onPaywall, type PaywallModel } from '../billing/planLimit.js';

// Protected acceptance spec: a 2xx response whose body cannot be decoded as
// JSON must surface as the typed `ApiError` — never a bare `SyntaxError`
// escaping `response.json()` on the success path. The typed error keeps the
// real HTTP status, uses the generic documented fallback code
// (`request_failed`, as for undecodable error bodies), is not retryable and
// carries no paywall. Its message must not leak the raw body or the decode
// failure. Valid-JSON success and the existing error path are unchanged.
// Shape validation of well-formed JSON is out of scope.

const HTML_SENTINEL = '<html><body>gateway sentinel SUCCESS-SHAPE-4b8d</body></html>';

function responseWith(body: string | null, status: number, contentType?: string): Response {
	return new Response(body, {
		status,
		headers: contentType ? { 'content-type': contentType } : undefined
	});
}

function clientReturning(response: Response): ApiClient {
	const fetchFn = vi.fn(async () => response);
	return new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
}

function expectTypedSuccessFailure(failure: unknown, status: number): ApiError {
	expect(failure).toBeInstanceOf(ApiError);
	expect(failure).not.toBeInstanceOf(SyntaxError);
	const apiError = failure as ApiError;
	expect(apiError.name).toBe('ApiError');
	expect(apiError.status).toBe(status);
	expect(apiError.code).toBe('request_failed');
	expect(apiError.retryable).toBe(false);
	expect(apiError.paywall).toBeNull();
	expect(apiError.message.length).toBeGreaterThan(0);
	expect(apiError.message).not.toContain('SUCCESS-SHAPE-4b8d');
	expect(apiError.message).not.toContain('<');
	expect(apiError.message).not.toContain('Unexpected');
	return apiError;
}

describe('ApiClient non-JSON success responses', () => {
	it('throws ApiError for a 200 with an empty body', async () => {
		const client = clientReturning(responseWith('', 200, 'application/json'));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200);
	});

	it('throws ApiError for a 204 No Content where the caller expects JSON', async () => {
		const client = clientReturning(responseWith(null, 204));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 204);
	});

	it('throws ApiError for a 200 HTML body without leaking markup', async () => {
		const client = clientReturning(responseWith(HTML_SENTINEL, 200, 'text/html'));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200);
	});

	it('throws ApiError for truncated JSON on a 200 labelled application/json', async () => {
		const client = clientReturning(responseWith('{"user":', 200, 'application/json'));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200);
	});

	it('throws ApiError for an HTML body on a 201 POST success', async () => {
		const client = clientReturning(responseWith(HTML_SENTINEL, 201, 'text/html'));

		const failure = await client
			.post('/v1/auth/magic/start', { email: 'alice@example.com' })
			.catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 201);
	});

	it('throws ApiError when success decoding rejects, without leaking the reason', async () => {
		// Dual-shape stub: whichever reader the implementation uses, the body
		// is undecodable and its contents must stay out of the error.
		const response = {
			ok: true,
			status: 200,
			headers: new Headers({ 'content-type': 'text/html' }),
			json: async () => {
				throw new SyntaxError('Unexpected token < SUCCESS-SHAPE-4b8d');
			},
			text: async () => HTML_SENTINEL
		} as unknown as Response;
		const client = clientReturning(response);

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200);
	});

	it('does not emit a paywall model for a non-JSON 2xx', async () => {
		const models: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));
		const client = clientReturning(responseWith(HTML_SENTINEL, 200, 'text/html'));

		const failure = await client.get('/me').catch((error: unknown) => error);
		unsubscribe();

		expectTypedSuccessFailure(failure, 200);
		expect(models).toEqual([]);
	});
});

describe('ApiClient success-shape controls', () => {
	it('still returns parsed JSON for a valid 200 body', async () => {
		const client = clientReturning(
			responseWith(JSON.stringify({ user: 'ada' }), 200, 'application/json')
		);

		await expect(client.get('/me')).resolves.toEqual({ user: 'ada' });
	});

	it('still returns parsed JSON for a valid 201 POST body', async () => {
		const client = clientReturning(
			responseWith(JSON.stringify({ ok: true }), 201, 'application/json')
		);

		await expect(
			client.post('/v1/auth/magic/start', { email: 'alice@example.com' })
		).resolves.toEqual({ ok: true });
	});

	it('passes a valid JSON array body through unchanged', async () => {
		const client = clientReturning(
			responseWith(JSON.stringify([{ id: 1 }, { id: 2 }]), 200, 'application/json')
		);

		await expect(client.get('/items')).resolves.toEqual([{ id: 1 }, { id: 2 }]);
	});

	it('keeps the existing typed error for a JSON error response', async () => {
		const client = clientReturning(
			responseWith(JSON.stringify({ error: 'boom' }), 500, 'application/json')
		);

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(500);
		expect((failure as ApiError).message).toBe('boom');
	});

	it('keeps the status fallback for a non-JSON error response', async () => {
		const client = clientReturning(responseWith(HTML_SENTINEL, 502, 'text/html'));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		const apiError = failure as ApiError;
		expect(apiError.status).toBe(502);
		expect(apiError.code).toBe('request_failed');
		expect(apiError.message).toBe('request failed with status 502');
		expect(apiError.retryable).toBe(true);
	});
});
