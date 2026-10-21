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
		// Pin the rolling 1-year design: a 60-second window would go stale for
		// cached checkers and a multi-year window would defeat RFC 9116
		// staleness, so assert a band around the intended 365 days.
		const DAY_MS = 24 * 60 * 60 * 1000;
		const parsed = Date.parse(expiresValue);
		expect(parsed).toBeGreaterThan(Date.now() + 180 * DAY_MS);
		expect(parsed).toBeLessThanOrEqual(Date.now() + 366 * DAY_MS);
	});
});
