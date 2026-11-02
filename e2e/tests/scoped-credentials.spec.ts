import { test, expect, type APIRequestContext } from '@playwright/test';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import {
  apiGet,
  apiPost,
  requireApiAccount,
  tryMintTestAccount,
  type TestAccount,
} from '../helpers/accounts.js';

// Scoped native REST credentials (ADR 0010, paired acceptance card,
// paired build card): mint → scoped route 404 (gate passed; data routes
// pending) → revoke → 404 (revoked/unknown bearers are the same plain 404
// as header-absent callers — uniform posture, no existence oracle),
// scope-matrix denials by name, header-only transport, names-only
// diagnostics.
//
// NO file-level skip here by design (see demo-audio-contract.spec.ts): the
// offline describe pins the contract table on every PR (via
// `npm run test:unit`) with no staging, while each live test skips green
// when STAGING_BASE_URL is unset and fails closed once it is configured.
//
// Native base: NATIVE_REST_BASE_URL when set (https only), else the staging
// base (same-origin /v1 routing). Pre-implementation both are unset, so the
// live legs skip; once configured they prove the worker surface.
//
// Names-only rule for this file: credential and session VALUES are compared
// in memory only. Failure messages name statuses, route keys and field
// names — never interpolate a response body (it may carry the one-time
// secret) or a credential value.

const SCOPES = [
  'library:read',
  'playlists:write',
  'plays:write',
  'stream:read',
  'ingest:submit',
] as const;

type Scope = (typeof SCOPES)[number];

/** Canonical bearer route per scope (mirrors the Rust route-map pins). */
const CANONICAL_ROUTE: Record<Scope, { method: 'GET' | 'POST'; path: string }> = {
  'library:read': { method: 'GET', path: '/v1/search' },
  'playlists:write': { method: 'POST', path: '/v1/playlists' },
  'plays:write': { method: 'POST', path: '/v1/plays' },
  'stream:read': { method: 'GET', path: '/v1/stream' },
  'ingest:submit': { method: 'POST', path: '/v1/ingest' },
};

/** Routes no bearer credential may ever reach (session-only per ADR 0010). */
const SESSION_ONLY_PATHS = [
  '/v1/me',
  '/v1/indexers',
  '/v1/vault',
  '/v1/vault/test-connection',
  '/v1/credentials',
];

/** Issuance surface the paired build implements (session-authenticated). */
const ISSUE_PATH = '/v1/credentials';

function nativeBaseUrl(): string | null {
  const raw = (process.env.NATIVE_REST_BASE_URL || '').trim().replace(/\/+$/, '');
  if (!raw) return stagingBaseUrl('skip', 'scoped-credentials');
  if (!/^https:\/\//i.test(raw)) {
    throw new Error(
      '[scoped-credentials] NATIVE_REST_BASE_URL must be an https:// URL. ' +
        'Refusing to run against a non-TLS target.',
    );
  }
  accessHeaders();
  return raw;
}

function bearerHeaders(secret: string): Record<string, string> {
  return { ...accessHeaders(), Authorization: `Bearer ${secret}` };
}

/** Raw session value for cookie transport (names-only failure, never the value). */
function sessionValue(account: TestAccount | null): string {
  const value = account?.session;
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(
      '[scoped-credentials] no authenticated account session (names only — no values logged).',
    );
  }
  return value;
}

/**
 * Session-cookie headers for the issuance surface (mint/revoke): the
 * privileged gate reads the session cookie plus the exact configured
 * Origin and ignores the Authorization bearer entirely.
 */
function privilegedHeaders(
  account: TestAccount | null,
  webBase: string,
): Record<string, string> {
  return {
    ...accessHeaders(),
    Cookie: `__Host-nntune-session=${sessionValue(account)}`,
    Origin: new URL(webBase).origin,
  };
}

/** Session-cookie headers for the list surface (ordinary session, no Origin gate). */
function sessionCookieHeaders(account: TestAccount | null): Record<string, string> {
  return {
    ...accessHeaders(),
    Cookie: `__Host-nntune-session=${sessionValue(account)}`,
  };
}

