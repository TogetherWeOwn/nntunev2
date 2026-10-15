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

// Acceptance pins for the passkey/BYOP web flows landing (public PR #27).
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

describe('utf-8 boundaries: server limits count bytes (Rust String.len)', () => {
	// `é` is 2 bytes in UTF-8 but a single JS string unit, so a `.length`
	// check would undercount every case below while the server rejects it.
	const byteLength = (value: string): number => new TextEncoder().encode(value).length;
	// 127 × 2 + 1 = 255 bytes in 128 chars; 128 × 2 = 256 bytes in 128 chars.
	const USER_255B = 'é'.repeat(127) + 'a';
	// 512 × 2 = 1024 bytes in 512 chars; 513 × 2 = 1026 bytes in 513 chars.
	const PASS_1024B = 'é'.repeat(512);
	const PASS_1026B = 'é'.repeat(513);

	it('measures the username ceiling in bytes: 255 bytes pass, 256 bytes fail', () => {
		expect(byteLength(USER_255B)).toBe(255);
		expect(byteLength('é'.repeat(128))).toBe(256);
		expect(validateProviderConfig({ ...GOOD_FORM, username: USER_255B })).toEqual([]);
		expect(
			validateProviderConfig({ ...GOOD_FORM, username: 'é'.repeat(128) }).length
		).toBeGreaterThan(0);
	});

	it('measures the password ceiling in bytes: 1024 bytes pass, 1026 bytes fail', () => {
		expect(byteLength(PASS_1024B)).toBe(1024);
		expect(byteLength(PASS_1026B)).toBe(1026);
		expect(validateProviderConfig({ ...GOOD_FORM, password: PASS_1024B })).toEqual([]);
		expect(
			validateProviderConfig({ ...GOOD_FORM, password: PASS_1026B }).length
		).toBeGreaterThan(0);
	});

	it('measures the indexer-key ceiling in bytes: 1024 bytes pass, 1026 bytes fail', () => {
		const url = 'https://index.example.com/api';
		expect(byteLength(PASS_1024B)).toBe(1024);
		expect(byteLength(PASS_1026B)).toBe(1026);
		expect(validateIndexerUrl(url, PASS_1024B)).toBeNull();
		expect(validateIndexerUrl(url, PASS_1026B)).not.toBeNull();
	});

	it('enforces the trimmed username byte length and sends the trimmed value', () => {
		const paddedOk = `  ${USER_255B}  `;
		const paddedOver = `  ${'é'.repeat(128)}  `;
		expect(byteLength(paddedOk.trim())).toBe(255);
		expect(byteLength(paddedOver.trim())).toBe(256);
		expect(validateProviderConfig({ ...GOOD_FORM, username: paddedOk })).toEqual([]);
		expect(
			validateProviderConfig({ ...GOOD_FORM, username: paddedOver }).length
		).toBeGreaterThan(0);
		expect(toProviderTestInput({ ...GOOD_FORM, username: paddedOk }, '', '').username).toBe(
			USER_255B
		);
	});

	it('enforces the trimmed indexer-key byte length and sends the trimmed value', () => {
		const url = 'https://index.example.com/api';
		const paddedOk = `  ${PASS_1024B}  `;
		const paddedOver = `  ${PASS_1026B}  `;
		expect(byteLength(paddedOk.trim())).toBe(1024);
		expect(byteLength(paddedOver.trim())).toBe(1026);
		expect(validateIndexerUrl(url, paddedOk)).toBeNull();
		expect(validateIndexerUrl(url, paddedOver)).not.toBeNull();
		expect(
			toProviderTestInput(GOOD_FORM, url, paddedOk).indexer?.api_key
		).toBe(PASS_1024B);
	});

	it('preserves the password byte-for-byte: raw length governs, never the trimmed one', () => {
		// 2 + 510 × 2 + 2 = 1024 raw bytes; 2 + 511 × 2 + 2 = 1026 raw bytes
		// whose 1022-byte trimmed form would wrongly pass a trimming check.
		const rawOk = `  ${'é'.repeat(510)}  `;
		const rawOver = `  ${'é'.repeat(511)}  `;
		expect(byteLength(rawOk)).toBe(1024);
		expect(byteLength(rawOver)).toBe(1026);
		expect(byteLength(rawOver.trim())).toBe(1022);
		expect(validateProviderConfig({ ...GOOD_FORM, password: rawOk })).toEqual([]);
		expect(
			validateProviderConfig({ ...GOOD_FORM, password: rawOver }).length
		).toBeGreaterThan(0);
		expect(toProviderTestInput({ ...GOOD_FORM, password: rawOver }, '', '').password).toBe(
			rawOver
		);
	});
});
