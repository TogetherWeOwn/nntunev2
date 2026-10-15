import { afterEach, describe, expect, it } from 'vitest';

import { GET } from './+server.js';

/**
 * Acceptance pin for [TOG-25176](/TOG/issues/TOG-25176) (roadmap R1):
 * `GET /api/ready` must answer 200 with `{ok:true}` when ready, and 503
 * with a `Retry-After` header when not. Test files only — the Builder
 * turns these green on the build card and never edits this file.
 *
 * Contract hook for the 503 path: setting `NNTUNE_FORCE_NOT_READY=1`
 * forces the not-ready shape so the 503 contract is exercisable without
 * breaking real dependencies. The Builder implements this env check.
 */

const NOT_READY_ENV = 'NNTUNE_FORCE_NOT_READY';

afterEach(() => {
	delete process.env[NOT_READY_ENV];
});

describe('GET /api/ready contract (TOG-25176 R1)', () => {
	it('returns 200 with {ok:true} when ready', async () => {
		delete process.env[NOT_READY_ENV];

		const res = await GET({} as never);

		expect(res.status).toBe(200);
		await expect(res.json()).resolves.toEqual({ ok: true });
	});

	it('returns 503 with Retry-After and ok:false when not ready', async () => {
		process.env[NOT_READY_ENV] = '1';

		const res = await GET({} as never);

		expect(res.status).toBe(503);
		const retryAfter = res.headers.get('retry-after');
		expect(
			retryAfter !== null && retryAfter.length > 0,
			'503 readiness must carry a Retry-After header'
		).toBe(true);
		const body = (await res.json()) as Record<string, unknown>;
		expect(body.ok).toBe(false);
	});

	it('answers only 200 or 503 with a JSON body', async () => {
		for (const forced of [undefined, '1'] as const) {
			if (forced === undefined) delete process.env[NOT_READY_ENV];
			else process.env[NOT_READY_ENV] = forced;

			const res = await GET({} as never);

			expect([200, 503]).toContain(res.status);
			expect(res.headers.get('content-type')).toMatch(/application\/json/);
		}
	});
});
