import { test, expect } from '@playwright/test';
import { stagingBaseUrl } from '../helpers/access.js';
import {
  annotateSignupPath,
  applyAccountToPage,
  submitPublicUiSignup,
  tryMintTestAccount,
  waitForHydration,
} from '../helpers/accounts.js';

// Golden path part 1: signup → library. Each run mints a fresh account so
// reruns never collide. The seam-absent fallback submits the real /signup
// form and asserts the emailed-code step (the full code-plus-passkey finish
// needs a staging code-retrieval seam plus a WebAuthn virtual authenticator,
// covered separately by signup-arrival.spec.ts).
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser. See health.spec.ts.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

test.describe.serial('signup → library', () => {
  test('signup creates an account and lands on the library', async ({ page, request }, testInfo) => {
    const baseURL = stagingBaseUrl('skip', 'signup');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const minted = await tryMintTestAccount(request, baseURL);
    // Evidence provenance: record which signup path this run exercises
    // (bounded label only — no account data) before either branch runs.
    annotateSignupPath(testInfo, minted);
    // Authenticate before the first navigation: the origin-scoped route
    // applies to subsequent loads only, so this must precede every goto.
    await applyAccountToPage(page, minted, baseURL);
    if (minted?.viaApi) {
      // API seam minted the account; the library must still be reachable.
      await page.goto(`${baseURL}/library`, {
        waitUntil: 'domcontentloaded',
        timeout: 30_000,
      });
      await waitForHydration(page, 'signup');
      await expect(page, 'library not reachable after API signup').toHaveURL(/\/library/, {
        timeout: 15_000,
      });
      await expect(
        page.getByRole('heading', { level: 1 }),
        'library heading not visible after API signup',
      ).toContainText('Library', { timeout: 15_000 });
      return;
    }

    await submitPublicUiSignup(page, baseURL);
  });
});

// Dedicated public-form coverage, deliberately outside the serial describe
// above: it never consults the account seam, so it exercises the real
// /signup submission on every configured-staging run — including seam-backed
// ones, where the first test takes the API path. Playwright gives each test
// a fresh context, so no session from an earlier test can leak into this
// one. The helper asserts the emailed-code step, not library arrival: the
// code-plus-passkey finish needs a staging code-retrieval seam plus a
// WebAuthn virtual authenticator (covered separately by
// signup-arrival.spec.ts; see row 1 of docs/m1-exit-traceability.md). The
// offline pin in workflows.spec.ts isolates this case by title and forbids a
// mint in it.
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
  // emailed-code step reached. Diagnostics name no values.
  await submitPublicUiSignup(page, baseURL);
});
