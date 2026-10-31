import { test, expect } from '@playwright/test';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import {
  apiGet,
  apiPost,
  requireApiAccount,
  retrieveStagedSignupCode,
  tryMintTestAccount,
  type TestAccount,
} from '../helpers/accounts.js';

// Live sign-out-everywhere revocation behind the security page: proves the
// real `POST /v1/session/revoke-all` kills every session for the account
// server-side, not just the caller's. The account under test comes from the
// test-account seam (`POST /api/test-accounts`); a second concurrent session
// for the same mailbox comes from the staged magic-link seam
// (`POST /v1/auth/magic/start` plus `POST /api/test-codes` plus
// `POST /v1/auth/magic/finish`, as `login-magic-link.spec.ts` does). The
// assertions below are the whole contract — the second cookie reads 200
// pre-revoke, revoke-all answers 200 `{ok:true}` with a clearing
// `Set-Cookie`, then the caller's vault read and session refresh are 401
// AND the second cookie's vault read is 401. A single-session-logout mutant
// (`revoke_session` for the caller only) keeps the caller legs green but
// leaves the second cookie live, so the final leg is what pins `everywhere`.
// Page behavior (confirm dialog, redirect, 401 notice) is pinned offline by
// `apps/web/tests/account-security.spec.ts` with stubbed backends and
// produces no staging evidence; this spec proves the server revocation live.
//
// Transport (mirrors `e2e/tests/scoped-credentials.spec.ts`): session
// endpoints authenticate via the `__Host-nntune-session` cookie only
// (`session_cookie_value` in `workers/api/src/handlers.rs` — the
// `Authorization` bearer is ignored there), and mutations additionally
// require the exact configured `Origin` (`authenticate(..., true)`).
// The magic-link finish mints from a pure bearer token, so it goes out with
// Access plus the exact `Origin` and deliberately NO session cookie (an
// attached cookie would hit the swap guard in `magic_finish`).
//
// - File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
//   without launching a browser. See health.spec.ts.
// - QA_AUTH_TOKEN guard: the mint and code seams need it. Missing token skips
//   locally and fails closed on CI (a skip on CI would leave revocation
//   never exercised while the job stays green). Mirrors login-magic-link.
// - Seam-absent failure signature: with the token set but either seam not
//   yet deployed, the helper throws `[seam] POST /api/test-accounts failed:
//   ...` or `[code-seam] POST /api/test-codes failed: ...` and the live test
//   goes red — that red IS the gap signal, not noise.
// - Diagnostics name no mailbox, code, or token values: assertion messages
//   carry fixed context only (see the helpers).
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

