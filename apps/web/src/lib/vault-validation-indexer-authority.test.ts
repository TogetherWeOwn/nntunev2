import { describe, expect, it } from 'vitest';

import { validateIndexerUrl } from './vault-validation.js';

// Raw-authority port forms that WHATWG normalization hides but the ingest
// fetch builder rejects. The client sends the raw trimmed string (not the
// normalized href), so these must fail here instead of passing validation
// and failing on every fetch: empty port digits normalize to an empty
// `port`, and a backslash reads as a path separator in WHATWG while Rust
// splits the authority only on `/`/`?`/`#` (split_authority). Empty digits
// and backslash-in-authority are malformed URLs, not ports, so they get the
// generic message rather than the port-specific one.

const KEY = 'indexer-key';
const INVALID_MESSAGE = 'That indexer URL does not look valid.';
const PORT_MESSAGE = 'Indexer URLs must use port 443.';

describe('validateIndexerUrl: raw-authority port forms', () => {
	it('rejects empty port digits with the generic message', () => {
		for (const url of [
			'https://index.example.com:/api',
			'https://index.example.com:/',
			'https://[2001:4860:4860::8888]:/api'
		]) {
			expect(validateIndexerUrl(url, KEY), `url rejected: ${url}`).toBe(INVALID_MESSAGE);
		}
	});

	it('rejects a backslash inside the raw authority with the generic message', () => {
		for (const url of [
			// Backslash after :443: WHATWG normalizes the default port away
			// while Rust reads port digits `443\api`.
			'https://index.example.com:443\\api',
			// Backslash before :8443: the colon lands in the WHATWG path
			// while Rust reads port digits `8443`.
			'https://index.example.com\\:8443/api'
		]) {
			expect(validateIndexerUrl(url, KEY), `url rejected: ${url}`).toBe(INVALID_MESSAGE);
		}
	});

	it('still judges pathless URLs by their explicit port', () => {
		expect(validateIndexerUrl('https://index.example.com:443', KEY)).toBeNull();
		expect(validateIndexerUrl('https://index.example.com:8443', KEY)).toBe(PORT_MESSAGE);
	});

	it('accepts leading-zero spellings of port 443', () => {
		// The fetch builder parses port digits as a u16 (parse_port) before
		// comparing against 443, so leading zeros are still port 443
		// downstream — the client must judge the numeric value, not the digit
		// string. Leading-zero non-443 ports stay rejected.
		for (const url of [
			'https://index.example.com:0443/api',
			'https://index.example.com:000443/api',
			'https://index.example.com:0443',
			'https://[2001:4860:4860::8888]:0443/api'
		]) {
			expect(validateIndexerUrl(url, KEY), `url accepted: ${url}`).toBeNull();
		}
		for (const url of [
			'https://index.example.com:080/api',
			'https://index.example.com:0444/api'
		]) {
			expect(validateIndexerUrl(url, KEY), `url rejected: ${url}`).toBe(PORT_MESSAGE);
		}
	});
});
