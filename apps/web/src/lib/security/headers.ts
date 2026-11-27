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

/** Keep runtime fetch destinations and the build-time CSP on the same URL rules. */
export function parseApiBaseUrl(value: string, allowHttpLoopback = false): URL | null {
	const base = value.trim();
	if (base === '') return null;

	const authority = /^https?:\/\/([^/]+)/i.exec(base)?.[1];
	if (!authority || authority.includes('@') || /[\u0000-\u0020\u007f\\?#]/.test(base) || !URL.canParse(base)) {
		throw new Error('Invalid API base URL');
	}
	const url = new URL(base);
	// URL host parsing alone allows punctuation that delimits CSP directives and policies.
	const host = url.hostname;
	if (!/^(?:[a-z0-9.-]+|\[[a-f0-9:]+\])$/i.test(host)) throw new Error('Invalid API base URL host');
	if (url.protocol === 'https:' || (allowHttpLoopback && url.protocol === 'http:' &&
		(host === 'localhost' || host === '127.0.0.1' || host === '[::1]'))) return url;
	throw new Error('API base URL must use HTTPS');
}

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
 *
 * `allowHttpLoopback` exists for local development only: `workers/api`
 * permits `http` on `localhost`/`127.0.0.1`/`::1`, and `vite.config.ts`
 * passes `true` for every non-production load so `vitest`, `vite dev` and
 * `svelte-check` do not abort at startup against a local API worker.
 * Production builds always pass `false`, so any `http:` API base —
 * loopback included — still throws and fails the build.
 */
export function cspSources(apiBaseUrl: string, options?: { allowHttpLoopback?: boolean }): string[] {
	const url = parseApiBaseUrl(apiBaseUrl, options?.allowHttpLoopback);
	return url ? ["'self'", url.origin] : ["'self'"];
}
