import { describe, expect, it } from 'vitest';

import { decodeBase64Url } from './webauthn.js';

describe('decodeBase64Url', () => {
	it('decodes unpadded base64url to raw bytes', () => {
		// "Hello" in base64url without padding.
		expect(Array.from(decodeBase64Url('SGVsbG8'))).toEqual([72, 101, 108, 108, 111]);
	});

	it('handles the base64url alphabet (- and _)', () => {
		// 0xfb 0xff 0xfe encodes with +//+ in base64, -__- in base64url.
		expect(Array.from(decodeBase64Url('-__-'))).toEqual([251, 255, 254]);
	});

	it('decodes an empty string to empty bytes', () => {
		expect(decodeBase64Url('').length).toBe(0);
	});
});
