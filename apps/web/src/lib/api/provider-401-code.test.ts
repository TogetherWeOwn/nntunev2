import { describe, expect, it } from 'vitest';

import { ApiClient, ApiError, toProviderTestInput } from './client.js';

/**
 * Forced-401 on the provider paths must surface the documented
 * `provider_auth_expired` code (shared verbatim with `workers/api`).
 *
 * Test-only card: no `src/` changes here. The first two tests FAIL until
 * the builder makes `ApiClient` map HTTP 401s from the vault provider
 * paths (`/v1/vault/test`, `/v1/vault`) to that code — e.g. path-aware
 * error mapping in `request()`. The last two tests are GREEN before the
 * build and guard existing behavior. The builder MUST NOT edit this file.
 *
 * Fixtures are synthetic only (`example.com`, synthetic passwords).
 */

const DOCUMENTED_CODE = 'provider_auth_expired';

function syntheticInput() {
	return toProviderTestInput(
		{
			host: 'news.example.com',
			port: 563,
			tls: true,
			allowInsecurePlaintext: false,
			username: 'synthetic-user',
			password: 'synthetic-provider-password'
		},
		'',
		''
	);
}

function unauthorizedOn(path: string): typeof fetch {
	return (async () => {
		void path;
		return new Response(JSON.stringify({ error: 'provider credentials expired' }), {
			status: 401,
			headers: { 'content-type': 'application/json' }
		});
	}) as unknown as typeof fetch;
}

describe('provider 401 code', () => {
	it('maps a 401 from test-connection to provider_auth_expired', async () => {
		const client = new ApiClient({
			baseUrl: 'https://api.example.com',
			fetchFn: unauthorizedOn('/v1/vault/test')
		});

		const failure = await client
			.testProviderConnection(syntheticInput())
			.catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(401);
		expect((failure as ApiError).code).toBe(DOCUMENTED_CODE);
	});

	it('maps a 401 from vault save to provider_auth_expired', async () => {
		const client = new ApiClient({
			baseUrl: 'https://api.example.com',
			fetchFn: unauthorizedOn('/v1/vault')
		});

		const failure = await client
			.saveProviderConfig(syntheticInput())
			.catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(401);
		expect((failure as ApiError).code).toBe(DOCUMENTED_CODE);
	});

	it('keeps the generic unauthorized code for non-provider 401s', async () => {
		const fetchFn = (async () =>
			new Response(JSON.stringify({ error: 'nope' }), {
				status: 401,
				headers: { 'content-type': 'application/json' }
			})) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('unauthorized');
	});

	it('never carries credential material in the surfaced error', async () => {
		const client = new ApiClient({
			baseUrl: 'https://api.example.com',
			fetchFn: unauthorizedOn('/v1/vault/test')
		});

		const failure = (await client
			.testProviderConnection(syntheticInput())
			.catch((error: unknown) => error)) as ApiError;

		const rendered = `${failure.code} ${failure.message}`;
		expect(rendered).not.toContain('synthetic-provider-password');
		expect(rendered).not.toContain('synthetic-indexer-key');
	});
});
