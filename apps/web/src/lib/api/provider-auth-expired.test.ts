import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

describe('ApiClient provider_auth_expired', () => {
	it('maps a 401 provider_auth_expired body to the re-auth code', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'provider_auth_expired' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/v1/stream').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('provider_auth_expired');
	});

	it('keeps generic 401s as unauthorized', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'nope' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect((failure as ApiError).code).toBe('unauthorized');
	});
});
