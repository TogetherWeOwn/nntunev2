import { test, expect } from '@playwright/test';
import {
  accessHeaders,
  assertAccessWall,
  assertNoAccessWall,
  stagingBaseUrl,
} from '../helpers/access.js';
import { anonymousApiGet, anonymousApiOptions, apiGet, apiPost } from '../helpers/accounts.js';

// Anonymous API Worker contract (no app session by construction): liveness
// answers 200, vault/session routes answer the generic 401, and unknown
// paths answer the 404 envelope.
//
// Preflight is an edge concern on staging (TWO-411, option 2): Cloudflare
// Access answers every OPTIONS itself with a bare 403 and never passes it
// to the Worker ("Bypass OPTIONS preflight requests to origin" stays off so
// no unauthenticated request reaches the origin). The live preflight test
// asserts that edge answer instead of the Worker's. The Worker's own
// preflight contract (empty 204 on known paths, 404 envelope otherwise, and
// the exact-Origin CORS grants) is pinned by the Rust unit tests in
// `workers/api/src/worker_entry.rs`: `render_preserves_empty_preflight_response`
// and the `cors_headers_*` tests (`cors_headers_preflight_adds_method_and_header_grants`).
//
// Sessionless construction here means the Cloudflare Access pair ONLY (no
// Authorization bearer, no cookies — the Playwright `request` fixture starts
// cookieless per test). Access itself is the staging edge: every Worker probe below
// first clears `assertNoAccessWall`, proving the response came from the
// Worker rather than the edge wall. The same-origin base serves the Worker
// paths (`/healthz`, `/v1/*`) exactly like the session specs do
// (`security-sign-out-everywhere.spec.ts`, `scoped-credentials.spec.ts`).
//
// NO file-level skip here by design (see demo-audio-contract.spec.ts): the
// offline describe pins the verdict contracts on every PR (via
// `npm run test:unit`) with no staging, while each live test skips green
// when STAGING_BASE_URL is unset and fails closed once it is configured:
// the pair is required before any request (`accessHeaders` throws on a
// half-set pair), every status is exact, and no probe has a fallback accept.
// Diagnostics name only the gate (fixed context + status) and cookie names:
// never a URL, header, or body value. Access admits the pair by issuing its
// own `CF_Authorization` cookie on every response, so the no-cookie checks
// exempt exactly that edge cookie (see ACCESS_EDGE_COOKIE).

/** Liveness probe: unauthenticated at the Worker, Access pair only at the edge. */
const HEALTHZ_PATH = '/healthz';

/** Anonymous 401 probes: one vault read plus the session mutation family. */
const ANONYMOUS_401_GET_PATHS = ['/v1/vault'] as const;
const ANONYMOUS_401_POST_PATHS = ['/v1/session/refresh', '/v1/session/revoke-all'] as const;

/** Unknown-path probe: served by no Worker route, so the catch-all 404 owns it. */
const UNKNOWN_PATH = '/v1/e2e-probe-no-such-route';

/** Known-path preflight probes (answered by Access): liveness plus the authed surface. */
const PREFLIGHT_PATHS = ['/healthz', '/v1/vault'] as const;

/**
 * The Worker's single generic anonymous-denial message, pinned in
 * `generic_auth` (`workers/api/src/handlers.rs`): every anonymous 401 body
 * is exactly `{error: <this string>}`. Any other message is a distinguishing
 * oracle; any extra key is an account-data oracle.
 */
const GENERIC_ANONYMOUS_401_MESSAGE = 'check your email and try again';

export interface DenialProbe {
  status: number;
  headers: Record<string, string>;
  bodyText: string;
  context: string;
}

/**
 * Cookie the Cloudflare Access edge issues on every response to a request
 * it admitted with the QA service-token pair (exact, case-sensitive name).
 * It is set by the edge in front of staging, not by the Worker, and carries
 * no app session, so it is the only cookie a denial may carry here.
 */
const ACCESS_EDGE_COOKIE = 'CF_Authorization';

/**
 * Absence check for app cookies on a denial. Only cookie NAMES are read (the
 * text before the first `=` of each Set-Cookie line Playwright joins with
 * newlines); values are never read or logged. Any cookie other than the
 * Access edge cookie fails, named in the error.
 */
