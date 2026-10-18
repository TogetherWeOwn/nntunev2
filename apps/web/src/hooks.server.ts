import { SECURITY_HEADERS } from '#lib/security/headers.js';

import type { Handle } from '@sveltejs/kit/hooks';

/**
 * Apply the security headers to every response — pages, `+server` endpoints
 * under `/api/*`, redirects, and SvelteKit's built-in error responses all
 * flow back through `resolve`. The Content-Security-Policy header itself is
 * emitted by SvelteKit's `csp` option (see `vite.config.ts`), which owns
 * the per-request nonces; only the static headers belong here. `set` (not
 * `append`) so a route can never weaken or duplicate them, and cache
 * headers set by the demo audio/artwork routes are left untouched.
 */
export const handle: Handle = async ({ event, resolve }) => {
	const response = await resolve(event);
	for (const [name, value] of Object.entries(SECURITY_HEADERS)) {
		response.headers.set(name, value);
	}
	return response;
};
