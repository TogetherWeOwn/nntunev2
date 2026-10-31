import {
  expect,
  request as playwrightRequest,
  type APIRequestContext,
  type APIResponse,
  type Locator,
  type Page,
} from '@playwright/test';
import { API, TESTIDS, uniqueTestEmail } from './contract.js';
import {
  accessHeaders,
  assertNoAccessWall,
  isStagingOrigin,
  qaAuthHeader,
  syntheticNntpEndpoint,
  type WallProbe,
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

/**
 * Bounded evidence label for which signup path a test's account came from.
 * M1 exit evidence must distinguish the real public UI signup from the
 * API test-account seam: a seam-backed run does not prove public signup.
 * The label is one of exactly these two fixed strings — never an email,
 * account id, session/token, provider setting or any other account data.
 */
export type SignupPathLabel = 'public-ui-signup' | 'api-minted-account';

/**
 * Label for the branch selected by a tryMintTestAccount result: a minted
 * (viaApi) account is the seam path; null (seam unconfigured) means the
 * caller proceeds to the public UI signup form. Call only at that branch
 * point — a null result elsewhere does not imply a UI signup happened.
 */
export function signupPathLabel(minted: TestAccount | null): SignupPathLabel {
  return minted?.viaApi ? 'api-minted-account' : 'public-ui-signup';
}

/**
 * Record the signup-path label as a test annotation so it rides along in
 * reporter output (the sanitized junit CI artifact) next to the test
 * result. The list reporter does not print annotations, and green runs
 * upload no artifacts, so the staging workflow additionally publishes the
 * label to the run's step summary on every configured run. A passing
 * test's label is the path that produced its evidence; on failure it names
 * the path under exercise. The annotation carries only the bounded label
 * above (names only — no values logged).
 */
export function annotateSignupPath(
  target: { annotations: Array<{ type: string; description?: string }> },
  minted: TestAccount | null,
): SignupPathLabel {
  const label = signupPathLabel(minted);
  target.annotations.push({ type: 'signup-path', description: label });
  return label;
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
 * No-follow OPTIONS for CORS preflight probes. Same contract as apiGet:
 * fetches with maxRedirects: 0 so a staging 3xx is returned as-is (never
 * followed with credentials); a 3xx throws via throwOnRedirect instead of
 * surfacing as a bare status mismatch. Live specs must use this instead of
 * a raw fetch with an OPTIONS method.
 */
export async function apiOptions(
  request: APIRequestContext,
  url: string,
  headers: Record<string, string>,
  context: string,
  opts?: { timeout?: number },
): Promise<APIResponse> {
  const res = await request.fetch(url, {
    method: 'OPTIONS',
    headers,
    timeout: opts?.timeout ?? 30_000,
    maxRedirects: 0,
  });
  throwOnRedirect(res.status(), res.headers(), context);
  return res;
}

/**
 * Anonymous GET in a fresh, isolated request context: no Access pair, no
 * bearer, no cookies or storage state — the context is created here, not
 * borrowed from a test fixture or browser context, and no headers are
 * attached regardless of what the environment holds. Fetches with
 * maxRedirects: 0 and returns the response as a WallProbe WITHOUT
 * throwOnRedirect: a 302 to the Access login is the expected denial
 * evidence here, and no-follow guarantees it is reported, not chased.
 * Pair with `assertAccessWall` (helpers/access.ts). Transport failures
 * throw names-only — neither the URL nor any credential value is logged.
 */
export async function anonymousApiGet(
  url: string,
  context: string,
  opts?: { timeout?: number },
): Promise<WallProbe> {
  const anon = await playwrightRequest.newContext();
  try {
    let res: APIResponse;
    try {
      res = await anon.get(url, { timeout: opts?.timeout ?? 30_000, maxRedirects: 0 });
    } catch {
      throw new Error(
        `[${context}] anonymous request transport failed (names only — no values logged). ` +
          'Refusing to treat a transport failure as an Access denial.',
      );
    }
    const bodyText = await res.text().catch(() => '');
    return { status: res.status(), headers: res.headers(), bodyText, url };
  } finally {
    await anon.dispose().catch(() => {});
  }
}

/**
 * Anonymous GET: NO Access pair, NO Authorization, NO cookies — by
 * construction, not by passing empty credentials through a credentialed
 * wrapper. This helper never reads the environment and takes no headers
 * parameter, so no caller can accidentally credential it. The caller MUST
 * pass a fresh, isolated APIRequestContext (a new Playwright request
 * context with no storageState or context-level headers) so no ambient
 * session can ride along.
 *
 * Unlike apiGet it does NOT throw on 3xx: it fetches with maxRedirects: 0
 * and returns the response as-is so the caller can classify it with
 * assertAccessWall. Nothing is followed, so a redirect target (Access login
 * or otherwise) receives zero follow-up requests from this probe.
 */
export async function anonymousGet(
  request: APIRequestContext,
  url: string,
  opts?: { timeout?: number },
): Promise<APIResponse> {
  return request.get(url, {
    headers: {},
    timeout: opts?.timeout ?? 30_000,
    maxRedirects: 0,
  });
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
      // A fetch still in flight when the test ends rejects with "Test ended"
      // and fails an otherwise green test. Abort instead: the page sees a
      // failed request, so a mid-test transport failure still fails closed.
      try {
        const response = await route.fetch({
          headers: { ...route.request().headers(), ...stagingHeaders },
          maxRedirects: 0,
        });
        await route.fulfill({ response });
      } catch {
        await route.abort().catch(() => {});
      }
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
 * Wait until the SvelteKit client has hydrated the current page (the root
 * layout sets `data-hydrated` on <html> from an effect, which never runs
 * during SSR). The server-rendered form is visible before its client
 * handler is attached: clicking then triggers a native GET submit
 * (`?email=...`) instead of the API call. Staging assets also load through
 * the origin-scoped route proxy, so the gap is wide enough to hit every run.
 * Call after each navigation, before the first interaction.
 */
export async function waitForHydration(page: Page, context: string): Promise<void> {
  await expect(
    page.locator('html[data-hydrated]'),
    `[${context}] page never hydrated (client bundle not started)`,
  ).toBeAttached({ timeout: 30_000 });
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
  await waitForHydration(page, 'byop');
  await expect(page.getByTestId(TESTIDS.byopHost)).toBeVisible({ timeout: 15_000 });
  await page.getByTestId(TESTIDS.byopHost).fill(endpoint.host);
  await page.getByTestId(TESTIDS.byopPort).selectOption(String(endpoint.port));
  await page.getByTestId(TESTIDS.byopUser).fill(endpoint.user);
  await page.getByTestId(TESTIDS.byopPass).fill(password);
  // Fail closed on transport fidelity: explicitly set AND verify the TLS
  // checkbox's requested state. A missing, unusable or un-settable control
  // must stop the wizard here — never submit test/save with whatever
  // transport the page happened to default to.
  const tlsToggle = page.getByTestId(TESTIDS.byopTls);
  const tlsMatches = await tlsToggle.count();
  if (tlsMatches !== 1) {
    throw new Error(
      `[byop] expected exactly one TLS checkbox (${TESTIDS.byopTls}) on the BYOP page, ` +
        `found ${tlsMatches}: cannot verify the requested transport — refusing to test ` +
        'or save. The page contract may have drifted (names only — no values logged).',
    );
  }
  const tlsType = await tlsToggle.getAttribute('type').catch(() => null);
  if (tlsType?.toLowerCase() !== 'checkbox') {
    throw new Error(
      '[byop] the BYOP TLS control is not a checkbox — cannot set or verify the ' +
        'requested TLS state; refusing to test or save (names only — no values logged).',
    );
  }
  // endpoint.tls is always true (TLS-off is rejected in syntheticNntpEndpoint),
  // so the requested state is unconditionally checked.
  try {
    await tlsToggle.check();
    await expect(tlsToggle).toBeChecked();
  } catch {
    // The underlying Playwright error can carry page detail; replace it with
    // a names-only diagnostic instead of letting it propagate.
    throw new Error(
      '[byop] could not set and verify the BYOP TLS checkbox to checked: ' +
        'refusing to test or save with an unverified transport (names only — no values logged).',
    );
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

/** Path the signup form posts to first (`registerStart` in the web client). */
const REGISTER_START_PATH = '/v1/auth/passkey/register/start';

/**
 * Record the HTTP status of the page's next `registerStart` call (status
 * only — never the request or response body).
 */
function recordRegisterStartStatus(page: Page): { status: number | null } {
  const seen: { status: number | null } = { status: null };
  page.on('response', (response) => {
    if (seen.status === null && new URL(response.url()).pathname === REGISTER_START_PATH) {
      seen.status = response.status();
    }
  });
  return seen;
}

/**
 * Wait for the signup submit to settle on the code step or the inline
 * error. On timeout, name the state the page is actually in (fixed labels
 * plus the `registerStart` HTTP status), so a rate limit or paywall (the
 * page returns to the email step and opens the plan-limit notice), a hung
 * request, or a browser without passkeys is distinguishable from a
 * missing form. Never logs field values or response bodies.
 */
export async function awaitSignupOutcome(
  page: Page,
  codeField: Locator,
  error: Locator,
  registerStatus: { status: number | null },
  timeout = 30_000,
): Promise<void> {
  try {
    await expect(async () => {
      const onCodeStep = await codeField.isVisible().catch(() => false);
      const onError = await error.isVisible().catch(() => false);
      expect(onCodeStep || onError).toBe(true);
    }).toPass({ timeout });
  } catch {
    const visible = async (locator: Locator) => locator.isVisible().catch(() => false);
    const state = (await visible(page.getByTestId('plan-limit-notice')))
      ? 'plan-limit notice open (registerStart answered a paywall/limit)'
      : (await visible(page.getByText(/sending your email code/i)))
        ? 'still sending (registerStart pending)'
        : (await visible(page.getByText(/does not support passkeys/i)))
          ? 'passkeys unsupported in this browser'
          : 'back at the email step with no status';
    throw new Error(
      `[signup] neither signup code form nor signup error appeared: ${state}; ` +
        `registerStart HTTP ${registerStatus.status ?? 'not observed'} ` +
        '(names only — no values logged).',
    );
  }
}

/**
 * Submit the public /signup form as a fresh synthetic identity and require
 * the reachable first step of the real two-step flow: submit starts
 * registration (`registerStart`) and the page renders the emailed-code form
 * (`input[name="magic_code"]`, "Finish creating account"). Navigation to the
 * BYOP wizard happens only after the emailed code plus a passkey finish
 * (`registerFinish`), which needs a staging-only code-retrieval seam and a
 * WebAuthn virtual authenticator that do not exist yet — so this helper
 * must NOT assert the wizard, or every configured run goes red. It never
 * touches the test-account seam, so it proves public UI form submission even
 * when the seam is configured. Callers authenticate the page first (a null
 * identity via applyAccountToPage gives the anonymous, Access-only starting
 * point). The generated mailbox is returned for callers that track the
 * identity; it is never interpolated into an assertion message or
 * diagnostic here.
 */
export async function submitPublicUiSignup(page: Page, baseURL: string): Promise<TestAccount> {
  const email = uniqueTestEmail('e2e-ui');
  await page.goto(`${baseURL}/signup`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
  await waitForHydration(page, 'signup');
  await expect(page.getByTestId(TESTIDS.signupEmail)).toBeVisible({ timeout: 15_000 });
  await page.getByTestId(TESTIDS.signupEmail).fill(email);
  const registerStatus = recordRegisterStartStatus(page);
  await page.getByTestId(TESTIDS.signupSubmit).click();
  const codeField = page.locator('input[name="magic_code"]');
  const error = page.getByTestId(TESTIDS.signupError);
  await awaitSignupOutcome(page, codeField, error, registerStatus);
  expect(await error.isVisible().catch(() => false), 'public UI signup showed an inline error').toBe(
    false,
  );
  await expect(codeField, 'public UI signup did not reach the emailed-code step').toBeVisible({
    timeout: 15_000,
  });
  return { email, viaApi: false };
}

/**
 * Retrieve the issued signup code for a synthetic mailbox via the
 * staging-only code seam (`POST /api/test-codes`, paired staging-seam
 * build slice). The seam returns the code without minting an account, only
 * for `example.test` mailboxes and only when the staging flag is set.
 *
 * Returns null ONLY when the seam is unconfigured (no QA_AUTH_TOKEN) so
 * callers can skip locally / fail closed on CI. When the token IS
 * configured the seam is required infrastructure: transport errors,
 * non-2xx (including 404 while the seam is absent) and malformed bodies
 * THROW with names-only detail instead of collapsing into a silent skip.
 * The mailbox and code values are never interpolated into an error.
 */
export async function retrieveStagedSignupCode(
  request: APIRequestContext,
  baseURL: string,
  email: string,
): Promise<string | null> {
  if (!process.env.QA_AUTH_TOKEN) return null;
  const url = `${baseURL}${API.testCodes}`;
  let status: number;
  let headers: Record<string, string>;
  let bodyText: string;
  try {
    const response = await apiPost(
      request,
      url,
      { ...accessHeaders(), ...qaAuthHeader(), 'Content-Type': 'application/json' },
      { email },
      'code-seam',
    );
    status = response.status();
    headers = response.headers();
    bodyText = await response.text().catch(() => '');
  } catch (error) {
    // apiPost's no-follow throws (redirect/wall) already name the cause —
    // preserve them instead of collapsing into a generic transport failure.
    if (error instanceof Error && /unexpected redirect|Access wall/.test(error.message)) throw error;
    throw new Error(
      '[code-seam] POST /api/test-codes transport failed (names only — no values logged). ' +
        'The seam is required once QA_AUTH_TOKEN is set; refusing to skip silently.',
    );
  }
  assertNoAccessWall({ status, headers, bodyText, url }, 'code-seam');
  if (status < 200 || status >= 300) {
    throw new Error(
      `[code-seam] POST /api/test-codes failed: HTTP ${status} (seam absent or outage — ` +
        'names only, no values logged). The staging code-retrieval seam must ' +
        'return the issued code for example.test mailboxes; without it the wizard-arrival ' +
        'spec fails here. See e2e/README.md.',
    );
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error('[code-seam] POST /api/test-codes returned a non-JSON body (seam outage — not a skip).');
  }
  const body: Record<string, unknown> =
    parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : {};
  const code = nonblankString(body.magic_token);
  if (!code) {
    throw new Error(
      '[code-seam] POST /api/test-codes returned no usable code: ' +
        'the response is not an object with a nonblank magic_token identity ' +
        '(names only — no values logged; refusing to fabricate a code).',
    );
  }
  return code;
}

/**
 * Attach a WebAuthn virtual authenticator to the page's browser context so
 * the real `/signup` passkey ceremony completes without hardware. Uses the
 * CDP WebAuthn domain: resident-key + user-verification capable, pre-verified
 * with presence simulation, matching the signup page's `required` ceremony
 * (`apps/web/src/routes/signup/+page.svelte`). Call before submitting the
 * emailed-code form. Failures throw names-only (no device or credential
 * values are ever logged).
 */
export async function enableVirtualAuthenticator(page: Page): Promise<void> {
  try {
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('WebAuthn.enable');
    await cdp.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'usb',
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    });
  } catch {
    throw new Error(
      '[webauthn] could not enable the virtual authenticator (names only — no values logged). ' +
        'The wizard-arrival spec requires a Chromium virtual authenticator; refusing to ' +
        'submit the passkey ceremony without one.',
    );
  }
}

/**
 * Submit the public `/signup` form as a fresh synthetic identity and carry
 * the real two-step flow all the way to the BYOP wizard: submit starts
 * registration (`registerStart`), the emailed-code step is filled with the
 * code from the staging-only retrieval seam (`retrieveStagedSignupCode`),
 * and the passkey finish (`registerFinish`) runs against a WebAuthn virtual
 * authenticator (`enableVirtualAuthenticator`), after which the page
 * navigates to `/onboarding/byop`.
 *
 * Unlike `submitPublicUiSignup` (which stops at the `magic_code` step so
 * every configured run stays green), this helper asserts wizard arrival and
 * therefore requires the seam: without it the code-retrieval POST fails with
 * the documented `[code-seam]` signature. It never touches the test-account
 * mint seam, so the account it creates comes only from the public form. Call
 * only on configured staging with QA_AUTH_TOKEN set (the spec skips green
 * otherwise). The generated mailbox is returned for callers that track the
 * identity; it and the code value are never interpolated into an assertion
 * message or diagnostic here.
 */
export async function submitPublicUiSignupToByopWizard(
  page: Page,
  request: APIRequestContext,
  baseURL: string,
): Promise<TestAccount> {
  const email = uniqueTestEmail('e2e-wizard');
  await page.goto(`${baseURL}/signup`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
  await waitForHydration(page, 'signup');
  await expect(page.getByTestId(TESTIDS.signupEmail)).toBeVisible({ timeout: 15_000 });
  await page.getByTestId(TESTIDS.signupEmail).fill(email);
  const registerStatus = recordRegisterStartStatus(page);
  await page.getByTestId(TESTIDS.signupSubmit).click();
  const codeField = page.locator('input[name="magic_code"]');
  const error = page.getByTestId(TESTIDS.signupError);
  await awaitSignupOutcome(page, codeField, error, registerStatus);
  expect(await error.isVisible().catch(() => false), 'public UI signup showed an inline error').toBe(
    false,
  );
  await expect(codeField, 'public UI signup did not reach the emailed-code step').toBeVisible({
    timeout: 15_000,
  });
  // Staging-only seam: the issued code for this synthetic mailbox, without
  // minting. Absent seam (or absent QA_AUTH_TOKEN) fails here by name —
  // never by asserting the wizard on a code that was never issued.
  const code = await retrieveStagedSignupCode(request, baseURL, email);
  if (!code) {
    throw new Error(
      '[code-seam] no staged signup code: this wizard-arrival step requires QA_AUTH_TOKEN ' +
        'plus the staging code-retrieval seam for example.test mailboxes ' +
        '(names only — no values logged). See e2e/README.md.',
    );
  }
  // Virtual passkey ceremony: the page's `registerFinish` requires a real
  // attestation from `navigator.credentials.create`.
  await enableVirtualAuthenticator(page);
  await codeField.fill(code);
  await page.getByRole('button', { name: 'Finish creating account' }).click();
  // The page navigates to the BYOP wizard only after the emailed code plus
  // the passkey finish succeed server-side. The regex waits for the
  // navigation; the plain-string read below names the arrival path verbatim
  // (kept as a pinnable contract string, never a value).
  await expect(page, 'public UI signup did not reach the BYOP wizard').toHaveURL(
    /\/onboarding\/byop/,
    { timeout: 30_000 },
  );
  expect(page.url(), 'public UI signup did not land on the BYOP wizard').toContain(
    '/onboarding/byop',
  );
  await expect(
    page.getByTestId(TESTIDS.byopHost),
    'BYOP wizard not reachable after public UI signup',
  ).toBeVisible({ timeout: 15_000 });
  return { email, viaApi: false };
}
