import { describe, expect, it } from 'vitest';

import {
	ApiClient,
	toProviderTestInput,
	type ProviderTestInput
} from './api/client.js';
import {
	validateIndexerUrl,
	validateProviderConfig,
	type ProviderForm
} from './vault-validation.js';

// Acceptance pins for TOG-25249 (Land nntunev2 PR #27: passkey/BYOP web flows).
//
// Theme: client validation is UX-only and the API (`workers/api` vault.rs) is
// authoritative — but where the client checks a trimmed/normalized value it
// must send that same value on the wire. The host-whitespace mismatch was a
// reviewed finding (fixed with `provider.host.trim()`); the cases below pin
// the same agreement for the adjacent fields and the server length ceilings,
// so a pasted value that passes client validation cannot fail server-side
// with only the generic "Connection test failed" message.
//
// RED before the Builder fix: the `wire agreement` and `server ceilings`
// blocks fail against the current implementation. The `flow contract` block
// passes and pins the fixed behavior (live test before encrypted save,
// password never normalized).

const GOOD_FORM: ProviderForm = {
	host: 'news.example.com',
	port: 563,
	tls: true,
	allowInsecurePlaintext: false,
	username: 'alice',
	password: 's3cret',
	maxConnections: 4
};

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

describe('wire agreement: validation and the wire payload use the same value', () => {
	it('sends the trimmed username, matching the emptiness check', () => {
		// validateProviderConfig judges `username.trim()`; the wire must agree.
		expect(validateProviderConfig({ ...GOOD_FORM, username: '  alice  ' })).toEqual([]);
		expect(
			toProviderTestInput({ ...GOOD_FORM, username: '  alice  ' }, '', '').username
		).toBe('alice');
	});

	it('sends the trimmed indexer API key, matching the required-key check', () => {
		// validateIndexerUrl judges `apiKey.trim()`; the wire must agree so a
		// pasted key with stray whitespace does not fail the indexer fetch.
		expect(validateIndexerUrl('https://index.example.com/api', '  indexer-key  ')).toBeNull();
		expect(
			toProviderTestInput(GOOD_FORM, 'https://index.example.com/api', '  indexer-key  ')
				.indexer?.api_key
		).toBe('indexer-key');
	});
});

describe('server ceilings: client mirrors the vault.rs length limits', () => {
	it('rejects usernames longer than the 255-character server limit', () => {
		expect(validateProviderConfig({ ...GOOD_FORM, username: 'a'.repeat(255) })).toEqual([]);
		expect(
			validateProviderConfig({ ...GOOD_FORM, username: 'a'.repeat(256) }).length
		).toBeGreaterThan(0);
	});

	it('rejects passwords longer than the 1024-character server limit', () => {
		expect(validateProviderConfig({ ...GOOD_FORM, password: 'p'.repeat(1024) })).toEqual([]);
		expect(
			validateProviderConfig({ ...GOOD_FORM, password: 'p'.repeat(1025) }).length
		).toBeGreaterThan(0);
	});

	it('rejects indexer API keys longer than the 1024-character server limit', () => {
		expect(
			validateIndexerUrl('https://index.example.com/api', 'k'.repeat(1024))
		).toBeNull();
		expect(
			validateIndexerUrl('https://index.example.com/api', 'k'.repeat(1025))
		).not.toBeNull();
	});
});

describe('flow contract: live test before encrypted vault save', () => {
	it('tests and saves through the same wire input on their own endpoints', async () => {
		const seen: Array<{ url: unknown; body: unknown }> = [];
		const fetchFn = (async (url: unknown, init: { body?: string } & RequestInit) => {
			seen.push({ url, body: JSON.parse(String(init.body)) });
			return jsonResponse({ ok: true, latencyMs: 120 });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const input: ProviderTestInput = toProviderTestInput(GOOD_FORM, '', '');
		await client.testProviderConnection(input);
		await client.saveProviderConfig(input);

		expect(seen.map((call) => call.url)).toEqual([
			'https://api.example.com/v1/vault/test',
			'https://api.example.com/v1/vault'
		]);
		// The saved config is byte-identical to the tested config: save can
		// never store values that were not the ones just tested.
		expect(seen[1]?.body).toEqual(seen[0]?.body);
	});

	it('never trims the password on the wire (credential material is exact)', () => {
		expect(
			toProviderTestInput({ ...GOOD_FORM, password: '  s3cret  ' }, '', '').password
		).toBe('  s3cret  ');
	});
});
