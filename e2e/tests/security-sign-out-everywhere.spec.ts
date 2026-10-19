import { test, expect } from '@playwright/test';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import {
  apiGet,
  apiPost,
  requireApiAccount,
  tryMintTestAccount,
  type TestAccount,
} from '../helpers/accounts.js';

// Live sign-out-everywhere revocation behind the security page: proves the
// real `POST /v1/session/revoke-all` kills the caller's session
// server-side. The account under test comes from the test-account seam
// (`POST /api/test-accounts`); the three assertions below are the whole
// contract — 200 `{ok:true}` with a clearing `Set-Cookie`, then 401 on the
// next authenticated read and 401 on a second live check (session refresh).
// Page behavior (confirm dialog, redirect, 401 notice) is pinned offline by
// `apps/web/tests/account-security.spec.ts` with stubbed backends and
// produces no staging evidence; this spec proves the server revocation live.
//
// Transport (mirrors `e2e/tests/scoped-credentials.spec.ts`): session
// endpoints authenticate via the `__Host-nntune-session` cookie only
// (`session_cookie_value` in `workers/api/src/handlers.rs` — the
// `Authorization` bearer is ignored there), and mutations additionally
// require the exact configured `Origin` (`authenticate(..., true)`).
//
// - File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
//   without launching a browser. See health.spec.ts.
// - QA_AUTH_TOKEN guard: the mint seam needs it. Missing token skips
//   locally and fails closed on CI (a skip on CI would leave revocation
//   never exercised while the job stays green). Mirrors login-magic-link.
// - Seam-absent failure signature: with the token set but the seam not yet
//   deployed, the helper throws `[seam] POST /api/test-accounts failed: ...`
//   and the live test goes red — that red IS the gap signal, not noise.
// - Diagnostics name no mailbox or token values: assertion messages carry
//   fixed context only (see the helpers).
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

function requireSeamToken(context: string): void {
  if (process.env.QA_AUTH_TOKEN) return;
  const reason =
    `${context} needs QA_AUTH_TOKEN plus the staging account seam ` +
    '(POST /api/test-accounts) — refusing to pass silently on CI.';
  if (process.env.CI) throw new Error(`[security-sign-out-everywhere] ${reason}`);
  test.skip(true, reason);
}

/** Raw session value for cookie transport (names-only failure, never the value). */
function sessionValue(account: TestAccount | null): string {
  const value = account?.session;
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(
      '[security-sign-out-everywhere] no authenticated account session (names only — no values logged).',
    );
  }
  return value;
}

/** Session-cookie headers for authenticated reads (ordinary session, no Origin gate). */
function sessionCookieHeaders(account: TestAccount | null): Record<string, string> {
  return {
    ...accessHeaders(),
    Cookie: `__Host-nntune-session=${sessionValue(account)}`,
  };
}

/**
 * Session-cookie headers for the mutation surface (revoke-all, refresh):
 * the session gate reads the cookie plus the exact configured Origin and
 * ignores the Authorization bearer entirely.
 */
function sessionMutationHeaders(
  account: TestAccount | null,
  webBase: string,
): Record<string, string> {
  return {
    ...sessionCookieHeaders(account),
    Origin: new URL(webBase).origin,
    'Content-Type': 'application/json',
  };
}

test('sign-out-everywhere revokes the session and closes follow-up checks', async ({
  request,
}) => {
  const baseURL = stagingBaseUrl('skip', 'security-sign-out-everywhere');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  requireSeamToken('sign-out-everywhere revocation');
  const account = await tryMintTestAccount(request, baseURL);
  if (!account) {
    throw new Error(
      '[security-sign-out-everywhere] no minted account: the revocation leg requires ' +
        'POST /api/test-accounts once QA_AUTH_TOKEN is set ' +
        '(names only — no values logged). See e2e/README.md.',
    );
  }
  requireApiAccount(account, 'security-sign-out-everywhere');

  // 1. Revoke-all answers 200 with a clearing Set-Cookie.
  const revoke = await apiPost(
    request,
    `${baseURL}/v1/session/revoke-all`,
    sessionMutationHeaders(account, baseURL),
    {},
    'security-sign-out-everywhere-revoke',
  );
  const revokeHeaders = revoke.headers();
  const revokeBody = await revoke.text().catch(() => '');
  assertNoAccessWall(
    { status: revoke.status(), headers: revokeHeaders, bodyText: revokeBody, url: 'revoke-all' },
    'security-sign-out-everywhere-revoke',
  );
  expect(revoke.status(), 'revoke-all must answer 200').toBe(200);
  let revokeParsed: unknown;
  try {
    revokeParsed = JSON.parse(revokeBody) as unknown;
  } catch {
    throw new Error(
      '[security-sign-out-everywhere] revoke-all body is not JSON (names only — no values logged).',
    );
  }
  expect(
    typeof revokeParsed === 'object' && revokeParsed !== null
      ? (revokeParsed as Record<string, unknown>).ok
      : undefined,
    'revoke-all must confirm with ok:true',
  ).toBe(true);
  const setCookie = revokeHeaders['set-cookie'] ?? '';
  expect(setCookie, 'revoke-all must clear the session cookie').toContain(
    '__Host-nntune-session=',
  );
  expect(setCookie.toLowerCase(), 'revoke-all must expire the session cookie').toContain(
    'max-age=0',
  );

  // 2. The caller's next authenticated request is 401: the revoked cookie
  // no longer verifies against any live server row.
  const vault = await apiGet(
    request,
    `${baseURL}/v1/vault`,
    sessionCookieHeaders(account),
    'security-sign-out-everywhere-vault',
  );
  const vaultBody = await vault.text().catch(() => '');
  assertNoAccessWall(
    { status: vault.status(), headers: vault.headers(), bodyText: vaultBody, url: 'vault' },
    'security-sign-out-everywhere-vault',
  );
  expect(vault.status(), 'vault read with the revoked session must answer 401').toBe(401);

  // 3. Second live check: session refresh with the same revoked cookie is
  // also 401 — no rotation path survives the sweep.
  const refresh = await apiPost(
    request,
    `${baseURL}/v1/session/refresh`,
    sessionMutationHeaders(account, baseURL),
    {},
    'security-sign-out-everywhere-refresh',
  );
  const refreshBody = await refresh.text().catch(() => '');
  assertNoAccessWall(
    {
      status: refresh.status(),
      headers: refresh.headers(),
      bodyText: refreshBody,
      url: 'refresh',
    },
    'security-sign-out-everywhere-refresh',
  );
  expect(refresh.status(), 'session refresh with the revoked session must answer 401').toBe(401);
});
