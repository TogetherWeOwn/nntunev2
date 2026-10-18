import { describe, expect, it } from 'vitest';

import { validateProviderConfig } from './vault-validation.js';

// Pins the 255 UTF-8 byte provider-username boundary (vault-validation.ts).
// Rust String.len() counts UTF-8 bytes, so multibyte input must be measured
// with TextEncoder.byteLength, not JS string length.

const USERNAME_LIMIT_MESSAGE = 'Provider username must be at most 255 UTF-8 bytes.';

const good = {
	host: 'news.example.com',
	port: 563,
	tls: true,
	allowInsecurePlaintext: false,
	username: 'alice',
	password: 's3cret',
	maxConnections: 10
};

describe('provider username 255-byte boundary', () => {
	it('accepts ASCII input of exactly 255 bytes', () => {
		const username = 'a'.repeat(255);
		expect(new TextEncoder().encode(username).byteLength).toBe(255);
		expect(validateProviderConfig({ ...good, username })).toEqual([]);
	});

	it('rejects ASCII input of 256 bytes with the 255-byte message', () => {
		const username = 'a'.repeat(256);
		expect(new TextEncoder().encode(username).byteLength).toBe(256);
		expect(validateProviderConfig({ ...good, username })).toContain(USERNAME_LIMIT_MESSAGE);
	});

	it('accepts multibyte input of exactly 255 bytes', () => {
		// '€' is 3 UTF-8 bytes; 85 * 3 = 255 bytes but only 85 JS chars.
		const username = '€'.repeat(85);
		expect(new TextEncoder().encode(username).byteLength).toBe(255);
		expect(validateProviderConfig({ ...good, username })).toEqual([]);
	});

	it('rejects multibyte input of 257 bytes with the 255-byte message', () => {
		// 'é' is 2 UTF-8 bytes; 128 * 2 + 1 = 257 bytes but only 129 JS chars,
		// so a char-length check would wrongly accept it.
		const username = `${'é'.repeat(128)}a`;
		expect(new TextEncoder().encode(username).byteLength).toBe(257);
		expect(validateProviderConfig({ ...good, username })).toContain(USERNAME_LIMIT_MESSAGE);
	});
});
