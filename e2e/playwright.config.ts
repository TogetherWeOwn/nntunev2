import { defineConfig, devices } from '@playwright/test';
import { accessHeaders, stagingBaseUrl } from './helpers/access.js';

// Staging base URL is injected by CI (secrets.STAGING_BASE_URL) or a local
// shell export. It is intentionally optional at config load: specs skip with
// a clear notice when staging is not configured (pre-deploy), and fail
// closed once it is. See e2e/README.md for the activation progression.
const baseURL = stagingBaseUrl('skip', 'config') ?? undefined;

// Validate the Access pair at startup when staging is configured
// (fail-closed before any request). Headers are deliberately NOT installed at
// context level: extraHTTPHeaders would attach the service token and session
// bearer to EVERY origin, including cross-origin subresources. Each spec
// scopes them to the staging origin via applyAccountToPage (origin-checked
// context.route), so fonts/analytics/stream URLs never see them.
accessHeaders();

export default defineConfig({
  testDir: './tests',
  // Ingest polling can take minutes; the ingest spec raises this further.
  timeout: 120_000,
  expect: { timeout: 15_000 },
  // One worker: journey specs each mint a fresh account, but serial execution
  // keeps staging logs and the usage meter deterministic per run.
  workers: 1,
  fullyParallel: false,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI
    ? [['list'], ['html', { open: 'never' }]]
    : [['list']],
  use: {
    baseURL,
    // Raw traces capture request headers/bodies (Access, session and NNTP
    // credentials); screenshots may expose account/provider details. Keep
    // CI evidence textual until a verified redaction path exists. Note:
    // Playwright still writes error-context.md snapshots (which render filled
    // input values) on page-test failure — the workflow sanitizes those paths
    // before uploading artifacts. See e2e-staging.yml.
    trace: 'off',
    screenshot: 'off',
  },
  projects: [
    {
      name: 'staging',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