const sleep = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Issuance with bounded retry over transient store blips: the mint path
 * 500s when its session/credential store call fails, and a brief store
 * outage 500s every mint attempt inside a test window (observed on the
 * staging journey — two independent mints 500ing seconds after a healthy
 * mint, then recovery). Bounded to 3 attempts with ~5s/~10s waits (inside
 * the 120s test timeout), covering 500/503 only; any 4xx verdict fails
 * fast and is never retried. Intermediate bodies are never read (only the
 * 201 body is parsed); assertions name statuses only.
 */
async function mintCredential(
  request: APIRequestContext,
  nativeBase: string,
  mintHeaders: Record<string, string>,
  label: string,
  scopes: Scope[],
  context: string,
): Promise<{ secret: string; id: string }> {
  let lastStatus = 0;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    const res = await apiPost(
      request,
      `${nativeBase}${ISSUE_PATH}`,
      mintHeaders,
      { label, scopes, ttl_secs: 3600 },
      context,
    );
    lastStatus = res.status();
    assertNoAccessWall(
      { status: lastStatus, headers: res.headers(), bodyText: '', url: ISSUE_PATH },
      context,
    );
    if (lastStatus === 201) {
      const body = (await res.json()) as Record<string, unknown>;
      const secret = body.secret;
      const id = body.id;
      expect(typeof secret === 'string' && secret.length > 0, 'issuance returns a secret').toBe(
        true,
      );
      expect(
        (secret as string).startsWith('ntk_'),
        'display form carries the ntk_ class prefix',
      ).toBe(true);
      expect(typeof id === 'string', 'issuance returns an id').toBe(true);
      return { secret: secret as string, id: id as string };
    }
    if (lastStatus !== 500 && lastStatus !== 503) {
      break;
    }
    if (attempt < 3) {
      await sleep(attempt === 1 ? 5_000 : 10_000);
    }
  }
  expect(lastStatus, 'issuance must answer 201 (4xx fails fast; 500/503 retried 3x)').toBe(201);
  throw new Error('[scoped-credentials] issuance never answered 201 (names only).');
}

test.describe('credential contract table (offline, no staging)', () => {
  test('scope vocabulary matches ADR 0010 exactly', () => {
    expect([...SCOPES]).toEqual([
      'library:read',
      'playlists:write',
      'plays:write',
      'stream:read',
      'ingest:submit',
    ]);
  });

  test('every scope has a canonical bearer route; session-only set is disjoint', () => {
    const bearerPaths = new Set(Object.values(CANONICAL_ROUTE).map((r) => r.path));
    expect(bearerPaths.size).toBe(SCOPES.length);
    for (const path of SESSION_ONLY_PATHS) {
      expect(bearerPaths.has(path), `${path} must not double as a bearer route`).toBe(false);
    }
    expect(SESSION_ONLY_PATHS).toContain(ISSUE_PATH);
  });

  test('contract table carries no secret material', () => {
    const table = JSON.stringify({ SCOPES, CANONICAL_ROUTE, SESSION_ONLY_PATHS, ISSUE_PATH });
    expect(table.includes('ntk_'), 'table must not embed credential values').toBe(false);
    for (const scope of SCOPES) {
      expect(scope.includes(' '), `scope ${scope} must be a single token`).toBe(false);
    }
  });

  test('bearer headers carry the secret in one place only', () => {
    const headers = bearerHeaders('SECRET_SENTINEL');
    expect(headers.Authorization).toBe('Bearer SECRET_SENTINEL');
    const url = `${CANONICAL_ROUTE['library:read'].path}`;
    expect(url.includes('SECRET_SENTINEL'), 'secret must never be built into a URL').toBe(false);
    expect(url.includes('?'), 'credential routes take no query credentials').toBe(false);
  });
});

