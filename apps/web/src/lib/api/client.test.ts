import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

describe('ApiClient', () => {
	it('joins baseUrl and path without double slashes', async () => {
		const seenUrls: unknown[] = [];
		const fetchFn = (async (url: unknown) => {
			seenUrls.push(url);
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com/', fetchFn });

		await client.get('/health');

		expect(seenUrls).toEqual(['https://api.example.com/health']);
	});

	it('sends a Bearer token when one is set', async () => {
		const seenInits: unknown[] = [];
		const fetchFn = (async (_url: unknown, init: unknown) => {
			seenInits.push(init);
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
		client.setToken('secret-token');

		await client.get('/me');

		expect(seenInits).toEqual([
			expect.objectContaining({
				headers: expect.objectContaining({ authorization: 'Bearer secret-token' })
			})
		]);
	});

	it('returns parsed JSON on success', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ user: 'ada' }));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await expect(client.get('/me')).resolves.toEqual({ user: 'ada' });
	});

	it('throws ApiError with status on error responses', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'boom' }, 500));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(500);
	});

	it('maps 401 to the unauthorized code', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'nope' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('unauthorized');
	});
});
