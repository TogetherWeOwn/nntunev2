import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError, toProviderTestInput, toVaultSaveBody } from './client.js';
import { onPaywall, type PaywallModel } from '../billing/planLimit.js';
import { KNOWN_ERROR_CODES } from './errors.js';
import apiErrorsRaw from '../../../../../docs/api-errors.md?raw';
// Canonical web/Worker contract, shared with the Rust handlers suite (which
// asserts every route is served). Change both sides in the same PR.
import contractRaw from '../../../../../workers/api/tests/fixtures/api-web-contract.json?raw';

interface ContractRoute {
	id: string;
	method: string;
	path: string;
	web?: string;
	input?: Record<string, unknown>;
	wire?: unknown;
}

const contract = JSON.parse(contractRaw) as { version: number; routes: ContractRoute[] };
const documentedErrorCodes = new Set(
	apiErrorsRaw.split('\n').flatMap((line) => {
		const code = line.match(/^\| `([^`]+)` \|/)?.[1];
		return code ? [code] : [];
	})
);

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

describe('API error code registry', () => {
	it('keeps the web parser codes aligned with docs/api-errors.md', () => {
		expect([...KNOWN_ERROR_CODES].sort()).toEqual([...documentedErrorCodes].sort());
	});
});

describe('toProviderTestInput', () => {
	it('trims the provider host to match validation and server rules', () => {
		expect(
			toProviderTestInput(
				{
					host: ' news.example.com ',
					port: 563,
					tls: true,
					allowInsecurePlaintext: false,
					username: 'alice',
					password: 's3cret',
					maxConnections: 4
				},
				'',
				''
			).host
		).toBe('news.example.com');
	});

	it('uses the API field names and omits an unset indexer', () => {
		expect(
			toProviderTestInput(
				{
					host: 'news.example.com',
					port: 563,
					tls: true,
					allowInsecurePlaintext: false,
					username: 'alice',
					password: 's3cret',
					maxConnections: 4
				},
				'',
				''
			)
		).toEqual({
			host: 'news.example.com',
			port: 563,
			tls: true,
			allow_insecure_plaintext: false,
			username: 'alice',
			password: 's3cret',
			max_connections: 4
		});
	});

	it('only sends the plaintext opt-in with TLS disabled and acknowledged', () => {
		const base = {
			host: 'news.example.com',
			port: 119,
			username: 'alice',
			password: 's3cret',
			maxConnections: 4
		};
		expect(
			toProviderTestInput({ ...base, tls: false, allowInsecurePlaintext: true }, '', '')
				.allow_insecure_plaintext
		).toBe(true);
		expect(
			toProviderTestInput({ ...base, tls: false, allowInsecurePlaintext: false }, '', '')
				.allow_insecure_plaintext
		).toBe(false);
		// A stale acknowledgement with TLS on must never reach the server.
		expect(
			toProviderTestInput({ ...base, tls: true, allowInsecurePlaintext: true }, '', '')
				.allow_insecure_plaintext
		).toBe(false);
	});
});

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
		expect((failure as ApiError).paywall).toBeNull();
	});

	it('shows 402 paywalls and still reads the original error body', async () => {
		const models: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));
		const fetchFn = vi.fn(async () => jsonResponse({ code: 'plan_limit_exceeded', error: 'limit reached' }, 402));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		unsubscribe();

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).message).toBe('limit reached');
		expect((failure as ApiError).code).toBe('plan_limit_exceeded');
		expect((failure as ApiError).paywall?.kind).toBe('terminal');
		expect(models).toHaveLength(1);
		expect(models[0]?.status).toBe(402);
	});

	it('shows 429 paywalls with Retry-After timing', async () => {
		const models: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));
		const fetchFn = vi.fn(
			async () =>
				new Response(JSON.stringify({ code: 'rate_limited' }), {
					status: 429,
					headers: { 'content-type': 'application/json', 'Retry-After': '45' }
				})
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		unsubscribe();

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).paywall?.kind).toBe('retryable');
		expect(models[0]?.retryAfterSeconds).toBe(45);
	});

	it('reads code, message, and retryable from the error envelope', async () => {
		const fetchFn = vi.fn(async () =>
			jsonResponse({ code: 'rate_limited', message: 'too many requests', retryable: true }, 429)
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('rate_limited');
		expect((failure as ApiError).message).toBe('too many requests');
		expect((failure as ApiError).retryable).toBe(true);
	});

	it('exposes the 402 plan-limit envelope as non-retryable', async () => {
		const fetchFn = vi.fn(async () =>
			jsonResponse({ code: 'plan_limit_exceeded', message: 'plan limit reached', retryable: false }, 402)
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect((failure as ApiError).code).toBe('plan_limit_exceeded');
		expect((failure as ApiError).retryable).toBe(false);
	});

	it('preserves server retryable:false on corrupt_content@502 where status fallback would retry', async () => {
		// `retryableFromStatus(502)` is `true`, so a client that ignored the
		// server flag and derived retryability from status would retry corrupt
		// content forever. The server flag (`false`) must win.
		const fetchFn = vi.fn(async () =>
			jsonResponse(
				{ code: 'corrupt_content', message: 'segment could not be decoded', retryable: false },
				502
			)
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect((failure as ApiError).code).toBe('corrupt_content');
		expect((failure as ApiError).message).toBe('segment could not be decoded');
		expect((failure as ApiError).retryable).toBe(false);
	});

	it('falls back generically for unknown codes and preserves server retryability', async () => {
		const fetchFn = vi.fn(async () =>
			jsonResponse({ code: 'future_code', message: 'from a newer server', retryable: false }, 429)
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect((failure as ApiError).code).toBe('request_failed');
		expect((failure as ApiError).message).toBe('from a newer server');
		expect((failure as ApiError).retryable).toBe(false);
	});

	it('maps 401 to the unauthorized code', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'nope' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('unauthorized');
	});

	it.each([
		[400, 'invalid_request'],
		[402, 'plan_limit_exceeded'],
		[403, 'forbidden'],
		[404, 'not_found'],
		[409, 'conflict'],
		[413, 'payload_too_large'],
		[416, 'range_not_satisfiable'],
		[422, 'validation_failed'],
		[429, 'rate_limited'],

		[503, 'service_unavailable'],
		[502, 'request_failed']
	])('derives legacy error code for status %i', async (status, code) => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'legacy failure' }, status));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect((failure as ApiError).code).toBe(code);
	});

	it('posts JSON bodies with the right content type', async () => {
		const seen: unknown[] = [];
		const fetchFn = (async (_url: unknown, init: unknown) => {
			seen.push(init);
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await client.post('/v1/auth/magic/start', { email: 'alice@example.com' });

		expect(seen).toEqual([
			expect.objectContaining({
				method: 'POST',
				credentials: 'include',
				headers: expect.objectContaining({ 'content-type': 'application/json' })
			})
		]);
	});

	it('puts JSON bodies with the right content type', async () => {
		const seen: unknown[] = [];
		const fetchFn = (async (_url: unknown, init: unknown) => {
			seen.push(init);
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await client.put('/v1/vault', { provider: {} });

		expect(seen).toEqual([
			expect.objectContaining({
				method: 'PUT',
				credentials: 'include',
				headers: expect.objectContaining({ 'content-type': 'application/json' })
			})
		]);
	});

	it('requests a magic link without leaking which emails exist', async () => {
		const seenUrls: unknown[] = [];
		const fetchFn = (async (url: unknown) => {
			seenUrls.push(url);
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await expect(
			client.requestMagicLink('alice@example.com')
		).resolves.toEqual({ ok: true });
		expect(seenUrls).toEqual(['https://api.example.com/v1/auth/magic/start']);
	});

	it('starts session-bound passkey reauthentication with credentials included', async () => {
		const seen: { url: unknown; init: RequestInit }[] = [];
		const fetchFn = (async (url: unknown, init: RequestInit) => {
			seen.push({ url, init });
			return jsonResponse({ challenge: 'fresh-challenge', rp_id: 'example.com', allow_credentials: [] });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await expect(client.reauthenticateStart()).resolves.toEqual({
			challenge: 'fresh-challenge',
			rp_id: 'example.com',
			allow_credentials: []
		});
		expect(seen.map(({ url }) => url)).toEqual([
			'https://api.example.com/v1/session/reauthenticate/start'
		]);
		expect(seen[0].init).toEqual(expect.objectContaining({ method: 'POST', credentials: 'include' }));
	});

	it('signs out through the session endpoint so a swapped user can recover', async () => {
		const seen: { url: unknown; init: RequestInit }[] = [];
		const fetchFn = (async (url: unknown, init: RequestInit) => {
			seen.push({ url, init });
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await expect(client.logout()).resolves.toBeUndefined();
		expect(seen.map(({ url }) => url)).toEqual(['https://api.example.com/v1/session/logout']);
		expect(seen.map(({ init }) => init.method)).toEqual(['POST']);
	});

	it('revokes all sessions with an authenticated empty JSON POST', async () => {
		const seen: { url: unknown; init: RequestInit }[] = [];
		const fetchFn = (async (url: unknown, init: RequestInit) => {
			seen.push({ url, init });
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com/', fetchFn });

		await expect(client.revokeAllSessions()).resolves.toEqual({ ok: true });
		expect(seen).toEqual([
			{
				url: 'https://api.example.com/v1/session/revoke-all',
				init: expect.objectContaining({
					method: 'POST',
					body: '{}',
					credentials: 'include',
					headers: expect.objectContaining({ 'content-type': 'application/json' })
				})
			}
		]);
	});

	it.each([401, 500])('preserves the typed ApiError for revoke-all status %i', async (status) => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'revoke failed' }, status));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.revokeAllSessions().catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(status);
		expect((failure as ApiError).code).toBe(status === 401 ? 'unauthorized' : 'request_failed');
	});

	it('nests the provider fields under the save envelope', () => {
		expect(
			toVaultSaveBody({
				host: 'news.example.com',
				port: 563,
				tls: true,
				allow_insecure_plaintext: false,
				username: 'alice',
				password: 's3cret',
				max_connections: 4
			})
		).toEqual({
			provider: {
				host: 'news.example.com',
				port: 563,
				tls: true,
				allow_insecure_plaintext: false,
				username: 'alice',
				password: 's3cret',
				max_connections: 4
			}
		});
		expect(
			toVaultSaveBody({
				host: 'news.example.com',
				port: 563,
				tls: true,
				allow_insecure_plaintext: false,
				username: 'alice',
				password: 's3cret',
				max_connections: 4,
				indexer: { url: 'https://index.example.com/api', api_key: 'indexer-key' }
			})
		).toEqual({
			provider: {
				host: 'news.example.com',
				port: 563,
				tls: true,
				allow_insecure_plaintext: false,
				username: 'alice',
				password: 's3cret',
				max_connections: 4
			},
			indexer: { url: 'https://index.example.com/api', api_key: 'indexer-key' }
		});
	});

	it('tests a provider connection and returns the redacted result', async () => {
		const seen: { url: unknown; init: { body?: string } & RequestInit }[] = [];
		const fetchFn = (async (url: unknown, init: { body?: string } & RequestInit) => {
			seen.push({ url, init });
			return jsonResponse({ ok: true, latencyMs: 120 });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const result = await client.testProviderConnection(
			toProviderTestInput(
				{
					host: 'news.example.com',
					port: 563,
					tls: true,
					allowInsecurePlaintext: false,
					username: 'alice',
					password: 's3cret',
					maxConnections: 4
				},
				'https://index.example.com/api',
				'indexer-key'
			)
		);

		expect(result).toEqual({ ok: true, latencyMs: 120 });
		expect(seen.map(({ url }) => url)).toEqual([
			'https://api.example.com/v1/vault/test'
		]);
		expect(seen.map(({ init }) => JSON.parse(String(init.body)))).toEqual([
			{
				provider: {
					host: 'news.example.com',
					port: 563,
					tls: true,
					allow_insecure_plaintext: false,
					username: 'alice',
					password: 's3cret',
					max_connections: 4
				},
				indexer: { url: 'https://index.example.com/api', api_key: 'indexer-key' }
			}
		]);
	});

	it('saves a tested provider config to the vault endpoint', async () => {
		const seen: { url: unknown; init: RequestInit }[] = [];
		const fetchFn = (async (url: unknown, init: RequestInit) => {
			seen.push({ url, init });
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await expect(
			client.saveProviderConfig(
				toProviderTestInput(
					{
						host: 'news.example.com',
						port: 563,
						tls: true,
						allowInsecurePlaintext: false,
						username: 'alice',
						password: 's3cret',
						maxConnections: 4
					},
					'',
					''
				)
			)
		).resolves.toEqual({ ok: true });
		expect(seen.map(({ url }) => url)).toEqual(['https://api.example.com/v1/vault']);
		expect(seen.map(({ init }) => init.method)).toEqual(['PUT']);
		expect(seen.map(({ init }) => JSON.parse(String(init.body)))).toEqual([
			{
				provider: {
					host: 'news.example.com',
					port: 563,
					tls: true,
					allow_insecure_plaintext: false,
					username: 'alice',
					password: 's3cret',
					max_connections: 4
				}
			}
		]);
	});

	it('matches the canonical web/Worker contract fixture', async () => {
		// The same fixture file the Rust handlers suite consumes: every
		// `web` entry pins the exact method, path and wire body the client
		// must send. Drift in either direction fails here.
		expect(contract.version).toBe(1);
		// Every body-bearing route must be pinned from the web side; a route
		// with a wire body but no `web` builder can drift silently.
		for (const route of contract.routes) {
			if (route.wire !== undefined) {
				expect(route.web, `${route.id} has a wire body but no web builder`).toBeDefined();
			}
		}
		for (const route of contract.routes) {
			if (route.web === undefined || route.input === undefined || route.wire === undefined) {
				continue;
			}
			const seen: { url: unknown; init: RequestInit }[] = [];
			const fetchFn = (async (url: unknown, init: RequestInit) => {
				seen.push({ url, init });
				return jsonResponse({ ok: true });
			}) as unknown as typeof fetch;
			const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
			if (route.web === 'requestMagicLink') {
				const email = route.input as { email: string };
				await client.requestMagicLink(email.email);
			} else if (route.web === 'finishMagicLink') {
				const token = route.input as { token: string };
				await client.finishMagicLink(token.token);
			} else if (route.web === 'registerStart') {
				const email = route.input as { email: string };
				await client.registerStart(email.email);
			} else if (route.web === 'loginStart') {
				const email = route.input as { email: string };
				await client.loginStart(email.email);
			} else if (route.web === 'registerFinish') {
				await client.registerFinish(
					route.input as unknown as Parameters<typeof client.registerFinish>[0]
				);
			} else if (route.web === 'loginFinish') {
				await client.loginFinish(
					route.input as unknown as Parameters<typeof client.loginFinish>[0]
				);
			} else if (route.web === 'reauthenticateFinish') {
				await client.reauthenticateFinish(
					route.input as unknown as Parameters<typeof client.reauthenticateFinish>[0]
				);
			} else if (route.web === 'testProviderConnection') {
				await client.testProviderConnection(
					route.input as unknown as Parameters<
						typeof client.testProviderConnection
					>[0]
				);
			} else if (route.web === 'saveProviderConfig') {
				await client.saveProviderConfig(
					route.input as unknown as Parameters<typeof client.saveProviderConfig>[0]
				);
			} else {
				throw new Error(`fixture names an unknown client method: ${route.web}`);
			}
			expect(seen).toHaveLength(1);
			expect(seen[0].url).toBe(`https://api.example.com${route.path}`);
			expect(seen[0].init.method).toBe(route.method);
			expect(JSON.parse(String(seen[0].init.body))).toEqual(route.wire);
		}
	});

	it('uses the global fetch without an Illegal-invocation receiver', async () => {
		const originalFetch = globalThis.fetch;
		// Mimic browsers/workerd: a bare global fetch throws when `this` is wrong.
		function strictFetch(this: unknown): Promise<Response> {
			if (this !== undefined && this !== globalThis) {
				throw new TypeError(
					"Illegal invocation: function called with incorrect this reference"
				);
			}
			return Promise.resolve(jsonResponse({ pong: true }));
		}
		vi.stubGlobal('fetch', strictFetch as typeof fetch);
		try {
			const client = new ApiClient({ baseUrl: 'https://api.example.com' });
			await expect(client.get('/health')).resolves.toEqual({ pong: true });
		} finally {
			vi.unstubAllGlobals();
			globalThis.fetch = originalFetch;
		}
	});
});
