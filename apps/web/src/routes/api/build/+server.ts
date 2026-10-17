import { json } from '@sveltejs/kit';

import { BUILD_SHA, parseBuildSha } from '#lib/build.js';

import type { RequestHandler } from './$types';

// Dedicated deployed-build identity surface (separate from /api/health).
// GET /api/build -> exactly `{ sha }`, where sha is the 40-char lowercase
// hex commit SHA baked at build time, or the explicit string "unknown"
// when the build carried no identity. The key is always present.
export const GET: RequestHandler = ({ platform }) => {
	const env = (platform as unknown as { env?: Record<string, unknown> } | undefined)?.env;
	const meta = env?.['CF_VERSION_METADATA'] as
		| { id?: unknown; tag?: unknown }
		| undefined;
	const raw = env?.['BUILD_SHA'] ?? BUILD_SHA;
	const sha = parseBuildSha(raw);
	if (sha !== 'unknown') return json({ sha });
	// Workers version fields are a last resort; parse keeps the fallback
	// explicit when they carry no commit identity.
	return json({ sha: parseBuildSha(meta?.id ?? meta?.tag) });
};
