import { describe, expect, it } from 'vitest';
import { ApiClient } from './client.js';
import contractRaw from '../../../../../workers/api/tests/fixtures/api-web-contract.json?raw';

// Synthetic fixtures only. Builder must not edit this acceptance file.
interface Route {
	id: string;
	method: string;
	path: string;
	web?: string;
	input?: Record<string, unknown>;
	wire?: unknown;
}
const contract = JSON.parse(contractRaw) as { version: number; routes: Route[] };

function credentialRoute(method: string): Route {
	const matches = contract.routes.filter(
		(row) => row.method === method && (
			method === 'DELETE'
				? /^\/v1\/credentials\/[^/]+$/.test(row.path)
				: row.path === '/v1/credentials'
		)
	);
	expect(matches, `fixture must pin exactly one credentials ${method} builder`).toHaveLength(1);
	return matches[0];
}

describe('credentials golden contract acceptance', () => {
	for (const method of ['POST', 'GET', 'DELETE']) {
		it(`executes the ${method} web builder and pins its exact request`, async () => {
			// Catch a missing fixture row, a stale/missing builder, skipped bodyless
			// builder coverage, camelCase wire drift, or a credential in the URL.
			const route = credentialRoute(method);
			expect(typeof route.web).toBe('string');
			const seen: { url: string; init: RequestInit }[] = [];
			const fetchFn = (async (url: unknown, init: RequestInit) => {
				seen.push({ url: String(url), init });
				const body = method === 'POST'
					? { id: 'synthetic-id', secret: 'synthetic-display-only', key_prefix: 'synthetic', scopes: ['library:read'], expires_at: 1700003600 }
					: method === 'GET' ? { credentials: [] } : { ok: true };
				return new Response(JSON.stringify(body), { status: method === 'POST' ? 201 : 200 });
			}) as typeof fetch;
			const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
			const builder: unknown = Reflect.get(client, route.web!);
			expect(typeof builder, 'fixture names a real callable builder').toBe('function');
			if (typeof builder !== 'function') throw new Error('missing credentials builder');
			if (method === 'POST') {
				expect(route.input).toBeDefined();
				expect(Object.keys(route.wire as object).sort()).toEqual(['label', 'scopes', 'ttl_secs']);
				await builder.call(client, route.input);
			} else if (method === 'DELETE') {
				expect(route.input?.id).toBe(route.path.slice('/v1/credentials/'.length));
				await builder.call(client, route.input?.id);
			} else {
				await builder.call(client);
			}
			expect(seen).toHaveLength(1);
			expect(seen[0].url).toBe(`https://api.example.com${route.path}`);
			expect(seen[0].init.method).toBe(method);
			expect(seen[0].init.credentials).toBe('include');
			if (method === 'POST') {
				expect(JSON.parse(String(seen[0].init.body))).toEqual(route.wire);
			} else {
				expect(seen[0].init.body == null).toBe(true);
			}
		});
	}
});
