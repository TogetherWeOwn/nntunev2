/**
 * Cloudflare Access service-token handling for staging.
 *
 * Staging sits behind Cloudflare Access; CI authenticates with a QA service
 * token passed as the CF-Access-Client-Id / CF-Access-Client-Secret headers.
 * Only secret NAMES appear here — values come from the environment
 * (GitHub Actions secrets on CI, shell exports locally). Nothing here may
 * log or persist a secret value.
 */

import { API, SUPPORTED_NNTP_PORTS } from './contract.js';

export interface WallProbe {
  status: number;
  headers: Record<string, string>;
  bodyText: string;
  url: string;
}

/** Extra headers to attach to every staging request (page + API). */
export function accessHeaders(): Record<string, string> {
  const id = process.env.CF_ACCESS_CLIENT_ID;
  const secret = process.env.CF_ACCESS_CLIENT_SECRET;
  if (id && secret) {
    return {
      'CF-Access-Client-Id': id,
      'CF-Access-Client-Secret': secret,
    };
  }
  if (process.env.STAGING_BASE_URL?.trim() || id || secret) {
    throw new Error(
      'Cloudflare Access misconfigured: configured staging requires both ' +
        'CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET (secret names only — never values).',
    );
  }
  return {};
}

/** Bearer header for the staging test-account seam, when configured. */
export function qaAuthHeader(): Record<string, string> {
  const token = process.env.QA_AUTH_TOKEN;
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/**
 * Same-origin gate for credentialed browser traffic (review finding: the
 * Access service token and session bearer must never go to every origin).
 * `extraHTTPHeaders` / `setExtraHTTPHeaders` attach to cross-origin
 * subresources too, so pages scope headers through this predicate in a
 * `context.route` handler instead: staging origin → attach, anything else →
 * continue unmodified. Pure function so the two-origin behavior is pinned by
 * offline self-tests without launching a browser.
 */
export function isStagingOrigin(requestUrl: string, baseURL: string): boolean {
  try {
    return new URL(requestUrl).origin === new URL(baseURL).origin;
  } catch {
    return false;
  }
}

/**
 * True iff the request is a POST to one of the API auth-start routes
 * (`API.authStarts`, exact path). Only these carry the QA bearer from a
 * browser page (TWO-434): every journey spec shares one CI runner IP, and
 * staging admits a bearer-carrying start past the per-IP start budget. The
 * bearer grants no identity there, and every other path (including the
 * native-REST bearer routes) never sees it. Pure so offline self-tests pin
 * the scope without a browser.
 */
export function isAuthStartRequest(method: string, requestUrl: string): boolean {
  if (method.toUpperCase() !== 'POST') return false;
  try {
    return (API.authStarts as readonly string[]).includes(new URL(requestUrl).pathname);
  } catch {
    return false;
  }
}

/**
 * Access-wall predicate shared by both directions of the check: a
 * wall-status code (401/403/302) TOGETHER WITH Access markers. Status and
 * markers are checked jointly so a legitimate app 401/403 or an app 302
 * is never classified as Access, and an app 200 never is either.
 */
export function isAccessWall(probe: WallProbe): boolean {
  // Match Access markers against header NAMES only. Header values can echo
  // Access identifiers back (e.g. `Access-Control-Allow-Headers:
  // CF-Access-Client-Id` on an ordinary app 403), which must never classify
  // an application response as an Access wall.
  const headerNames = Object.keys(probe.headers).join('\n');
  const location = probe.headers['location'] ?? probe.headers['Location'] ?? '';
  const redirectToAccess =
    probe.status === 302 && /cloudflareaccess\.com|\/cdn-cgi\/access/i.test(location);
  const wallStatus =
    probe.status === 401 || probe.status === 403 || probe.status === 302 || redirectToAccess;
  // Access-specific response markers only. Generic edge-challenge
  // markers (`cf-mitigated`, `cf_clearance`) are deliberately excluded: a
  // bot-management/WAF challenge is not proof of Access enforcement, and
  // accepting it would let anonymous checks pass without Access in play.
  // A generic `<title>Access...` page is excluded for the same reason: only
  // an explicit `Cloudflare Access` marker proves Access enforcement.
  const hasMarkers =
    /cf-access|cf_authorization/i.test(headerNames) ||
    /cloudflare.?access/i.test(headerNames) ||
    /cloudflare.?access/i.test(probe.bodyText) ||
    redirectToAccess;
  return wallStatus && hasMarkers;
}

/**
 * Fail closed when a response is the Cloudflare Access wall instead of the
 * app: the wall means the service token is missing or wrong. Never
 * silently treat the wall as an app response.
 * The error names only the fixed context, status and config variable
 * names: `probe.url` is never interpolated (the staging origin is not
 * scrubbed from public artifacts, and URLs can carry secret-bearing
 * components), matching the redirect diagnostics in accounts.ts.
 */
export function assertNoAccessWall(probe: WallProbe, context: string): void {
  if (isAccessWall(probe)) {
    throw new Error(
      `[${context}] hit the Cloudflare Access wall ` +
        `(HTTP ${probe.status}). Set CF_ACCESS_CLIENT_ID + ` +
        'CF_ACCESS_CLIENT_SECRET for a QA service token with staging access. ' +
        'See e2e/README.md. No secret values are logged here by design.',
    );
  }
}

/**
 * The inverse check for anonymous requests: fail unless the response IS
 * an Access-specific denial. An application answer without Access — an
 * `ok:true` 200, an app 403, an app redirect, a served app shell — means
 * anonymous traffic reached (or may have reached) the app, which is exactly
 * the failure this assertion exists to catch. Same diagnostics contract as
 * assertNoAccessWall: fixed context and status only, never the probe
 * URL, headers or body (all untrusted and potentially sensitive).
 */
export function assertAccessWall(probe: WallProbe, context: string): void {
  if (!isAccessWall(probe)) {
    throw new Error(
      `[${context}] expected Cloudflare Access denial for an anonymous request, ` +
        `got HTTP ${probe.status} without Access-specific markers: anonymous ` +
        'traffic was not proven denied by Access (names only — no values logged). ' +
        'See e2e/README.md.',
    );
  }
}

interface ApiResponseLike {
  status(): number;
  headers(): Record<string, string>;
  text(): Promise<string>;
}

/**
 * Read an API response body, probe it for the Access wall, and return the
 * text for the caller to parse. Use for JSON/error bodies (small). For
 * binary success bodies (e.g. 206 stream segments) probe headers only via
 * assertNoAccessWall with an empty bodyText instead of buffering megabytes.
 */
export async function readApiBody(
  res: ApiResponseLike,
  url: string,
  context: string,
): Promise<string> {
  const bodyText = await res.text().catch(() => '');
  assertNoAccessWall({ status: res.status(), headers: res.headers(), bodyText, url }, context);
  return bodyText;
}

/**
 * Resolve the staging base URL.
 * - Returns null when unconfigured: callers skip with a notice (pre-deploy).
 * - Throws when `required` and unconfigured: callers fail closed (post-deploy).
 */
export function stagingBaseUrl(required: 'skip' | 'fail-closed', context: string): string | null {
  const raw = (process.env.STAGING_BASE_URL || '').trim().replace(/\/+$/, '');
  if (!raw) {
    if (required === 'fail-closed') {
      throw new Error(
        `[${context}] STAGING_BASE_URL is not set. Staging is expected to be ` +
          'configured — refusing to pass silently. See e2e/README.md.',
      );
    }
    return null;
  }
  if (!/^https:\/\//i.test(raw)) {
    throw new Error(
      `[${context}] STAGING_BASE_URL must be an https:// URL, got a non-https value. ` +
        'Refusing to run against a non-TLS staging target.',
    );
  }
  accessHeaders(); // Validate the pair before any caller can use the protected URL.
  return raw;
}

/** Synthetic NNTP endpoint the BYOP wizard points at (hosted fixture server or test provider). */
export interface NntpEndpoint {
  host: string;
  port: number;
  user: string;
  // The fixture harness only supports TLS-on: syntheticNntpEndpoint rejects
  // TLS-off outright, so no endpoint with tls=false can exist here.
  tls: true;
}

/**
 * NNTP endpoint config. Host/user have fixture defaults (not secrets); the
 * password has NO default and stays in the environment. TLS defaults on.
 *
 * Fail closed on a fixture transport the BYOP wizard cannot represent:
 * only the UI allowlist ports (119 / 563) are accepted, and TLS-off is
 * rejected outright — the product allows plaintext only behind an explicit
 * in-page acknowledgement the fixture is not configured to give, so the
 * harness must never enable (or silently ignore) plaintext on its behalf.
 */
export function syntheticNntpEndpoint(): NntpEndpoint & { hasPassword: boolean } {
  const host = (process.env.SYNTHETIC_NNTP_HOST || 'synthetic-nntp').trim();
  const port = Number(process.env.SYNTHETIC_NNTP_PORT || '563');
  const user = (process.env.SYNTHETIC_NNTP_USER || 'e2e').trim();
  const tlsSetting = (process.env.SYNTHETIC_NNTP_TLS || 'on').trim().toLowerCase();
  if (!host) throw new Error('[byop] SYNTHETIC_NNTP_HOST is empty.');
  if (!Number.isInteger(port) || !SUPPORTED_NNTP_PORTS.includes(port)) {
    throw new Error(
      '[byop] SYNTHETIC_NNTP_PORT must be one of the BYOP allowlist ports: 119 or 563. ' +
        'The wizard cannot select any other port — refusing to run against an ' +
        'unrepresentable fixture transport (names only — no values logged).',
    );
  }
  if (tlsSetting === 'off') {
    throw new Error(
      '[byop] SYNTHETIC_NNTP_TLS=off requests a plaintext transport, which the fixture ' +
        'harness does not support: plaintext requires an explicit in-page acknowledgement ' +
        'the fixture is not configured to give. Run the synthetic endpoint with TLS on.',
    );
  }
  if (tlsSetting !== 'on') {
    throw new Error('[byop] SYNTHETIC_NNTP_TLS must be "on" (or unset). See e2e/README.md.');
  }
  return { host, port, user, tls: true, hasPassword: Boolean(process.env.SYNTHETIC_NNTP_PASS) };
}
