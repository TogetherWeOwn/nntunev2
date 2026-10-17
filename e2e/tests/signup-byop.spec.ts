import { test, expect } from '@playwright/test';
import { TESTIDS } from '../helpers/contract.js';
import { stagingBaseUrl } from '../helpers/access.js';
import {
  applyAccountToPage,
  completeByopWizard,
  submitPublicUiSignup,
  tryMintTestAccount,
  type TestAccount,
} from '../helpers/accounts.js';

// Serial file: the signup test shares its account with the BYOP test so the
// wizard runs authenticated. Retries re-run a single test, so the BYOP test
// mints a fallback account when no shared one exists.
let sharedAccount: TestAccount | null = null;

// Golden path part 1: signup → BYOP wizard pointing at the synthetic NNTP
// endpoint. Each run mints a fresh account so reruns never collide.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser. See health.spec.ts.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

test.describe.serial('signup → BYOP wizard', () => {
  test('signup creates an account and lands on the BYOP wizard', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'signup');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const minted = await tryMintTestAccount(request, baseURL);
    // Authenticate before the first navigation: the origin-scoped route
    // applies to subsequent loads only, so this must precede every goto.
    await applyAccountToPage(page, minted, baseURL);
    sharedAccount = minted;
    if (minted?.viaApi) {
      // API seam minted the account; the wizard must still be reachable.
      await page.goto(`${baseURL}/onboarding/byop`, {
        waitUntil: 'domcontentloaded',
        timeout: 30_000,
      });
      await expect(
        page.getByTestId(TESTIDS.byopHost),
        'BYOP wizard not reachable after API signup',
      ).toBeVisible({ timeout: 15_000 });
      return;
    }

    await submitPublicUiSignup(page, baseURL);
  });

  test('BYOP wizard connects to the synthetic NNTP endpoint and saves', async ({
    page,
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'byop');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Prefer the signup test's account; mint a fallback on retry isolation.
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    await applyAccountToPage(page, account, baseURL);
    // Shared helper (also used by the ingest spec): connects the synthetic
    // endpoint, asserts test-connection success, saves into the library.
    // Fails closed when SYNTHETIC_NNTP_PASS is missing.
    await completeByopWizard(page, baseURL);
  });
});

// Dedicated public-form coverage, deliberately outside the serial describe
// above: it shares no account with the BYOP test and never consults the
// account seam, so it exercises the real /signup submission on every
// configured-staging run — including seam-backed ones, where the first
// test takes the API path. Playwright gives each test a fresh context, so
// no session from an earlier test can leak into this one. The offline pin
// in workflows.spec.ts isolates this case by title and forbids a mint in it.
test('public UI signup succeeds even when the test-account seam is configured', async ({ page }) => {
  const baseURL = stagingBaseUrl('skip', 'ui-signup');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  // Null identity: Access pair only, no account session. The page starts
  // unauthenticated regardless of seam configuration, so the form below is
  // the only account-creation path. Must precede the first navigation.
  await applyAccountToPage(page, null, baseURL);
  // Asserts: unique synthetic identity submitted, no inline signup error,
  // BYOP wizard reached at /onboarding/byop. Diagnostics name no values.
  await submitPublicUiSignup(page, baseURL);
});
