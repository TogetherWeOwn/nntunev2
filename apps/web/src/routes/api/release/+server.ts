import { json } from '@sveltejs/kit';
import { DEPLOYED_RELEASE_VERSION } from '$app/env/private';
import type { RequestHandler } from './$types';

// Kit 3's Cloudflare adapter supplies dynamic env rather than event.platform.
// Retain the typed platform path for adapters that provide it; neither path
// changes the baked source SHA or the package compatibility version.
export const GET: RequestHandler = ({ platform }) =>
	json(
		{ version: platform?.env?.DEPLOYED_RELEASE_VERSION ?? DEPLOYED_RELEASE_VERSION ?? null },
		{ headers: { 'cache-control': 'no-store' } }
	);
