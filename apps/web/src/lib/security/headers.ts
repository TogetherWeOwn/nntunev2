/**
 * Security response headers applied to every response by the `handle` hook
 * in `src/hooks.server.ts`, plus the origin list the Content-Security-Policy
 * in `vite.config.ts` allows for API calls and media playback.
 *
 * `Referrer-Policy: no-referrer` is load-bearing: the magic-link token
 * travels in the query string (`/auth/magic?token=…`), and no outbound
 * click or third-party subresource may ever leak it. HSTS deliberately has
 * no `preload`: preload submissions are hard to reverse and are a separate
 * decision. `Permissions-Policy` turns off capabilities the app never uses;
 * it must not mention `publickey-credentials-*`, or passkey signup/login
 * would break.
 */
export const SECURITY_HEADERS: Readonly<Record<string, string>> = Object.freeze({
	'Strict-Transport-Security': 'max-age=31536000; includeSubDomains',
	'X-Content-Type-Options': 'nosniff',
	'X-Frame-Options': 'DENY',
	'Referrer-Policy': 'no-referrer',
	'Permissions-Policy': 'camera=(), microphone=(), geolocation=()'
});

/**
 * CSP source list for the configured API origin. Returns header-ready
 * sources (`'self'` plus the API origin with any path stripped).
 *
 * An empty/absent base URL means the API is same-origin (see `src/env.ts`),
 * so only `'self'` is allowed. Anything else must be an absolute `https:`
 * URL: a plaintext or malformed API base would silently widen or corrupt
 * the policy, so misconfiguration throws and fails the build instead.
 * SvelteKit's `csp` option is evaluated at build time, so
 * `PUBLIC_API_BASE_URL` must be set in the build environment — a runtime-only
 * override would leave the CSP narrower than the client.
 */
export function cspSources(apiBaseUrl: string): string[] {
	const trimmed = apiBaseUrl.trim();
	if (trimmed === '') {
		return ["'self'"];
	}

	const url = new URL(trimmed);
	if (url.protocol !== 'https:') {
		throw new Error(`API base URL must use https: for CSP sources, got ${url.protocol}`);
	}
	return ["'self'", url.origin];
}
