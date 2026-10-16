import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

describe('ApiClient provider_auth_expired', () => {
	it('maps the structured provider_auth_expired envelope to the re-auth code', async () => {
		const fetchFn = vi.fn(async () =>
			jsonResponse(
				{
					code: 'provider_auth_expired',
					message: 'provider credentials expired',
					retryable: false
				},
				401
			)
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/v1/stream').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('provider_auth_expired');
	});

	it('keeps the legacy provider_auth_expired body during migration', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'provider_auth_expired' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/v1/stream').catch((error: unknown) => error);
		expect((failure as ApiError).code).toBe('provider_auth_expired');
	});

	it('maps a 401 indexer_auth_expired body to the indexer re-key code', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'indexer_auth_expired' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/v1/vault/test').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('indexer_auth_expired');
	});

	it('maps a 401 indexer_auth_expired code field to the indexer re-key code', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ code: 'indexer_auth_expired' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/v1/vault/test').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('indexer_auth_expired');
	});

	it('keeps generic 401s as unauthorized', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'nope' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect((failure as ApiError).code).toBe('unauthorized');
	});
});
