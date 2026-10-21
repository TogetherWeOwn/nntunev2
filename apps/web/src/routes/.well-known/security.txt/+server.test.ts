import { describe, expect, it } from 'vitest';
import { GET } from './+server';

describe('security.txt', () => {
	it('serves the RFC 9116 policy at the canonical path', async () => {
		const response = await GET({} as never);
		const body = await response.text();

		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')).toMatch(/^text\/plain(?:;|$)/);
		expect(body).toContain(
			'Contact: https://github.com/TogetherWeOwn/nntunev2/security/advisories/new'
		);

		const expiresLine = body.split('\n').find((line) => line.startsWith('Expires: '));
		expect(expiresLine).toBeDefined();
		const expiresValue = expiresLine!.slice('Expires: '.length);
		expect(expiresValue).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
		expect(Date.parse(expiresValue)).toBeGreaterThan(Date.now());
	});
});