test.describe('staging credential lifecycle', () => {
  test('mint → scoped route 404 (gate passed) → revoke → 404 (fail closed)', async ({
    request,
  }) => {
    const webBase = stagingBaseUrl('skip', 'scoped-credentials-lifecycle');
    if (!webBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const nativeBase = nativeBaseUrl();
    if (!nativeBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const account = await tryMintTestAccount(request, webBase);
    if (!account) {
      test.skip(true, 'account seam unconfigured (no QA_AUTH_TOKEN) — lifecycle needs a session.');
      return;
    }
    requireApiAccount(account, 'scoped-credentials-lifecycle');
    // Issuance is session-cookie + exact Origin (the bearer header is
    // ignored there).
    const mintHeaders = {
      ...privilegedHeaders(account, webBase),
      'Content-Type': 'application/json',
    };
    const revokeHeaders = privilegedHeaders(account, webBase);

    // Mint a library:read credential (one-time secret). The helper
    // checks the access wall on every attempt and retries boundedly
    // over transient store-blip 500s.
    const { secret: secretValue, id: idValue } = await mintCredential(
      request,
      nativeBase,
      mintHeaders,
      'e2e-lifecycle',
      ['library:read'],
      'scoped-credentials-mint',
    );

    // Scoped route: the bearer passes the gate, then lands on the
    // documented 404 until the data routes land. Presented-but-invalid
    // bearers are the SAME plain 404 (uniform posture per ADR 0010 — a
    // distinct status would be a route-existence oracle), so 404 here
    // proves the gate passed only together with the wrong-scope 403
    // pinned in the matrix test below.
    const scoped = await apiGet(
      request,
      `${nativeBase}${CANONICAL_ROUTE['library:read'].path}`,
      bearerHeaders(secretValue),
      'scoped-credentials-use',
    );
    assertNoAccessWall(
      { status: scoped.status(), headers: scoped.headers(), bodyText: '', url: 'use' },
      'scoped-credentials-use',
    );
    expect(scoped.status(), 'in-scope bearer call passes the gate to the documented 404').toBe(
      404,
    );

    // Revoke via the session-cookie + Origin surface.
    const revokeRes = await request.delete(`${nativeBase}${ISSUE_PATH}/${idValue}`, {
      headers: revokeHeaders,
      timeout: 30_000,
      maxRedirects: 0,
    });
    expect(revokeRes.status(), 'revocation must answer 200 once implemented').toBe(200);

    // The revoked credential fails closed with the same plain 404 as a
    // header-absent caller (uniform posture per ADR 0010 — revoked,
    // expired, and unknown bearers must not disclose route existence;
    // only a valid credential on the wrong scope is the distinct 403) —
    // and the denial names nothing.
    const denied = await apiGet(
      request,
      `${nativeBase}${CANONICAL_ROUTE['library:read'].path}`,
      bearerHeaders(secretValue),
      'scoped-credentials-revoked',
    );
    expect(denied.status(), 'revoked credential must fail closed with 404').toBe(404);
    const deniedBody = await denied.text().catch(() => '');
    expect(deniedBody.includes(secretValue), 'denial must not echo the secret').toBe(false);
  });

  test('unscoped caller and session-cookie fallback both fail closed', async ({ request }) => {
    const webBase = stagingBaseUrl('skip', 'scoped-credentials-negative');
    if (!webBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const nativeBase = nativeBaseUrl();
    if (!nativeBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const target = `${nativeBase}${CANONICAL_ROUTE['library:read'].path}`;

    // No credential at all: header-absent callers skip the bearer gate
    // and land on the plain 404 until the data routes land (fail-closed
    // either way).
    const bare = await apiGet(request, target, accessHeaders(), 'scoped-credentials-bare');
    expect([401, 403, 404].includes(bare.status()), 'bare call must fail closed').toBe(true);

    // Garbage bearer: an unknown credential is the same plain 404 as a
    // header-absent caller (uniform posture per ADR 0010 — only
    // wrong-scope is the distinct 403).
    const garbage = await apiGet(
      request,
      target,
      bearerHeaders('ntk_no-such-credential-000000000000000000000'),
      'scoped-credentials-garbage',
    );
    expect(garbage.status(), 'unknown credential must fail closed with 404').toBe(404);

    // Session cookie WITHOUT an Authorization bearer: credential routes
    // must not fall back to the cookie (plain 404 until the data routes
    // land with their session-or-bearer dispatch — fail-closed either way).
    const cookieOnly = await request.get(target, {
      headers: { ...accessHeaders(), Cookie: '__Host-nntune-session=e2e-synthetic-session' },
      timeout: 30_000,
      maxRedirects: 0,
    });
    expect(
      [401, 403, 404].includes(cookieOnly.status()),
      'cookie-only call on a credential route must fail closed',
    ).toBe(true);

    // Query-string bearer is rejected (query strings land in logs); with no
    // Authorization header the probe lands on the plain 404 until the data
    // routes land — fail-closed either way.
    const query = await apiGet(
      request,
      `${target}?apiKey=ntk_query-string-probe-00000000000000000`,
      accessHeaders(),
      'scoped-credentials-query',
    );
    expect(
      [401, 403, 404].includes(query.status()),
      'query-string credential must be rejected',
    ).toBe(true);
  });

  test('scope matrix spot check fails by name (403, not 401)', async ({ request }) => {
    const webBase = stagingBaseUrl('skip', 'scoped-credentials-matrix');
    if (!webBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const nativeBase = nativeBaseUrl();
    if (!nativeBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const account = await tryMintTestAccount(request, webBase);
    if (!account) {
      test.skip(true, 'account seam unconfigured (no QA_AUTH_TOKEN) — matrix needs a session.');
      return;
    }
    requireApiAccount(account, 'scoped-credentials-matrix');
    // Issuance is session-cookie + exact Origin (the bearer header is
    // ignored there).
    const matrixMintHeaders = {
      ...privilegedHeaders(account, webBase),
      'Content-Type': 'application/json',
    };

    // Bounded retry over transient store-blip 500s (see mintCredential):
    // a single store outage previously 500ed both matrix mints in the
    // same window, and the whole-test retry only re-ran the failure.
    const mintFor = async (scope: Scope): Promise<string> => {
      const { secret } = await mintCredential(
        request,
        nativeBase,
        matrixMintHeaders,
        `e2e-matrix-${scope}`,
        [scope],
        'scoped-credentials-matrix-mint',
      );
      return secret;
    };

    const call = async (method: 'GET' | 'POST', path: string, secret: string): Promise<number> => {
      const url = `${nativeBase}${path}`;
      const res =
        method === 'GET'
          ? await apiGet(request, url, bearerHeaders(secret), 'scoped-credentials-matrix')
          : await apiPost(
              request,
              url,
              { ...bearerHeaders(secret), 'Content-Type': 'application/json' },
              {},
              'scoped-credentials-matrix',
            );
      return res.status();
    };

    // library:read reaches its own route but not plays:write's, and vice
    // versa — each denial is 403 (authenticated, wrong scope by name).
    // Own-scope bearers pass the gate to the documented 404 until the data
    // routes land (a wrong-scope bearer is 403 instead, so 404 proves the
    // gate passed for the right scope).
    const reader = await mintFor('library:read');
    const writer = await mintFor('plays:write');
    expect(await call('GET', '/v1/search', reader), 'own scope passes the gate to 404').toBe(
      404,
    );
    expect(await call('POST', '/v1/plays', writer), 'own scope passes the gate to 404').toBe(404);
    expect(await call('POST', '/v1/plays', reader), 'cross-scope must be 403 by name').toBe(403);
    expect(await call('GET', '/v1/search', writer), 'cross-scope must be 403 by name').toBe(403);
  });

  test('credential list and issuance surface never return secret material', async ({
    request,
  }) => {
    const webBase = stagingBaseUrl('skip', 'scoped-credentials-list');
    if (!webBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const nativeBase = nativeBaseUrl();
    if (!nativeBase) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const account = await tryMintTestAccount(request, webBase);
    if (!account) {
      test.skip(true, 'account seam unconfigured (no QA_AUTH_TOKEN) — list needs a session.');
      return;
    }
    requireApiAccount(account, 'scoped-credentials-list');
    // List is an ordinary session-cookie surface (no Origin gate, no bearer).
    const listHeaders = sessionCookieHeaders(account);

    // Issuance itself requires a session: bearer and anonymous callers fail.
    const anonIssue = await apiPost(
      request,
      `${nativeBase}${ISSUE_PATH}`,
      { ...accessHeaders(), 'Content-Type': 'application/json' },
      { label: 'e2e-anon', scopes: ['library:read'], ttl_secs: 60 },
      'scoped-credentials-anon-issue',
    );
    expect(
      [401, 403].includes(anonIssue.status()),
      'anonymous issuance must fail closed',
    ).toBe(true);

    const list = await apiGet(
      request,
      `${nativeBase}${ISSUE_PATH}`,
      listHeaders,
      'scoped-credentials-list',
    );
    expect(list.status(), 'credential list must answer 200 once implemented').toBe(200);
    const listBody = await list.text().catch(() => '');
    expect(listBody.includes('"secret"'), 'list must never carry a secret key').toBe(false);
    expect(listBody.includes('ntk_'), 'list must never carry display secrets').toBe(false);
  });
});
