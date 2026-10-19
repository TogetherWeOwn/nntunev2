import { expect, test } from '@playwright/test';

/**
 * Cacheability contract for operational endpoints.
 *
 * `GET /api/health`, `/api/ready` and `/api/build` are per-request
 * operational signals (liveness, readiness, deployed-build identity). They
 * must never be served stale from a cache, so each responds with
 * `Cache-Control: no-store`. Bodies are pinned unchanged: no version,
 * commit SHA, hostname or binding value may be added to any of them
 * (health carries only `{ok, version}`, ready only `{ok}`, build only
 * `{sha}`).
 */

for (const route of ['/api/health', '/api/ready', '/api/build']) {
	test(`${route} responds with Cache-Control: no-store and an unchanged body shape`, async ({
		request
	}) => {
		const response = await request.get(route);

		expect(response.status(), `GET ${route} should succeed`).toBe(200);
		expect(
			response.headers()['cache-control'],
			`GET ${route} must send Cache-Control: no-store`
		).toBe('no-store');

		const body = (await response.json()) as Record<string, unknown>;
		if (route === '/api/health') {
			expect(Object.keys(body).sort()).toEqual(['ok', 'version']);
			expect(body.ok).toBe(true);
		} else if (route === '/api/ready') {
			expect(body).toEqual({ ok: true });
		} else {
			expect(Object.keys(body).sort()).toEqual(['sha']);
			expect(typeof body.sha).toBe('string');
			expect((body.sha as string).length).toBeGreaterThan(0);
		}
	});
}
