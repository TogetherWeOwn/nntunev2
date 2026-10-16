import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError, toProviderTestInput, toVaultSaveBody } from './client.js';
import { onPaywall, type PaywallModel } from '../billing/planLimit.js';
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

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

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

	it('maps 401 to the unauthorized code', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'nope' }, 401));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).code).toBe('unauthorized');
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
