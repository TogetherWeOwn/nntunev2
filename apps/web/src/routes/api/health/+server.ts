import { json } from '@sveltejs/kit';

import { APP_VERSION } from '#lib/version.js';

import type { RequestHandler } from './$types';

// Exactly {ok, version}: version is the public package version, never an
// internal identifier (commit SHA, hostname, binding). Pinned by
// apps/web/tests/smoke.spec.ts and src/lib/version.test.ts.
//
// Per-request liveness signal: never served stale, so `no-store` (pinned by
// apps/web/tests/cache-headers.spec.ts).
export const GET: RequestHandler = () =>
	json({ ok: true, version: APP_VERSION }, { headers: { 'cache-control': 'no-store' } });
