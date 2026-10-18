import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';
import { parseErrorBody } from './errors.js';
import { onPaywall } from '../billing/planLimit.js';

const HTML_SENTINEL = '<html><body>gateway sentinel UNDECODABLE-BODY-7f3a</body></html>';

function htmlResponse(body: string, status: number): Response {
	return new Response(body, {
		status,
		headers: { 'content-type': 'text/html' }
	});
}

function rejectingResponse(status: number, reason: string): Response {
	return {
		ok: false,
		status,
		headers: new Headers(),
		json: async () => {
			throw new SyntaxError(reason);
		}
	} as unknown as Response;
}

describe('ApiClient non-JSON error bodies', () => {
	it('retains ApiError for an HTML gateway body without leaking markup', async () => {
		const fetchFn = vi.fn(async () => htmlResponse(HTML_SENTINEL, 502));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect(failure).not.toBeInstanceOf(SyntaxError);
		const apiError = failure as ApiError;
		expect(apiError.status).toBe(502);
		expect(apiError.code).toBe('request_failed');
		expect(apiError.message).toBe('request failed with status 502');
		expect(apiError.retryable).toBe(true);
		expect(apiError.paywall).toBeNull();
		expect(apiError.message).not.toContain('UNDECODABLE-BODY-7f3a');
		expect(apiError.message).not.toContain('<html>');
		expect(apiError.message).not.toContain('Unexpected token');
	});

	it('retains ApiError for an empty body without leaking a decode error', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 500 }));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect(failure).not.toBeInstanceOf(SyntaxError);
		const apiError = failure as ApiError;
		expect(apiError.status).toBe(500);
		expect(apiError.code).toBe('request_failed');
		expect(apiError.message).toBe('request failed with status 500');
		expect(apiError.retryable).toBe(true);
		expect(apiError.message).not.toContain('Unexpected end');
		expect(apiError.message).not.toContain('JSON');
	});

	it('retains ApiError when json() rejects without leaking the SyntaxError', async () => {
		const fetchFn = vi.fn(async () =>
			rejectingResponse(503, 'Unexpected token < in JSON at position 0')
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect(failure).not.toBeInstanceOf(SyntaxError);
		const apiError = failure as ApiError;
		expect(apiError.status).toBe(503);
		expect(apiError.code).toBe('service_unavailable');
		expect(apiError.message).toBe('request failed with status 503');
		expect(apiError.retryable).toBe(true);
		expect(apiError.message).not.toContain('Unexpected token');
		expect(apiError.message).not.toContain('<');
	});

	it('derives the 401 code from an HTML body without leaking markup', async () => {
		const fetchFn = vi.fn(async () => htmlResponse(HTML_SENTINEL, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		const apiError = failure as ApiError;
		expect(apiError.code).toBe('unauthorized');
		expect(apiError.message).toBe('request failed with status 401');
		expect(apiError.retryable).toBe(false);
		expect(apiError.message).not.toContain('UNDECODABLE-BODY-7f3a');
	});

	it('derives the 429 code and retryability from an HTML body', async () => {
		const fetchFn = vi.fn(async () => htmlResponse(HTML_SENTINEL, 429));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		const apiError = failure as ApiError;
		expect(apiError.code).toBe('rate_limited');
		expect(apiError.message).toBe('request failed with status 429');
		expect(apiError.retryable).toBe(true);
		expect(apiError.message).not.toContain('UNDECODABLE-BODY-7f3a');
	});

	it('keeps the terminal paywall for an HTML 402 without leaking markup', async () => {
		const models: { kind: string; status: number }[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));
		const fetchFn = vi.fn(async () => htmlResponse(HTML_SENTINEL, 402));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		unsubscribe();

		expect(failure).toBeInstanceOf(ApiError);
		const apiError = failure as ApiError;
		expect(apiError.code).toBe('plan_limit_exceeded');
		expect(apiError.message).toBe('request failed with status 402');
		expect(apiError.retryable).toBe(false);
		expect(apiError.paywall?.kind).toBe('terminal');
		expect(apiError.paywall?.status).toBe(402);
		expect(apiError.message).not.toContain('UNDECODABLE-BODY-7f3a');
		expect(models).toHaveLength(1);
	});

	it.each([
		['string scalar', '"oops"', 500, 'request_failed', true],
		['number scalar', '42', 500, 'request_failed', true],
		['boolean scalar', 'true', 500, 'request_failed', true],
		['null scalar', 'null', 500, 'request_failed', true],
		['array body', '[]', 500, 'request_failed', true]
	])(
		'falls back to status for %s without leaking the raw body',
		async (_label, raw, status, code, retryable) => {
			const fetchFn = vi.fn(
				async () =>
					new Response(raw, {
						status,
						headers: { 'content-type': 'application/json' }
					})
			);
			const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

			const failure = await client.get('/me').catch((error: unknown) => error);

			expect(failure).toBeInstanceOf(ApiError);
			const apiError = failure as ApiError;
			expect(apiError.code).toBe(code);
			expect(apiError.message).toBe(`request failed with status ${status}`);
			expect(apiError.retryable).toBe(retryable);
		}
	);
});

