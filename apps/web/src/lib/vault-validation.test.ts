import { describe, expect, it } from 'vitest';

import { validateIndexerUrl, validateProviderConfig } from './vault-validation.js';

// Client-side validation mirrors the API worker's vault rules for fast UX
// feedback. It is UX-only: the server re-validates everything (SSRF guard,
// envelope encryption) and is the authority.

describe('validateProviderConfig', () => {
	const good = {
		host: 'news.example.com',
		port: 563,
		tls: true,
		username: 'alice',
		password: 's3cret',
		maxConnections: 10
	};

	it('accepts a well-formed provider config', () => {
		expect(validateProviderConfig(good)).toEqual([]);
	});

	it('rejects empty hosts and hosts with scheme or path', () => {
		for (const host of ['', 'https://news.example.com/', 'news.example.com/x', 'has space.com']) {
			const errors = validateProviderConfig({ ...good, host });
			expect(errors.length, `host rejected: ${host}`).toBeGreaterThan(0);
		}
	});

	it('rejects private, loopback and link-local literal IPs', () => {
		for (const host of [
			'10.1.2.3',
			'127.0.0.1',
			'169.254.169.254',
			'192.168.1.1',
			'172.16.0.5',
			'::1'
		]) {
			const errors = validateProviderConfig({ ...good, host });
			expect(errors.length, `host rejected: ${host}`).toBeGreaterThan(0);
		}
	});

	it('rejects out-of-range ports and connection counts', () => {
		expect(validateProviderConfig({ ...good, port: 0 }).length).toBeGreaterThan(0);
		expect(validateProviderConfig({ ...good, port: 65536 }).length).toBeGreaterThan(0);
		expect(validateProviderConfig({ ...good, maxConnections: 0 }).length).toBeGreaterThan(0);
		expect(validateProviderConfig({ ...good, maxConnections: 500 }).length).toBeGreaterThan(0);
	});

	it('requires a username and a password', () => {
		expect(validateProviderConfig({ ...good, username: '' }).length).toBeGreaterThan(0);
		expect(validateProviderConfig({ ...good, password: '' }).length).toBeGreaterThan(0);
	});
});

describe('validateIndexerUrl', () => {
	it('treats an empty URL as valid (indexer is optional)', () => {
		expect(validateIndexerUrl('')).toBeNull();
	});

	it('accepts an https indexer URL', () => {
		expect(validateIndexerUrl('https://index.example.com/api')).toBeNull();
	});

	it('rejects non-https, unparsable and metadata URLs', () => {
		for (const url of [
			'http://169.254.169.254/latest/meta-data/',
			'http://127.0.0.1:8080/',
			'ftp://index.example.com/',
			'not-a-url'
		]) {
			expect(validateIndexerUrl(url), `url rejected: ${url}`).not.toBeNull();
		}
	});

	it('allows http localhost for development', () => {
		expect(validateIndexerUrl('http://localhost:8080/api')).toBeNull();
	});
});
