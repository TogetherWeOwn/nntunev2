import type { RequestHandler } from './$types';
import { artworkCacheHeaders } from '#lib/player/cacheHeaders.js';

const PLACEHOLDER_SVG = `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256" role="img" aria-label="Placeholder artwork"><rect width="256" height="256" rx="24" fill="#1f2430"/><circle cx="128" cy="104" r="40" fill="#3a4356"/><rect x="56" y="160" width="144" height="12" rx="6" fill="#3a4356"/></svg>`;

/**
 * Synthetic artwork for smoke tests: always the placeholder (the R2
 * object is "missing"), served with long-lived immutable caching.
 */
export const GET: RequestHandler = () =>
	new Response(PLACEHOLDER_SVG, {
		status: 200,
		headers: {
			...artworkCacheHeaders(),
			'content-type': 'image/svg+xml'
		}
	});
