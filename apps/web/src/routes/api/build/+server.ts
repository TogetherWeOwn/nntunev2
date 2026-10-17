import { json } from '@sveltejs/kit';

import { BUILD_SHA } from '#lib/build.js';

import type { RequestHandler } from './$types';

// Dedicated deployed-build identity surface (separate from /api/health).
// GET /api/build -> exactly `{ sha }`, where sha is the 40-char lowercase
// hex commit SHA baked at build time, or the explicit string "unknown"
// when the build carried no identity. The key is always present.
//
// The baked value is authoritative by design: a runtime var or Workers
// version field is mutable per-request/per-deploy state, so letting one
// override the bake would let an empty, invalid, or stale value mask the
// exact head this artifact was built from. Nothing sets a runtime
// BUILD_SHA (no wrangler.toml declares it), so there is no override to
// lose by serving the bake directly.
export const GET: RequestHandler = () => {
	return json({ sha: BUILD_SHA });
};
