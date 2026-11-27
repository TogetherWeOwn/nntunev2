import { defineEnvVars } from '@sveltejs/kit/env';
import { cspSources } from './lib/security/headers.js';

function parseApiBaseUrl(value = ''): string {
	if (value === '') return value;
	const invalid = () => new Error('Use an absolute HTTPS API base URL without credentials, a query, or a fragment.');
	// Reject syntax the URL parser could silently repair or discard before
	// clients append their endpoint paths. Keep supported path prefixes.
	const authority = /^https?:\/\/([^/]+)/i.exec(value)?.[1];
	if (!authority || authority.includes('@') || /[\\?#\s\p{Cc}]/u.test(value)) throw invalid();
	try {
		cspSources(value, { allowHttpLoopback: import.meta.env.DEV });
	} catch {
		throw invalid();
	}
	return value;
}

/**
 * Public runtime environment for the web frontend. Declared here (SvelteKit 3
 * `defineEnvVars`) so `$app/env/public` is typed; consumed by the app
 * through {@link ApiClient}.
 *
 * `PUBLIC_API_BASE_URL` is empty by default, which makes the client use
 * same-origin relative URLs (e.g. `/v1/auth/magic/start`). Set it to the
 * `workers/api` URL when the API deploys separately (wrangler `[vars]` or
 * platform env). A non-empty value must also be present in the **build**
 * environment: the Content-Security-Policy's `connect-src`/`media-src` are
 * derived from it at build time (`cspSources` in `src/lib/security/headers.ts`,
 * wired in `vite.config.ts`), so a runtime-only override would have the
 * browser block the cross-origin calls this value enables. ApiClient includes credentials so the HttpOnly session cookie
 * is sent. For a different origin, the API must allow the exact frontend origin
 * with credentialed CORS (never `*`); because the cookie is SameSite=Lax, the
 * frontend and API must also be same-site over HTTPS. No secrets here: public
 * vars ship to the browser.
 */
export const variables = defineEnvVars({
	PUBLIC_BROWSER_PLAYBACK: {
		public: true,
		description: 'Use the same-origin browser playback relay when set to 1.',
		schema: (value) => value === '1'
	},
	PUBLIC_API_BASE_URL: {
		public: true,
		description: 'Base URL of the workers/api worker. Empty means same-origin.',
		schema: parseApiBaseUrl
	}
});
