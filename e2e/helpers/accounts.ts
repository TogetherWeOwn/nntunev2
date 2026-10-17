import { expect, type APIRequestContext, type APIResponse, type Page } from '@playwright/test';
import { API, TESTIDS, uniqueTestEmail } from './contract.js';
import {
  accessHeaders,
  assertNoAccessWall,
  isStagingOrigin,
  qaAuthHeader,
  syntheticNntpEndpoint,
} from './access.js';

export interface TestAccount {
  email: string;
  /**
   * Opaque session for the minted account, when the seam provisions one.
   * Contract (owned by the auth card): the staging API accepts it as
   * `Authorization: Bearer <session>`. Account-specific assertions require
   * this per-account credential; QA_AUTH_TOKEN is seam-setup-only and never
   * substitutes for it. Absent is a setup-only result.
   * If the auth card implements cookie sessions instead, update
   * accountHeaders/applyAccountToPage together with the specs.
   */
  session?: string;
  /** True when minted via the API seam (skip the UI signup form). */
  viaApi: boolean;
}

/** First value that is a nonblank string, else undefined (value kept as-is). */
function nonblankString(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value : undefined;
}

/** The account's own usable session credential, if it has one. */
function accountSession(account: TestAccount | null): string | undefined {
  return account ? nonblankString(account.session) : undefined;
}

/**
 * Headers for API calls as a specific test account: Access pair +
 * per-account session. Every caller is account-specific, so a missing or
 * blank minted session throws instead of falling back to the generic QA
 * seam token (setup-only; it proves nothing about this account).
 * Every credentialed API call MUST go through apiGet/apiPost below, which
 * fetch with maxRedirects: 0: Playwright's APIRequestContext follows
 * redirects by default (up to 20) and forwards custom headers — including
 * the Access pair and session bearer — to the redirect target. A 3xx on a
 * staging /api path (canonical-host/scheme redirect, object-store hand-off)
 * must fail by name, never follow with credentials.
 */
export function accountHeaders(account: TestAccount | null): Record<string, string> {
  const session = accountSession(account);
  if (!session) {
    throw new Error(
      'accountHeaders requires the minted account session credential: ' +
        'no nonblank per-account session is available, and the generic QA seam ' +
        'token cannot stand in for an account identity (names only — no values logged).',
    );
  }
  return { ...accessHeaders(), Authorization: `Bearer ${session}` };
}

/**
 * Fixed, value-free description of a redirect Location header. The header
 * is untrusted input (userinfo/query/fragment can carry secrets), so only
 * its state — missing, malformed, present — is ever reported, never its
 * text. Whitespace/control characters are malformed per RFC 3986; anything
 * else must parse as an absolute URL or a relative reference.
 */
function locationNote(location: string | undefined): string {
  if (location === undefined || location === '') return 'no location header';
  try {
    if (/[\s\u0000-\u001f\u007f]/.test(location)) throw new Error('malformed');
    // The base is a throwaway placeholder so relative references parse; it
    // is never emitted.
    new URL(location, 'https://placeholder.invalid/');
    return 'location present (redacted)';
  } catch {
    return 'location malformed (redacted)';
  }
}

/**
 * Throw on a 3xx before any caller can follow it with credentials.
 * Access-login redirects keep the wall remediation (not a generic redirect);
 * any other 3xx fails naming the status + location state. Neither the
 * request URL nor the Location value is ever interpolated: both are
 * untrusted and can carry userinfo/query/fragment secrets, so diagnostics
 * carry only the fixed context, category, status and variable names.
 */
function throwOnRedirect(status: number, headers: Record<string, string>, context: string): void {
  if (status < 300 || status >= 400) return;
  const location = headers['location'] ?? headers['Location'];
  const note = locationNote(location);
  if (location !== undefined && /cloudflareaccess\.com|\/cdn-cgi\/access/i.test(location)) {
    throw new Error(
      `[${context}] hit the Cloudflare Access wall (HTTP ${status}, ${note}). ` +
        'Set CF_ACCESS_CLIENT_ID + CF_ACCESS_CLIENT_SECRET for a QA service ' +
        'token with staging access. See e2e/README.md. No secret values are ' +
        'logged here by design.',
    );
  }
  throw new Error(
    `[${context}] unexpected redirect (HTTP ${status}, ${note}): ` +
      'refusing to follow with credentials (names only — no values logged).',
  );
}

/**
 * Credentialed GET that never follows redirects with headers.
 * Fetches with maxRedirects: 0 so a staging 302 to another origin is
 * returned as-is (foreign origin receives zero requests); a 3xx throws
 * via throwOnRedirect instead of surfacing as a bare status mismatch.
 */
