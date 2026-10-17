import { expect, test } from '@playwright/test';

/**
 * Deployed-build identity acceptance (test-first contract).
 *
 * Dedicated surface: GET /api/build -> exactly `{ sha }`, where sha is a
 * 40-char lowercase hex commit SHA on staging builds or the explicit string
 * `"unknown"` when no SHA is available. Never silent: the key is always
 * present, never null/empty/missing. Existing `/api/health` pins
 * (tests/smoke.spec.ts, src/lib/version.test.ts) are untouched; the health
 * assertion below is a read-only regression guard.
 */

const SHA40 = /^[0-9a-f]{40}$/;
const FORBIDDEN = /secret|token|password|api[_-]?key|private[_-]?key|begin private|bearer |localhost|\.internal|\.local\b|hostname|binding|kv_|r2_|d1_|assets/i;

test('build endpoint exposes exactly { sha } with a 40-hex sha or explicit unknown', async ({
	request
}) => {
	const response = await request.get('/api/build');

	expect(response.status(), 'GET /api/build must exist (Builder implements it)').toBe(200);
	const body = (await response.json()) as Record<string, unknown>;
	// Exactly these keys: the dedicated surface carries nothing else.
	expect(Object.keys(body).sort()).toEqual(['sha']);
	const { sha } = body;
	expect(typeof sha, 'sha is always present, never null/missing').toBe('string');
	expect((sha as string).length).toBeGreaterThan(0);
	expect(
		sha === 'unknown' || (typeof sha === 'string' && SHA40.test(sha)),
		`sha must be 40-hex or explicit "unknown", got ${JSON.stringify(sha)}`
	).toBe(true);
});

test('build endpoint never leaks secrets, hostnames, or binding material', async ({
	request
}) => {
	const response = await request.get('/api/build');
	expect(response.status()).toBe(200);
	const raw = await response.text();
	expect(raw).not.toMatch(FORBIDDEN);
	const body = JSON.parse(raw) as Record<string, unknown>;
	expect(Object.keys(body).sort()).toEqual(['sha']);
});

test('health endpoint still reports exactly { ok, version } (regression guard)', async ({
	request
}) => {
	const response = await request.get('/api/health');

	expect(response.ok()).toBe(true);
	const body = (await response.json()) as Record<string, unknown>;
	expect(Object.keys(body).sort()).toEqual(['ok', 'version']);
	expect(body.ok).toBe(true);
	expect(typeof body.version).toBe('string');
	expect((body.version as string).length).toBeGreaterThan(0);
});