describe('parseErrorBody scalar and wrong-field-type fallback', () => {
	it('returns the status fallback for scalar bodies', () => {
		expect(parseErrorBody(null, 500)).toEqual({
			code: 'request_failed',
			message: 'request failed with status 500',
			retryable: true
		});
		expect(parseErrorBody(undefined, 404)).toEqual({
			code: 'not_found',
			message: 'request failed with status 404',
			retryable: false
		});
		expect(parseErrorBody('oops', 500)).toEqual({
			code: 'request_failed',
			message: 'request failed with status 500',
			retryable: true
		});
		expect(parseErrorBody(42, 500)).toEqual({
			code: 'request_failed',
			message: 'request failed with status 500',
			retryable: true
		});
		expect(parseErrorBody(true, 500)).toEqual({
			code: 'request_failed',
			message: 'request failed with status 500',
			retryable: true
		});
	});

	it('treats an array body without code or message as a legacy fallback', () => {
		expect(parseErrorBody([], 500)).toEqual({
			code: 'request_failed',
			message: 'request failed with status 500',
			retryable: true
		});
	});

	it('ignores wrong-field-type code, message and retryable', () => {
		expect(parseErrorBody({ code: 42, message: 42, retryable: 'yes' }, 400)).toEqual(
			{
				code: 'invalid_request',
				message: 'request failed with status 400',
				retryable: false
			}
		);
	});

	it('keeps a valid string message when the code has the wrong type', () => {
		expect(
			parseErrorBody({ code: 42, message: 'kept message', retryable: 'yes' }, 400)
		).toEqual({
			code: 'invalid_request',
			message: 'kept message',
			retryable: false
		});
	});

	it('keeps the re-auth signal from the error field when the code field is mistyped', () => {
		expect(
			parseErrorBody(
				{ code: 123, error: 'provider_auth_expired', message: 'creds expired' },
				401
			)
		).toEqual({
			code: 'provider_auth_expired',
			message: 'creds expired',
			retryable: false
		});
	});

	it('prefers message over error over the generic diagnostic', () => {
		expect(parseErrorBody({ message: '', error: 'legacy boom' }, 400)).toEqual({
			code: 'invalid_request',
			message: 'legacy boom',
			retryable: false
		});
		expect(parseErrorBody({ message: 42, error: 'legacy boom' }, 400)).toEqual({
			code: 'invalid_request',
			message: 'legacy boom',
			retryable: false
		});
		// Valid string messages keep their existing precedence: message wins.
		expect(
			parseErrorBody({ message: 'direct message', error: 'legacy boom' }, 400)
		).toEqual({
			code: 'invalid_request',
			message: 'direct message',
			retryable: false
		});
	});

	it('ignores a wrong-type retryable on a known code and uses the status fallback', () => {
		expect(
			parseErrorBody({ code: 'rate_limited', message: 'slow', retryable: 'yes' }, 400)
		).toEqual({
			code: 'rate_limited',
			message: 'slow',
			retryable: false
		});
	});

	it('keeps the server message for an unknown string code without changing category', () => {
		expect(
			parseErrorBody({ code: 'future_code', message: 'from a newer server' }, 429)
		).toEqual({
			code: 'request_failed',
			message: 'from a newer server',
			retryable: true
		});
	});
});
