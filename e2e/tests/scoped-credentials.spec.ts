import { test, expect } from '@playwright/test';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import {
  accountHeaders,
  apiGet,
  apiPost,
  requireApiAccount,
  tryMintTestAccount,
} from '../helpers/accounts.js';

// Scoped native REST credentials (ADR 0010, paired acceptance card,
// paired build card): mint → scoped route 200
// → revoke → 401/403, scope-matrix denials by name, header-only transport,
// names-only diagnostics.
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
  test('mint → scoped route 200 → revoke → 401/403', async ({ request }) => {
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
    const sessionHeaders = {
      ...accountHeaders(account),
      'Content-Type': 'application/json',
    };

    // Mint a library:read credential (one-time secret).
    const mint = await apiPost(
      request,
      `${nativeBase}${ISSUE_PATH}`,
      sessionHeaders,
      { label: 'e2e-lifecycle', scopes: ['library:read'], ttl_secs: 3600 },
      'scoped-credentials-mint',
    );
    const mintBody = await mint.text().catch(() => '');
    assertNoAccessWall(
      { status: mint.status(), headers: mint.headers(), bodyText: '', url: ISSUE_PATH },
      'scoped-credentials-mint',
    );
    expect(mint.status(), 'issuance must answer 201 once implemented').toBe(201);
    const minted = JSON.parse(mintBody) as Record<string, unknown>;
    const secret = minted.secret;
    expect(typeof secret === 'string' && secret.length > 0, 'issuance returns a secret').toBe(
      true,
    );
    expect(
      (secret as string).startsWith('ntk_'),
      'display form carries the ntk_ class prefix',
    ).toBe(true);
    const credentialId = minted.id;
    expect(typeof credentialId === 'string', 'issuance returns an id').toBe(true);

    const secretValue = secret as string;
    const idValue = credentialId as string;

    // Scoped route answers 200 with the credential.
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
    expect(scoped.status(), 'in-scope bearer call must answer 200').toBe(200);

    // Revoke via the session-authenticated surface.
    const revokeRes = await request.delete(`${nativeBase}${ISSUE_PATH}/${idValue}`, {
      headers: sessionHeaders,
      timeout: 30_000,
      maxRedirects: 0,
    });
    expect(revokeRes.status(), 'revocation must answer 200 once implemented').toBe(200);

    // The revoked credential fails closed — and the denial names nothing.
    const denied = await apiGet(
      request,
      `${nativeBase}${CANONICAL_ROUTE['library:read'].path}`,
      bearerHeaders(secretValue),
      'scoped-credentials-revoked',
    );
    expect(
      [401, 403].includes(denied.status()),
      'revoked credential must fail closed with 401/403',
    ).toBe(true);
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

    // No credential at all.
    const bare = await apiGet(request, target, accessHeaders(), 'scoped-credentials-bare');
    expect([401, 403].includes(bare.status()), 'bare call must fail closed').toBe(true);

    // Garbage bearer.
    const garbage = await apiGet(
      request,
      target,
      bearerHeaders('ntk_no-such-credential-000000000000000000000'),
      'scoped-credentials-garbage',
    );
    expect([401, 403].includes(garbage.status()), 'unknown credential must fail closed').toBe(
      true,
    );

    // Session cookie WITHOUT an Authorization bearer: credential routes
    // must not fall back to the cookie.
    const cookieOnly = await request.get(target, {
      headers: { ...accessHeaders(), Cookie: '__Host-nntune-session=e2e-synthetic-session' },
      timeout: 30_000,
      maxRedirects: 0,
    });
    expect(
      [401, 403].includes(cookieOnly.status()),
      'cookie-only call on a credential route must fail closed',
    ).toBe(true);

    // Query-string bearer is rejected (query strings land in logs).
    const query = await apiGet(
      request,
      `${target}?apiKey=ntk_query-string-probe-00000000000000000`,
      accessHeaders(),
      'scoped-credentials-query',
    );
    expect(
      [401, 403].includes(query.status()),
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
    const sessionHeaders = {
      ...accountHeaders(account),
      'Content-Type': 'application/json',
    };

    const mintFor = async (scope: Scope): Promise<string> => {
      const res = await apiPost(
        request,
        `${nativeBase}${ISSUE_PATH}`,
        sessionHeaders,
        { label: `e2e-matrix-${scope}`, scopes: [scope], ttl_secs: 3600 },
        'scoped-credentials-matrix-mint',
      );
      expect(res.status(), 'matrix issuance must answer 201 once implemented').toBe(201);
      const body = (await res.json()) as Record<string, unknown>;
      const secret = body.secret;
      expect(typeof secret === 'string', 'matrix issuance returns a secret').toBe(true);
      return secret as string;
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
    const reader = await mintFor('library:read');
    const writer = await mintFor('plays:write');
    expect(await call('GET', '/v1/search', reader), 'own scope must answer 200').toBe(200);
    expect(await call('POST', '/v1/plays', writer), 'own scope must answer 200').toBe(200);
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
    const sessionHeaders = {
      ...accountHeaders(account),
      'Content-Type': 'application/json',
    };

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
      sessionHeaders,
      'scoped-credentials-list',
    );
    expect(list.status(), 'credential list must answer 200 once implemented').toBe(200);
    const listBody = await list.text().catch(() => '');
    expect(listBody.includes('"secret"'), 'list must never carry a secret key').toBe(false);
    expect(listBody.includes('ntk_'), 'list must never carry display secrets').toBe(false);
  });
});
