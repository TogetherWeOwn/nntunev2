import { test, expect } from '@playwright/test';
import { uniqueTestEmail } from '../helpers/contract.js';
import { stagingBaseUrl } from '../helpers/access.js';
import {
  applyAccountToPage,
  retrieveStagedSignupCode,
  tryMintTestAccount,
  waitForHydration,
} from '../helpers/accounts.js';

// Returning-user login leg: proves the real `/login` magic-link form signs
// an existing account in, redeeming the staged code on the `/auth/magic`
// confirm page and landing on provider setup. The account under test comes
// from the test-account seam (`POST /api/test-accounts`), and the code from
// the staging-only retrieval seam (`POST /api/test-codes`, paired
// staging-seam build slice) — the page form stays the only sign-in path
// because the page starts unauthenticated (null identity below).
//
// - File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
//   without launching a browser. See health.spec.ts.
// - QA_AUTH_TOKEN guard: the mint and code seams need it. Missing token
//   skips locally and fails closed on CI (a skip on CI would leave
//   returning-user login never exercised while the job stays green).
// - Seam-absent failure signature (documented on the card): with the token
//   set but either seam not yet deployed, the helper throws
//   `[seam] POST /api/test-accounts failed: ...` or
//   `[code-seam] POST /api/test-codes failed: ...` and the live test goes
//   red — that red IS the gap signal, not noise.
// - Diagnostics name no mailbox or token values: assertion messages carry
//   fixed context only (see the helpers).
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
  if (process.env.CI) throw new Error(`[login-magic-link] ${reason}`);
  test.skip(true, reason);
}

test('returning user signs in with an emailed magic link and lands on provider setup', async ({
  page,
  request,
}) => {
  const baseURL = stagingBaseUrl('skip', 'login-magic-link');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  requireSeamToken('returning-user login');
  const minted = await tryMintTestAccount(request, baseURL);
  if (!minted) {
    throw new Error(
      '[login-magic-link] no minted account: the returning-user leg requires ' +
        'POST /api/test-accounts once QA_AUTH_TOKEN is set ' +
        '(names only — no values logged). See e2e/README.md.',
    );
  }
  // Null identity: Access pair only, no account session. The page starts
  // unauthenticated, so the form below is the only sign-in path. Must
  // precede the first navigation.
  await applyAccountToPage(page, null, baseURL);
  await page.goto(`${baseURL}/login`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
  await waitForHydration(page, 'login');
  await expect(page.getByRole('heading', { level: 1 }), 'login page did not render').toContainText(
    'Sign in',
  );
  const emailField = page.locator('input[name="email"]');
  await expect(emailField, 'login email field not visible').toBeVisible({ timeout: 15_000 });
  await emailField.fill(minted.email);
  await page.getByRole('button', { name: /email me a sign-in link/i }).click();
  // Generic either way: the API never reveals which emails exist, so a known
  // address lands on the same sent state as an unknown one (pinned below).
  await expect(
    page.getByRole('status'),
    'login did not reach the generic sent state',
  ).toContainText(/check your inbox/i, { timeout: 15_000 });
  // Staging-only seam: the issued code for this existing mailbox, without
  // minting. Absent seam fails here by name — never by redeeming a code
  // that was never issued.
  const code = await retrieveStagedSignupCode(request, baseURL, minted.email);
  if (!code) {
    throw new Error(
      '[login-magic-link] no staged login code: this returning-user step requires ' +
        'QA_AUTH_TOKEN plus the staging code-retrieval seam for example.test mailboxes ' +
        '(names only — no values logged). See e2e/README.md.',
    );
  }
  // The Worker mails `{origin}/auth/magic?token=…`: the landing never
  // redeems on load — the token is spent only on the explicit confirm below.
  await page.goto(`${baseURL}/auth/magic?token=${encodeURIComponent(code)}`, {
    waitUntil: 'domcontentloaded',
    timeout: 30_000,
  });
  await waitForHydration(page, 'login-magic-link');
  const confirm = page.getByRole('button', { name: /sign in with this link/i });
  await expect(confirm, 'magic-link confirm not offered').toBeVisible({ timeout: 15_000 });
  await confirm.click();
  // The app serves provider setup at `/onboarding/byop` (`+server.ts` on
  // `/onboarding/provider` unconditionally 307-redirects there): the page
  // navigates there only after the finish succeeds server-side and mints the
  // session cookie, so the landing URL plus the wizard heading together are
  // the session proof (a failed finish stays on `/auth/magic` with the
  // generic error).
  await expect(page, 'magic-link redeem did not land on provider setup').toHaveURL(
    /\/onboarding\/byop/,
    { timeout: 30_000 },
  );
  expect(page.url(), 'magic-link redeem did not land on provider setup').toContain(
    '/onboarding/byop',
  );
  await expect(
    page.getByRole('heading', { level: 1 }),
    'provider setup not visible after magic-link sign-in',
  ).toContainText(/connect your provider/i, { timeout: 15_000 });
});

test('login shows the same generic sent state for an unknown address', async ({ page }) => {
  const baseURL = stagingBaseUrl('skip', 'login-generic-sent');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  // Null identity: the probe address was never registered, so any
  // address-specific answer here would be an enumeration oracle.
  await applyAccountToPage(page, null, baseURL);
  await page.goto(`${baseURL}/login`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
  await waitForHydration(page, 'login');
  const emailField = page.locator('input[name="email"]');
  await expect(emailField, 'login email field not visible').toBeVisible({ timeout: 15_000 });
  await emailField.fill(uniqueTestEmail('e2e-unknown'));
  await page.getByRole('button', { name: /email me a sign-in link/i }).click();
  await expect(
    page.getByRole('status'),
    'login enumerated an unknown address instead of the generic sent state',
  ).toContainText(/check your inbox/i, { timeout: 15_000 });
});

test('login surfaces a generic error when the magic-link request fails', async ({ page }) => {
  const baseURL = stagingBaseUrl('skip', 'login-error');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  await applyAccountToPage(page, null, baseURL);
  // Force the backend failure locally: the page must settle on its generic
  // error, never a spinner and never a backend-shaped message.
  await page.route('**/v1/auth/magic/start', async (route) => {
    await route.fulfill({
      status: 500,
      contentType: 'application/json',
      body: JSON.stringify({ error: 'internal' }),
    });
  });
  await page.goto(`${baseURL}/login`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
  await waitForHydration(page, 'login');
  const emailField = page.locator('input[name="email"]');
  await expect(emailField, 'login email field not visible').toBeVisible({ timeout: 15_000 });
  await emailField.fill(uniqueTestEmail('e2e-error'));
  await page.getByRole('button', { name: /email me a sign-in link/i }).click();
  await expect(
    page.getByRole('alert'),
    'login did not surface the generic error state',
  ).toContainText(/something went wrong/i, { timeout: 15_000 });
});