function requireSeamToken(context: string): void {
  if (process.env.QA_AUTH_TOKEN) return;
  const reason =
    `${context} needs QA_AUTH_TOKEN plus the staging account and code ` +
    'seams (POST /api/test-accounts, POST /api/test-codes) — ' +
    'refusing to pass silently on CI.';
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

/** Session-cookie headers for a raw session value (the second session, no Origin gate). */
function cookieHeadersForValue(session: string): Record<string, string> {
  return {
    ...accessHeaders(),
    Cookie: `__Host-nntune-session=${session}`,
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

/**
 * Anonymous mutation headers for the magic-link start/finish surface: Access
 * plus the exact configured Origin, deliberately no Cookie and no bearer.
 * The finish mints from a pure token, so an attached session cookie would
 * hit the swap guard instead of the ordinary sign-in path.
 */
function anonymousMutationHeaders(webBase: string): Record<string, string> {
  return {
    ...accessHeaders(),
    Origin: new URL(webBase).origin,
    'Content-Type': 'application/json',
  };
}

/**
 * Extract the session value from a magic-finish Set-Cookie (names-only
 * failure, never the value).
 */
function sessionValueFromSetCookie(setCookie: string, context: string): string {
  // Playwright joins Set-Cookie lines with '\n', and the Access edge adds
  // its own CF_Authorization line, so the session cookie may start any line.
  const value = /(?:^|[;\n]\s*)__Host-nntune-session=([^;\n]*)/.exec(setCookie)?.[1]?.trim();
  if (!value) {
    throw new Error(
      `[${context}] no session cookie issued (names only — no values logged).`,
    );
  }
  return value;
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

  // 1. Open a second concurrent session for the same mailbox via the staged
  // magic-link seam, then prove it is live before the sweep.
  const start = await apiPost(
    request,
    `${baseURL}/v1/auth/magic/start`,
    anonymousMutationHeaders(baseURL),
    { email: account.email },
    'security-sign-out-everywhere-second-start',
  );
  const startBody = await start.text().catch(() => '');
  assertNoAccessWall(
    { status: start.status(), headers: start.headers(), bodyText: startBody, url: 'magic-start' },
    'security-sign-out-everywhere-second-start',
  );
  expect(start.status(), 'magic start must answer 200').toBe(200);
  const code = await retrieveStagedSignupCode(request, baseURL, account.email);
  if (!code) {
    throw new Error(
      '[security-sign-out-everywhere] no staged second-session code: this everywhere leg requires ' +
        'QA_AUTH_TOKEN plus the staging code-retrieval seam for example.test mailboxes ' +
        '(names only — no values logged). See e2e/README.md.',
    );
  }
  const finish = await apiPost(
    request,
    `${baseURL}/v1/auth/magic/finish`,
    anonymousMutationHeaders(baseURL),
    { token: code },
    'security-sign-out-everywhere-second-finish',
  );
  const finishHeaders = finish.headers();
  const finishBody = await finish.text().catch(() => '');
  assertNoAccessWall(
    {
      status: finish.status(),
      headers: finishHeaders,
      bodyText: finishBody,
      url: 'magic-finish',
    },
    'security-sign-out-everywhere-second-finish',
  );
  expect(finish.status(), 'magic finish must answer 200').toBe(200);
  let finishParsed: unknown;
  try {
    finishParsed = JSON.parse(finishBody) as unknown;
  } catch {
    throw new Error(
      '[security-sign-out-everywhere] magic finish body is not JSON (names only — no values logged).',
    );
  }
  expect(
    typeof finishParsed === 'object' && finishParsed !== null
      ? (finishParsed as Record<string, unknown>).ok
      : undefined,
    'magic finish must confirm with ok:true',
  ).toBe(true);
  const secondSession = sessionValueFromSetCookie(
    finishHeaders['set-cookie'] ?? '',
    'security-sign-out-everywhere-second-finish',
  );
  const secondVaultBefore = await apiGet(
    request,
    `${baseURL}/v1/vault`,
    cookieHeadersForValue(secondSession),
    'security-sign-out-everywhere-second-vault-before',
  );
  const secondVaultBeforeBody = await secondVaultBefore.text().catch(() => '');
  assertNoAccessWall(
    {
      status: secondVaultBefore.status(),
      headers: secondVaultBefore.headers(),
      bodyText: secondVaultBeforeBody,
      url: 'vault',
    },
    'security-sign-out-everywhere-second-vault-before',
  );
  expect(
    secondVaultBefore.status(),
    'second session vault read must answer 200 before revoke-all',
  ).toBe(200);

  // 2. Revoke-all answers 200 with a clearing Set-Cookie. Boolean expects
  // keep failure output value-free (no header values are ever printed).
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
  expect(
    setCookie.includes('__Host-nntune-session='),
    'revoke-all must clear the session cookie',
  ).toBe(true);
  expect(
    setCookie.toLowerCase().includes('max-age=0'),
    'revoke-all must expire the session cookie',
  ).toBe(true);

  // 3. The caller's next authenticated request is 401: the revoked cookie
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

  // 4. Second live check: session refresh with the same revoked cookie is
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

  // 5. The everywhere proof: the second session's vault read is 401 too. A
  // single-session-logout mutant (revoking only the caller's row) keeps legs
  // 3–4 green while this leg stays 200, so this is the assertion that pins
  // sign-out-everywhere rather than single-session logout.
  const secondVaultAfter = await apiGet(
    request,
    `${baseURL}/v1/vault`,
    cookieHeadersForValue(secondSession),
    'security-sign-out-everywhere-second-vault-after',
  );
  const secondVaultAfterBody = await secondVaultAfter.text().catch(() => '');
  assertNoAccessWall(
    {
      status: secondVaultAfter.status(),
      headers: secondVaultAfter.headers(),
      bodyText: secondVaultAfterBody,
      url: 'vault',
    },
    'security-sign-out-everywhere-second-vault-after',
  );
  expect(
    secondVaultAfter.status(),
    'second session vault read must answer 401 after revoke-all',
  ).toBe(401);
});