function assertNoSetCookie(headers: Record<string, string>, context: string): void {
  const appCookies: string[] = [];
  for (const [name, value] of Object.entries(headers)) {
    if (name.toLowerCase() !== 'set-cookie') continue;
    for (const line of value.split('\n')) {
      // The name is before the first `=` of the leading name=value pair
      // (attributes follow the first `;`). No `=` there means no name:
      // never echo the raw pair.
      const pair = line.split(';', 1)[0]!;
      const eq = pair.indexOf('=');
      const cookieName = eq === -1 ? '' : pair.slice(0, eq).trim();
      if (eq === -1) appCookies.push('(malformed)');
      else if (cookieName !== ACCESS_EDGE_COOKIE) appCookies.push(cookieName || '(unnamed)');
    }
  }
  if (appCookies.length > 0) {
    throw new Error(
      `[${context}] denial must not set a cookie, got: ${appCookies.join(', ')} ` +
        '(names only — no values logged).',
    );
  }
}

/**
 * The generic anonymous 401 verdict: exactly 401, a JSON body that is
 * exactly the single `error` key carrying the generic denial message (the
 * Worker's `generic_auth` shape — never the typed `{code, message,
 * retryable}` envelope, never a per-gate message, never a superset with
 * account fields), and no cookie. Every failure names the gate (fixed
 * context + status) and key names only, never a value.
 */
export function assertGenericAnonymous401(probe: DenialProbe): void {
  const { status, headers, bodyText, context } = probe;
  if (status !== 401) {
    throw new Error(
      `[${context}] expected the generic anonymous 401, got HTTP ${status}: ` +
        'the route must fail closed for callers with no app session ' +
        '(names only — no values logged).',
    );
  }
  assertNoSetCookie(headers, context);
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error(
      `[${context}] anonymous 401 body is not JSON (${bodyText.length} chars received): ` +
        'expected the generic error shape (names only — no values logged).',
    );
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error(
      `[${context}] anonymous 401 body is not a JSON object: expected the generic error shape ` +
        '(names only — no values logged).',
    );
  }
  const record = parsed as Record<string, unknown>;
  const keys = Object.keys(record).sort();
  if (keys.join(',') !== 'error') {
    throw new Error(
      `[${context}] anonymous 401 body must be exactly the single error key, ` +
        `got keys: ${keys.join(', ') || '(empty)'}.`,
    );
  }
  if (record.error !== GENERIC_ANONYMOUS_401_MESSAGE) {
    throw new Error(
      `[${context}] anonymous 401 body carries a non-generic error message: ` +
        'expected the generic denial (names only — no values logged).',
    );
  }
}

/**
 * The staging edge preflight verdict: Cloudflare Access's bare 403 for an
 * anonymous OPTIONS, which never reaches the Worker. The Worker has no 403
 * path for OPTIONS and answers only an empty 204 or the JSON 404 envelope,
 * so any of these means the preflight reached it (or something else answered
 * it): a status other than 403, a non-HTML body, the absence of the bare
 * Cloudflare 403 page, a CORS grant (`access-control-allow-*`, set only by
 * the Worker's `cors_headers`), or a cookie. The page carries no Access
 * markers, so the live test pairs this with `assertAccessWall` on an
 * anonymous GET of the same path to prove Access is in front. Failures name
 * the gate and header names only, never a value.
 */
export function assertEdgePreflightDenial(probe: DenialProbe): void {
  const { status, headers, bodyText, context } = probe;
  if (status !== 403) {
    throw new Error(
      `[${context}] expected the edge 403 for an anonymous preflight, got HTTP ${status}: ` +
        'Access must answer OPTIONS without passing it to the Worker ' +
        '(names only — no values logged).',
    );
  }
  assertNoSetCookie(headers, context);
  const grants = Object.keys(headers).filter((name) => /^access-control-allow-/i.test(name));
  if (grants.length > 0) {
    throw new Error(
      `[${context}] edge preflight 403 carries Worker CORS grants (${grants.join(', ')}): ` +
        'the preflight reached the Worker.',
    );
  }
  const contentType = Object.entries(headers).find(
    ([name]) => name.toLowerCase() === 'content-type',
  )?.[1];
  if (!contentType || !/^text\/html\b/i.test(contentType)) {
    throw new Error(
      `[${context}] edge preflight 403 must be the Cloudflare HTML page, ` +
        'got a non-HTML content type (names only — no values logged).',
    );
  }
  if (!/<title>\s*403 Forbidden\s*<\/title>/i.test(bodyText) || !/>\s*cloudflare\s*</i.test(bodyText)) {
    throw new Error(
      `[${context}] edge preflight 403 body is not the bare Cloudflare 403 page ` +
        `(${bodyText.length} chars received; names only — no values logged).`,
    );
  }
}

