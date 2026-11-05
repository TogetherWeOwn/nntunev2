import { test, expect } from '@playwright/test';
import { stagingBaseUrl } from '../helpers/access.js';
import {
  annotateSignupPath,
  applyAccountToPage,
  submitPublicUiSignupToBilling,
} from '../helpers/accounts.js';

// Billing-arrival leg for roadmap M1 traceability row 1: proves the real
// `/signup` form reaches `/account/billing` via a WebAuthn virtual authenticator
// plus the staging-only emailed-code retrieval seam
// (`POST /api/test-codes`, paired staging-seam build slice).
//
// - File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
//   without launching a browser. See health.spec.ts.
// - QA_AUTH_TOKEN guard: the seam needs it. Missing token skips locally and
//   fails closed on CI (a skip on CI would leave billing arrival never
//   exercised while the job stays green).
// - Seam-absent failure signature (documented on the card): with the token
//   set but the seam not yet deployed, the helper throws
//   `[code-seam] POST /api/test-codes failed: HTTP 404 (seam absent or
//   outage — names only, no values logged). ...` and this test goes red —
//   that red IS the gap signal, not noise.
// - No mint: this case never consults the test-account seam
//   (`tryMintTestAccount` / `POST /api/test-accounts`), so the account under
//   test comes only from the public form. The dedicated no-mint case in
//   `signup.spec.ts` stays mint-free alongside (pinned offline in
//   `workflows.spec.ts`).
// - Diagnostics name no mailbox or token values: assertion messages carry
//   fixed context only (see the helper).
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

test('public UI signup with virtual authenticator reaches billing', async ({
  page,
  request,
}, testInfo) => {
  const baseURL = stagingBaseUrl('skip', 'billing-arrival');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  if (!process.env.QA_AUTH_TOKEN) {
    const reason =
      'billing-arrival needs QA_AUTH_TOKEN + POST /api/test-codes ' +
      '(staging code seam) — refusing to pass silently on CI.';
    if (process.env.CI) throw new Error(`[billing-arrival] ${reason}`);
    test.skip(true, reason);
    return;
  }
  // Evidence provenance: the bounded public-UI label (no account data).
  // This case never mints, so the label is always the form path.
  annotateSignupPath(testInfo, null);
  // Null identity: Access pair only, no account session. The page starts
  // unauthenticated, so the form below is the only account-creation path.
  // Must precede the first navigation.
  await applyAccountToPage(page, null, baseURL);
  // Full form flow: email submit → seam code → virtual-passkey finish →
  // billing arrival asserted inside the helper. Diagnostics name no values.
  await submitPublicUiSignupToBilling(page, request, baseURL);
  // Belt-and-braces arrival assertion at the spec level (the helper already
  // checks the authenticated no-subscription state): URL, heading and action.
  expect(page.url(), 'public UI signup did not land on billing').toContain('/account/billing');
  await expect(
    page.getByRole('heading', { level: 1 }),
    'billing heading not visible after public UI signup',
  ).toContainText('Billing', { timeout: 15_000 });
  await expect(page.getByText('to see your billing.', { exact: false })).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'See plans', exact: true })).toBeVisible();
});
