import { test, expect } from '@playwright/test';
import { TESTIDS, uniqueTestEmail } from '../helpers/contract.js';
import { accessHeaders, stagingBaseUrl, syntheticNntpEndpoint } from '../helpers/access.js';
import { tryMintTestAccount } from '../helpers/accounts.js';

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

    const email = uniqueTestEmail();
    await page.goto(`${baseURL}/signup`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    await expect(page.getByTestId(TESTIDS.signupEmail)).toBeVisible({ timeout: 15_000 });
    await page.getByTestId(TESTIDS.signupEmail).fill(email);
    await page.getByTestId(TESTIDS.signupSubmit).click();
    // Either we land on the wizard, or an inline error explains why.
    const wizard = page.getByTestId(TESTIDS.byopHost);
    const error = page.getByTestId(TESTIDS.signupError);
    await expect(async () => {
      const onWizard = await wizard.isVisible().catch(() => false);
      const onError = await error.isVisible().catch(() => false);
      expect(onWizard || onError, 'neither BYOP wizard nor signup error appeared').toBe(true);
    }).toPass({ timeout: 30_000 });
    expect(await error.isVisible().catch(() => false), 'signup rejected the test mailbox').toBe(
      false,
    );
    expect(page.url()).toContain('/onboarding/byop');
  });

  test('BYOP wizard connects to the synthetic NNTP endpoint and saves', async ({ page }) => {
    const baseURL = stagingBaseUrl('skip', 'byop');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
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
    const extraHeaders = accessHeaders();
    if (Object.keys(extraHeaders).length > 0) {
      await page.setExtraHTTPHeaders(extraHeaders);
    }
    await expect(page.getByTestId(TESTIDS.byopHost)).toBeVisible({ timeout: 15_000 });
    await page.getByTestId(TESTIDS.byopHost).fill(endpoint.host);
    await page.getByTestId(TESTIDS.byopPort).fill(String(endpoint.port));
    await page.getByTestId(TESTIDS.byopUser).fill(endpoint.user);
    await page.getByTestId(TESTIDS.byopPass).fill(password);
    if (endpoint.tls) {
      const tls = page.getByTestId(TESTIDS.byopTls);
      if ((await tls.count()) > 0 && (await tls.getAttribute('type')) === 'checkbox') {
        const checked = await tls.isChecked().catch(() => false);
        if (!checked) await tls.check().catch(() => {});
      }
    }
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
  });
});