/**
 * The unknown-path 404 verdict: exactly 404 with the canonical
 * `{code: not_found, message, retryable: false}` envelope (exactly those
 * three keys, mirroring the served `not_found()` pin in
 * `workers/api/src/handlers.rs`), and no cookie. Failures name the gate
 * only, never a value.
 */
export function assertNotFoundEnvelope(probe: DenialProbe): void {
  const { status, headers, bodyText, context } = probe;
  if (status !== 404) {
    throw new Error(
      `[${context}] expected the unknown-path 404, got HTTP ${status}: ` +
        'unknown worker paths must fail closed with the not_found envelope ' +
        '(names only — no values logged).',
    );
  }
  assertNoSetCookie(headers, context);
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error(
      `[${context}] 404 body is not JSON (${bodyText.length} chars received): ` +
        'expected the not_found envelope (names only — no values logged).',
    );
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error(
      `[${context}] 404 body is not a JSON object: expected the not_found envelope ` +
        '(names only — no values logged).',
    );
  }
  const record = parsed as Record<string, unknown>;
  const keys = Object.keys(record).sort();
  if (keys.join(',') !== 'code,message,retryable') {
    throw new Error(
      `[${context}] 404 body must be exactly the envelope keys (code, message, retryable), ` +
        `got keys: ${keys.join(', ') || '(empty)'}.`,
    );
  }
  if (record.code !== 'not_found') {
    throw new Error(
      `[${context}] 404 envelope code must be not_found ` +
        '(names only — no values logged).',
    );
  }
  if (record.retryable !== false) {
    throw new Error(
      `[${context}] 404 envelope must be non-retryable ` +
        '(names only — no values logged).',
    );
  }
  if (typeof record.message !== 'string' || record.message.length === 0) {
    throw new Error(
      `[${context}] 404 envelope message must be a non-empty string ` +
        '(names only — no values logged).',
    );
  }
}