export async function apiGet(
  request: APIRequestContext,
  url: string,
  headers: Record<string, string>,
  context: string,
  opts?: { timeout?: number },
): Promise<APIResponse> {
  const res = await request.get(url, {
    headers,
    timeout: opts?.timeout ?? 30_000,
    maxRedirects: 0,
  });
  throwOnRedirect(res.status(), res.headers(), context);
  return res;
}

/**
 * Credentialed POST that never follows redirects with headers.
 * Same no-follow contract as apiGet; used by the test-account seam.
 */
export async function apiPost(
  request: APIRequestContext,
  url: string,
  headers: Record<string, string>,
  data: unknown,
  context: string,
  opts?: { timeout?: number },
): Promise<APIResponse> {
  const res = await request.post(url, {
    headers,
    data: data as Record<string, unknown>,
    timeout: opts?.timeout ?? 30_000,
    maxRedirects: 0,
  });
  throwOnRedirect(res.status(), res.headers(), context);
  return res;
}

/**
 * Authenticate a browser page as a minted account. Call BEFORE the first
 * navigation: the route applies to subsequent loads, never past ones.
 *
 * Headers are attached ONLY to the staging origin via a context route:
 * `extraHTTPHeaders`/`setExtraHTTPHeaders` would also send the Access
 * service token and session bearer to cross-origin subresources (fonts,
 * analytics, stream/store URLs, payment redirects). Anything off-origin
 * continues unmodified. Staging requests are fetched with maxRedirects: 0
 * and fulfilled back to the browser, so a 302 to another origin is followed
 * WITHOUT the credential headers — the follow-up request is evaluated fresh
 * by this handler. Re-entrant: replaces any previous scope routing.
 */
export async function applyAccountToPage(
  page: Page,
  account: TestAccount | null,
  baseURL: string,
): Promise<void> {
  // A non-null account is an account-specific identity: without its own
  // nonblank session it must fail before any routing, never silently become
  // an Access-only page. Null stays allowed for the UI-signup fallback and
  // health checks (Access pair only, no Authorization).
  const session = accountSession(account);
  if (account && !session) {
    throw new Error(
      'applyAccountToPage requires the minted account session credential: ' +
        'a non-null account has no nonblank per-account session, and the generic ' +
        'QA seam token cannot stand in for an account identity (names only — no values logged).',
    );
  }
  const stagingHeaders: Record<string, string> = { ...accessHeaders() };
  if (session) stagingHeaders.Authorization = `Bearer ${session}`;
  const context = page.context();
  await context.unrouteAll({ behavior: 'wait' }).catch(() => {});
  if (Object.keys(stagingHeaders).length === 0) return;
  const stagingOrigin = new URL(baseURL).origin;
  await context.route('**/*', async (route) => {
    const url = route.request().url();
    if (isStagingOrigin(url, stagingOrigin)) {
      // No-redirect fetch: route.continue({ headers }) would keep the headers
      // on the browser's automatic redirect follow to another origin. Fetching
      // with maxRedirects: 0 and fulfilling hands the 3xx to the browser as-is
      // so the next request re-enters this handler and goes out clean.
      const response = await route.fetch({
        headers: { ...route.request().headers(), ...stagingHeaders },
        maxRedirects: 0,
      });
      await route.fulfill({ response });
    } else {
      await route.continue();
    }
  });
}

/**
 * Fail closed when an account-specific API step lacks the minted account's
 * own nonblank session. The generic QA seam token is setup-only and cannot
 * satisfy this guard: running Range/meter assertions as the seam identity
 * (or anonymously) would pass or fail for the wrong reasons while looking green.
 */
export function requireApiAccount(account: TestAccount | null, context: string): void {
  if (accountSession(account)) return;
  throw new Error(
    `[${context}] no authenticated account: this account-specific step requires the ` +
      "minted account's own nonblank session credential, and the generic QA seam " +
      'token cannot satisfy it (names only — no values logged). See e2e/README.md.',
  );
}

/**
 * Mint a throwaway staging account via POST /api/test-accounts.
 *
 * Returns null ONLY when the seam is unconfigured (no QA_AUTH_TOKEN) so
 * callers can fall back to UI signup. When the token IS configured the seam
 * is required infrastructure: transport errors, non-2xx (expired/rotated
 * token, 5xx, Access wall) and non-JSON bodies THROW with names-only detail
 * instead of collapsing into a silent skip. Staging-only: this endpoint must
 * never exist in production.
 */
