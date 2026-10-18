import { test } from '@playwright/test';
import { stagingBaseUrl } from '../helpers/access.js';
import { assertServingBuildMatchesPromotion } from '../helpers/build-identity-live.js';

// CLOSING same-build assertion for exact-head staging evidence (row 7):
// re-reads `GET /api/build` after every other spec and requires it to still
// equal the promotion SHA. The `zz-` prefix sorts this file last under the
// serial config (`workers: 1`, `fullyParallel: false` in
// `e2e/playwright.config.ts`; order pinned offline by
// `e2e/tests/workflows.spec.ts`), so a green run proves the whole journey
// ran against the promoted build — not just its first minutes.
//
// Why this exists: the deploy releases its job-level concurrency group when
// the deploy finishes while E2E serializes separately, so a later main push
// can deploy staging mid-journey after the opening read
// (`e2e/tests/build-identity.spec.ts`) passes. That timing now fails here
// with a "superseded by a newer deploy" message instead of passing green;
// the reverse timing (a newer deploy lands before the opening read) fails
// the opening leg. Either way no green run can cite a mixed-build journey
// as same-build evidence. Deploy concurrency is intentionally unchanged.
//
// NO file-level skip by design (like the opening leg): the live test skips
// green when STAGING_BASE_URL is unset and fails closed once configured.
// Runs only in the full journey (`npm run e2e:staging`).

test('deployed build still matches the promotion SHA at journey end (superseded-deploy guard)', async ({
  request,
}) => {
  const baseURL = stagingBaseUrl('skip', 'build-identity-final');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  try {
    await assertServingBuildMatchesPromotion(request, baseURL, 'build-identity-final');
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    // A changed serving build voids the journey's same-build evidence: name
    // the supersession instead of reporting a bare mismatch. Transport, wall
    // and shape errors propagate unchanged (they void the evidence too).
    if (/build identity (mismatch|is unknown|is malformed)/.test(message)) {
      throw new Error(
        '[build-identity-final] staging served a different build at the end of the ' +
          'journey than the promotion SHA — superseded by a newer deploy during the run; ' +
          'same-build evidence is void. ' +
          message,
      );
    }
    throw error;
  }
});