test.describe('anonymous worker verdicts (offline, no staging)', () => {
  test('generic 401 verdict accepts the documented shape and rejects the rest', () => {
    const ok = {
      status: 401,
      headers: {},
      bodyText: '{"error":"check your email and try again"}',
      context: 'offline',
    };
    assertGenericAnonymous401(ok);

    for (const status of [200, 403, 404, 500]) {
      expect(() => assertGenericAnonymous401({ ...ok, status }), `status ${status} must fail`).toThrow(
        /expected the generic anonymous 401/,
      );
    }
    expect(() => assertGenericAnonymous401({ ...ok, bodyText: 'not json' })).toThrow(
      /not JSON/,
    );
    expect(() => assertGenericAnonymous401({ ...ok, bodyText: '[]' })).toThrow(
      /not a JSON object/,
    );
    expect(
      () => assertGenericAnonymous401({ ...ok, bodyText: '{"code":"unauthorized"}' }),
      'typed envelope without the generic error key must fail',
    ).toThrow(/exactly the single error key/);
    expect(
      () => assertGenericAnonymous401({ ...ok, bodyText: '{"error":""}' }),
      'empty error message must fail as non-generic',
    ).toThrow(/non-generic error message/);
    for (const key of ['code', 'message', 'retryable']) {
      const bodyText =
        key === 'retryable'
          ? '{"error":"check your email and try again","retryable":false}'
          : `{"error":"check your email and try again","${key}":"x"}`;
      expect(
        () => assertGenericAnonymous401({ ...ok, bodyText }),
        `generic error mixed with typed-envelope key '${key}' must fail`,
      ).toThrow(/exactly the single error key/);
    }
    // A distinguishing per-gate message is an existence oracle even with the
    // right keys: only the exact generic denial passes, and the failure must
    // not echo the distinguishing text.
    let distinguishing = '';
    try {
      assertGenericAnonymous401({ ...ok, bodyText: '{"error":"vault not found for this user"}' });
    } catch (error) {
      distinguishing = (error as Error).message;
    }
    expect(distinguishing, 'distinguishing message must fail').toMatch(/non-generic error message/);
    expect(distinguishing).not.toContain('vault not found for this user');
    // A superset carrying account fields is an account-data oracle even with
    // the right message: only the single error key passes.
    expect(
      () =>
        assertGenericAnonymous401({
          ...ok,
          bodyText:
            '{"error":"check your email and try again",' +
            '"provider_configured":true,"user_id":"u","updated_at":"t"}',
        }),
      'superset body with account fields must fail',
    ).toThrow(/exactly the single error key/);
    expect(
      () =>
        assertGenericAnonymous401({
          ...ok,
          bodyText: '{"error":"check your email and try again","extra":"x"}',
        }),
      'extra non-credential key must fail',
    ).toThrow(/exactly the single error key/);
    // The failure names the offending key but never its value.
    let credentialed = '';
    try {
      assertGenericAnonymous401({
        ...ok,
        bodyText: '{"error":"check your email and try again","session":"SENTINEL_SESSION_VALUE"}',
      });
    } catch (error) {
      credentialed = (error as Error).message;
    }
    expect(credentialed, 'credential-shaped keys must fail without echoing values').toMatch(
      /exactly the single error key/,
    );
    expect(credentialed).toContain('session');
    expect(credentialed).not.toContain('SENTINEL_SESSION_VALUE');
    let cookie = '';
    try {
      assertGenericAnonymous401({
        ...ok,
        headers: { 'set-cookie': '__Host-nntune-session=SENTINEL' },
      });
    } catch (error) {
      cookie = (error as Error).message;
    }
    expect(cookie, 'denials must not set cookies').toMatch(/must not set a cookie/);
    expect(cookie).not.toContain('SENTINEL');
    // The Access edge cookie alone passes (issued by Cloudflare Access for the
    // service-token pair, never by the Worker) ...
    assertGenericAnonymous401({
      ...ok,
      headers: { 'set-cookie': 'CF_Authorization=SENTINEL; Path=/; Secure; HttpOnly' },
    });
    // ... but never covers an app cookie on another Set-Cookie line, nor a
    // look-alike name.
    for (const setCookie of [
      'CF_Authorization=SENTINEL; Path=/\n__Host-nntune-session=SENTINEL; Path=/',
      'cf_authorization=SENTINEL; Path=/',
      'CF_Authorization_x=SENTINEL',
      '=SENTINEL',
      'SENTINEL',
      'CF_Authorization=SENTINEL\nSENTINEL; Path=/',
    ]) {
      let mixed = '';
      try {
        assertGenericAnonymous401({ ...ok, headers: { 'set-cookie': setCookie } });
      } catch (error) {
        mixed = (error as Error).message;
      }
      expect(mixed, 'non-Access cookies must fail').toMatch(/must not set a cookie, got: /);
      expect(mixed).not.toContain('SENTINEL');
    }
  });

  test('not_found verdict accepts the envelope and rejects the rest', () => {
    const ok = {
      status: 404,
      headers: {},
      bodyText: '{"code":"not_found","message":"not found","retryable":false}',
      context: 'offline',
    };
    assertNotFoundEnvelope(ok);

    for (const status of [200, 401, 500]) {
      expect(() => assertNotFoundEnvelope({ ...ok, status }), `status ${status} must fail`).toThrow(
        /expected the unknown-path 404/,
      );
    }
    expect(() => assertNotFoundEnvelope({ ...ok, bodyText: 'not json' })).toThrow(/not JSON/);
    expect(
      () => assertNotFoundEnvelope({ ...ok, bodyText: '{"code":"unauthorized"}' }),
      'wrong envelope keys must fail',
    ).toThrow(/exactly the envelope keys/);
    expect(
      () =>
        assertNotFoundEnvelope({
          ...ok,
          bodyText: '{"code":"unauthorized","message":"m","retryable":false}',
        }),
      'wrong code must fail',
    ).toThrow(/code must be not_found/);
    expect(
      () =>
        assertNotFoundEnvelope({
          ...ok,
          bodyText: '{"code":"not_found","message":"m","retryable":true}',
        }),
      'retryable 404 must fail',
    ).toThrow(/non-retryable/);
    expect(
      () =>
        assertNotFoundEnvelope({
          ...ok,
          bodyText: '{"code":"not_found","message":"","retryable":false}',
        }),
      'empty message must fail',
    ).toThrow(/non-empty string/);
    let cookie = '';
    try {
      assertNotFoundEnvelope({
        ...ok,
        headers: { 'set-cookie': 'a=SENTINEL' },
      });
    } catch (error) {
      cookie = (error as Error).message;
    }
    expect(cookie, 'denials must not set cookies').toMatch(/must not set a cookie/);
    expect(cookie).not.toContain('SENTINEL');
  });

  test('edge preflight verdict accepts the bare Access 403 and rejects Worker answers', () => {
    const ok = {
      status: 403,
      headers: { 'content-type': 'text/html; charset=UTF-8', server: 'cloudflare' },
      bodyText: '<html>\n<head><title>403 Forbidden</title></head>\n<body>\n<center><h1>403 Forbidden</h1></center>\n<hr><center>cloudflare</center>\n</body>\n</html>\n',
      context: 'offline',
    };
    assertEdgePreflightDenial(ok);

    // The Worker's own answers (empty 204, JSON 404) mean the preflight
    // reached the origin: exactly what Access must prevent.
    for (const status of [204, 200, 302, 401, 404]) {
      expect(
        () => assertEdgePreflightDenial({ ...ok, status }),
        `status ${status} must fail`,
      ).toThrow(/expected the edge 403/);
    }
    expect(
      () =>
        assertEdgePreflightDenial({
          ...ok,
          headers: { ...ok.headers, 'access-control-allow-origin': 'https://SENTINEL' },
        }),
      'Worker CORS grants must fail',
    ).toThrow(/carries Worker CORS grants \(access-control-allow-origin\)/);
    expect(
      () =>
        assertEdgePreflightDenial({
          ...ok,
          headers: { 'content-type': 'application/json' },
          bodyText: '{"code":"forbidden"}',
        }),
      'a JSON 403 is an app answer, not the edge page',
    ).toThrow(/non-HTML content type/);
    expect(
      () => assertEdgePreflightDenial({ ...ok, headers: {} }),
      'a missing content type must fail',
    ).toThrow(/non-HTML content type/);
    expect(
      () => assertEdgePreflightDenial({ ...ok, bodyText: '' }),
      'an empty 403 must fail',
    ).toThrow(/not the bare Cloudflare 403 page/);
    expect(
      () => assertEdgePreflightDenial({ ...ok, bodyText: '<html><title>403 Forbidden</title>app</html>' }),
      'an HTML 403 without the Cloudflare signature must fail',
    ).toThrow(/not the bare Cloudflare 403 page/);
    let cookie = '';
    try {
      assertEdgePreflightDenial({ ...ok, headers: { ...ok.headers, 'set-cookie': 'a=SENTINEL' } });
    } catch (error) {
      cookie = (error as Error).message;
    }
    expect(cookie, 'denials must not set cookies').toMatch(/must not set a cookie/);
    expect(cookie).not.toContain('SENTINEL');
    for (const fn of [
      () =>
        assertEdgePreflightDenial({
          ...ok,
          headers: { ...ok.headers, 'access-control-allow-origin': 'https://SENTINEL' },
        }),
      () => assertEdgePreflightDenial({ ...ok, bodyText: 'SENTINEL' }),
    ]) {
      expect(fn).toThrow();
      try {
        fn();
      } catch (error) {
        expect((error as Error).message).not.toContain('SENTINEL');
      }
    }
  });

  test('probe paths stay pinned to the anonymous Worker contract', () => {
    // These assertions pin the paths used by the live probes.
    // Configured staging legs check them against the served Worker.
    expect(HEALTHZ_PATH).toBe('/healthz');
    expect([...ANONYMOUS_401_GET_PATHS]).toEqual(['/v1/vault']);
    expect([...ANONYMOUS_401_POST_PATHS]).toEqual([
      '/v1/session/refresh',
      '/v1/session/revoke-all',
    ]);
    expect(UNKNOWN_PATH).toBe('/v1/e2e-probe-no-such-route');
  });

  test('preflight probes cover liveness and the authed surface', () => {
    expect([...PREFLIGHT_PATHS]).toEqual(['/healthz', '/v1/vault']);
  });
});