export async function tryMintTestAccount(
  request: APIRequestContext,
  baseURL: string,
  plan?: string,
): Promise<TestAccount | null> {
  if (!process.env.QA_AUTH_TOKEN) return null;
  const url = `${baseURL}${API.testAccounts}`;
  let status: number;
  let headers: Record<string, string>;
  let bodyText: string;
  try {
    const response = await apiPost(
      request,
      url,
      { ...accessHeaders(), ...qaAuthHeader(), 'Content-Type': 'application/json' },
      { email: uniqueTestEmail(plan ? `e2e-${plan}` : 'e2e'), ...(plan ? { plan } : {}) },
      'seam',
    );
    status = response.status();
    headers = response.headers();
    bodyText = await response.text().catch(() => '');
  } catch (error) {
    // apiPost's no-follow throws (redirect/wall) already name the cause —
    // preserve them instead of collapsing into a generic transport failure.
    if (error instanceof Error && /unexpected redirect|Access wall/.test(error.message)) throw error;
    throw new Error(
      '[seam] POST /api/test-accounts transport failed (names only — no values logged). ' +
        'The seam is required once QA_AUTH_TOKEN is set; refusing to skip silently.',
    );
  }
  assertNoAccessWall({ status, headers, bodyText, url }, 'seam');
  if (status < 200 || status >= 300) {
    throw new Error(
      `[seam] POST /api/test-accounts failed: HTTP ${status} (seam outage — not a skip; ` +
        'names only, no values logged).',
    );
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error('[seam] POST /api/test-accounts returned a non-JSON body (seam outage — not a skip).');
  }
  // A 2xx alone does not prove an account was minted: require a plain
  // object carrying a nonblank identity email. Never fabricate the email
  // client-side — a malformed success body is a seam failure, not an account.
  const body: Record<string, unknown> =
    parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : {};
  const email = nonblankString(body.email);
  if (!email) {
    throw new Error(
      '[seam] POST /api/test-accounts returned no usable account identity: ' +
        'the response is not an object with a nonblank email identity ' +
        '(names only — no values logged; refusing to fabricate an account).',
    );
  }
  // Documented variants: session preferred, token otherwise; the first
  // nonblank string wins. No usable variant stays a setup-only result
  // (session undefined) — never a blank-string session.
  const session = nonblankString(body.session) ?? nonblankString(body.token);
  return { email, session, viaApi: true };
}

/**
 * Complete the BYOP wizard for the current page identity: point it at the
 * synthetic NNTP endpoint and save. Every identity that ingests runs this
 * first — the ingest worker probes through the user's authenticated provider
 * connection, so an account that never ran the wizard cannot ingest. Fails
 * closed when SYNTHETIC_NNTP_PASS is missing (no default, never logged).
 */
export async function completeByopWizard(page: Page, baseURL: string): Promise<void> {
  const endpoint = syntheticNntpEndpoint();
  const password = process.env.SYNTHETIC_NNTP_PASS;
  if (!password) {
    throw new Error(
      '[byop] SYNTHETIC_NNTP_PASS is not set. The wizard must point at the hosted ' +
        'synthetic endpoint or test provider — refusing to submit an empty credential. ' +
        'See e2e/README.md.',
    );
  }
  await page.goto(`${baseURL}/onboarding/byop`, {
    waitUntil: 'domcontentloaded',
    timeout: 30_000,
  });
  await expect(page.getByTestId(TESTIDS.byopHost)).toBeVisible({ timeout: 15_000 });
  await page.getByTestId(TESTIDS.byopHost).fill(endpoint.host);
  await page.getByTestId(TESTIDS.byopPort).selectOption(String(endpoint.port));
  await page.getByTestId(TESTIDS.byopUser).fill(endpoint.user);
  await page.getByTestId(TESTIDS.byopPass).fill(password);
  if (endpoint.tls) {
    const tls = page.getByTestId(TESTIDS.byopTls);
    if ((await tls.count()) > 0 && (await tls.getAttribute('type')) === 'checkbox') {
      const checked = await tls.isChecked().catch(() => false);
      if (!checked) await tls.check().catch(() => {});
    }
  }
  await page.getByRole('button', { name: 'Continue' }).click();
  await page.getByRole('button', { name: 'Continue' }).click();
  await page.getByTestId(TESTIDS.byopTest).click();
  const status = page.getByTestId(TESTIDS.byopStatus);
  await expect(status, 'BYOP test-connection gave no status').toBeVisible({ timeout: 60_000 });
  await expect(status, 'BYOP test-connection to the synthetic endpoint failed').toContainText(
    /success|connected|ok/i,
    { timeout: 60_000 },
  );
  await page.getByTestId(TESTIDS.byopSave).click();
  await expect(page, 'BYOP save did not reach the library').toHaveURL(/\/library/, {
    timeout: 30_000,
  });
}
