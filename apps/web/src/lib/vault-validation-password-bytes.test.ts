import { describe, expect, it } from 'vitest';

import { validateProviderConfig } from './vault-validation.js';

// Pins the provider-password ceiling to UTF-8 bytes (mirrors Rust
// String::len): 1024 bytes pass, 1025+ bytes fail, even when the character
// count stays far below the byte count.

const PASSWORD_TOO_LONG = 'Provider password must be at most 1024 UTF-8 bytes.';

const GOOD_FORM = {
	host: 'news.example.com',
	port: 563,
	tls: true,
	allowInsecurePlaintext: false,
	username: 'alice',
	password: 's3cret',
	maxConnections: 10
};

function byteLength(s: string): number {
	return new TextEncoder().encode(s).byteLength;
}

describe('provider password 1024-byte boundary', () => {
	it('accepts exactly 1024 bytes of multibyte input', () => {
		// 512 × 2 = 1024 bytes in 512 chars.
		const password = 'é'.repeat(512);
		expect(byteLength(password)).toBe(1024);
		expect(validateProviderConfig({ ...GOOD_FORM, password })).toEqual([]);
	});

	it('rejects 1025+ byte multibyte input with the 1024-byte string', () => {
		// 511 × 2 + 3 = 1025 bytes in 514 chars: over the byte ceiling
		// while far under any 1024-character reading of the limit.
		const password = `${'é'.repeat(511)}aaa`;
		expect(byteLength(password)).toBe(1025);
		expect(validateProviderConfig({ ...GOOD_FORM, password })).toContain(PASSWORD_TOO_LONG);
	});

	it('rejects 1026-byte multibyte input with the 1024-byte string', () => {
		// 513 × 2 = 1026 bytes in 513 chars.
		const password = 'é'.repeat(513);
		expect(byteLength(password)).toBe(1026);
		expect(validateProviderConfig({ ...GOOD_FORM, password })).toContain(PASSWORD_TOO_LONG);
	});

	it('holds the ASCII boundary at 1024 accept / 1025 reject', () => {
		const asciiOk = 'a'.repeat(1024);
		const asciiOver = 'a'.repeat(1025);
		expect(byteLength(asciiOk)).toBe(1024);
		expect(byteLength(asciiOver)).toBe(1025);
		expect(validateProviderConfig({ ...GOOD_FORM, password: asciiOk })).toEqual([]);
		expect(validateProviderConfig({ ...GOOD_FORM, password: asciiOver })).toContain(
			PASSWORD_TOO_LONG
		);
	});
});
