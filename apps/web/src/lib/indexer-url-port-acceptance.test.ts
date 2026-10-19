import { describe, expect, it } from 'vitest';

import { validateIndexerUrl } from './vault-validation.js';

// Acceptance contract: the indexer fetch model is 443-only, so an indexer
// URL carrying an explicit port other than 443 must fail client validation
// with a port-specific message instead of passing here and failing later at
// the server / ingest boundary. An explicit `:443` and a portless (default
// port) URL remain valid.
//
// Sensitivity: every rejected URL below is otherwise valid (https, public
// host, non-empty API key, no credentials/query/fragment), so removing the
// port check makes each rejected case return `null` and flips this suite
// red; rejecting every explicit port flips the `:443` cases red.

const KEY = 'indexer-key';
const PORT_MESSAGE = 'Indexer URLs must use port 443.';

describe('validateIndexerUrl: explicit indexer ports', () => {
	it('rejects explicit non-443 ports with the port-specific message', () => {
		for (const url of [
			'https://index.example.com:8443/api',
			'https://index.example.com:80/api',
			'https://index.example.com:8080/api',
			'https://index.example.com:1/api',
			'https://index.example.com:0/api',
			'https://index.example.com:65535/api'
		]) {
			expect(validateIndexerUrl(url, KEY), `url rejected: ${url}`).toBe(PORT_MESSAGE);
		}
	});

	it('accepts an explicit :443 port', () => {
		expect(validateIndexerUrl('https://index.example.com:443/api', KEY)).toBeNull();
		expect(validateIndexerUrl('https://index.example.com:443/api/v1', KEY)).toBeNull();
	});

	it('accepts a portless (default-port) URL', () => {
		expect(validateIndexerUrl('https://index.example.com/api', KEY)).toBeNull();
		expect(validateIndexerUrl('https://index.example.com', KEY)).toBeNull();
	});

	it('reads the port from the authority, not from colons in an IPv6 literal', () => {
		// A check that naively scans for ':' after the scheme would misread the
		// colons inside the bracketed literal as a port.
		expect(validateIndexerUrl('https://[2001:4860:4860::8888]/api', KEY)).toBeNull();
		expect(validateIndexerUrl('https://[2001:4860:4860::8888]:443/api', KEY)).toBeNull();
		expect(validateIndexerUrl('https://[2001:4860:4860::8888]:8443/api', KEY)).toBe(
			PORT_MESSAGE
		);
	});
});
