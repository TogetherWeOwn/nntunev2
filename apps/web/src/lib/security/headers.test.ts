import { describe, expect, it } from 'vitest';

import { cspSources, SECURITY_HEADERS } from './headers.js';

describe('SECURITY_HEADERS', () => {
	it('pins the transport, content-type, framing, and referrer protections', () => {
		expect(SECURITY_HEADERS).toMatchObject({
			'Strict-Transport-Security': 'max-age=31536000; includeSubDomains',
			'X-Content-Type-Options': 'nosniff',
			'X-Frame-Options': 'DENY',
			'Referrer-Policy': 'no-referrer'
		});
		expect(SECURITY_HEADERS['Strict-Transport-Security']).not.toContain('preload');
	});

	it('disables unneeded browser capabilities without changing passkey defaults', () => {
		const policy = SECURITY_HEADERS['Permissions-Policy'];

		expect(policy).toContain('camera=()');
		expect(policy).toContain('microphone=()');
		expect(policy).toContain('geolocation=()');
		expect(policy).not.toMatch(/publickey-credentials-(?:get|create)/);
	});
});

describe('cspSources', () => {
	it('uses only the application origin when no API base URL is configured', () => {
		expect(cspSources('')).toEqual(["'self'"]);
	});

	it('returns the API origin without its path', () => {
		expect(cspSources('https://api.example.com/x')).toEqual([
			"'self'",
			'https://api.example.com'
		]);
	});

	it.each(['http://api.example.com', 'not a URL'])('rejects invalid API base URL %s', (apiBaseUrl) => {
		expect(() => cspSources(apiBaseUrl)).toThrow();
	});
});