test.describe('anonymous API Worker contract (staging)', () => {
  test('GET /healthz answers 200 with ok:true and no session', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'api-worker-healthz');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Sessionless by construction: Access pair only. A present Authorization
    // header would mean an app session credential leaked into this probe.
    const headers = accessHeaders();
    expect(headers['Authorization'], 'worker probe must carry no app session').toBeUndefined();

    const res = await apiGet(request, `${baseURL}${HEALTHZ_PATH}`, headers, 'api-worker-healthz');
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${HEALTHZ_PATH}` },
      'api-worker-healthz',
    );
    expect(res.status(), '[api-worker-healthz] liveness must answer 200').toBe(200);
    let parsed: unknown;
    try {
      parsed = JSON.parse(bodyText) as unknown;
    } catch {
      throw new Error(
        '[api-worker-healthz] liveness body is not JSON (names only — no values logged).',
      );
    }
    expect(
      typeof parsed === 'object' && parsed !== null
        ? (parsed as Record<string, unknown>).ok
        : undefined,
      '[api-worker-healthz] liveness must confirm with ok:true',
    ).toBe(true);
  });

  test('anonymous GET /v1/vault answers the generic 401', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'api-worker-vault-anonymous');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = accessHeaders();
    expect(headers['Authorization'], 'worker probe must carry no app session').toBeUndefined();

    for (const path of ANONYMOUS_401_GET_PATHS) {
      const context = `api-worker-vault-anonymous ${path}`;
      const res = await apiGet(request, `${baseURL}${path}`, headers, context);
      const bodyText = await res.text().catch(() => '');
      assertNoAccessWall(
        { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${path}` },
        context,
      );
      assertGenericAnonymous401({ status: res.status(), headers: res.headers(), bodyText, context });
    }
  });

  test('anonymous POST /v1/session/refresh answers the generic 401', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'api-worker-session-refresh-anonymous');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Session mutations read the session cookie plus the exact configured
    // Origin: the Origin below exercises the session gate (no cookie), not
    // the Origin gate — both collapse to the same generic 401.
    const headers: Record<string, string> = {
      ...accessHeaders(),
      Origin: new URL(baseURL).origin,
      'Content-Type': 'application/json',
    };
    expect(headers['Authorization'], 'worker probe must carry no app session').toBeUndefined();

    const path = ANONYMOUS_401_POST_PATHS[0];
    const context = `api-worker-session-refresh-anonymous ${path}`;
    const res = await apiPost(request, `${baseURL}${path}`, headers, {}, context);
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${path}` },
      context,
    );
    assertGenericAnonymous401({ status: res.status(), headers: res.headers(), bodyText, context });
  });

  test('anonymous POST /v1/session/revoke-all answers the generic 401', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'api-worker-session-revoke-all-anonymous');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers: Record<string, string> = {
      ...accessHeaders(),
      Origin: new URL(baseURL).origin,
      'Content-Type': 'application/json',
    };
    expect(headers['Authorization'], 'worker probe must carry no app session').toBeUndefined();

    const path = ANONYMOUS_401_POST_PATHS[1];
    const context = `api-worker-session-revoke-all-anonymous ${path}`;
    const res = await apiPost(request, `${baseURL}${path}`, headers, {}, context);
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${path}` },
      context,
    );
    assertGenericAnonymous401({ status: res.status(), headers: res.headers(), bodyText, context });
  });

  test('GET unknown worker path answers the 404 not_found envelope', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'api-worker-unknown-path');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = accessHeaders();
    expect(headers['Authorization'], 'worker probe must carry no app session').toBeUndefined();

    const context = `api-worker-unknown-path ${UNKNOWN_PATH}`;
    const res = await apiGet(request, `${baseURL}${UNKNOWN_PATH}`, headers, context);
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${UNKNOWN_PATH}` },
      context,
    );
    assertNotFoundEnvelope({ status: res.status(), headers: res.headers(), bodyText, context });
  });

  test('anonymous OPTIONS preflight is answered by Access and never reaches the Worker', async () => {
    const baseURL = stagingBaseUrl('skip', 'api-worker-preflight');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Same-origin browser-shaped preflight from a fresh anonymous context:
    // no Access pair, no bearer, no cookies, no follow. Access answers it at
    // the edge (TWO-411 option 2). The Worker's preflight contract is
    // unit-tested in `workers/api/src/worker_entry.rs` (see file header).
    const corsHeaders: Record<string, string> = {
      Origin: new URL(baseURL).origin,
      'Access-Control-Request-Method': 'GET',
      'Access-Control-Request-Headers': 'content-type',
    };

    for (const path of PREFLIGHT_PATHS) {
      const context = `api-worker-preflight ${path}`;
      const preflight = await anonymousApiOptions(`${baseURL}${path}`, corsHeaders, context);
      assertEdgePreflightDenial({ ...preflight, context });

      // The bare 403 carries no Access markers, so prove Access is the edge
      // on the same path: an anonymous GET must hit the Access wall.
      const gate = await anonymousApiGet(`${baseURL}${path}`, `${context} access-gate`);
      assertAccessWall(gate, `${context} access-gate`);
    }
  });
});
