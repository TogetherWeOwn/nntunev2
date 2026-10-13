import { defineConfig, devices } from '@playwright/test';

// Staging base URL is injected by CI (secrets.STAGING_BASE_URL) or a local
// shell export. It is intentionally optional at config load: specs skip with
// a clear notice when staging is not configured (pre-deploy), and fail
// closed once it is. See e2e/README.md for the activation progression.
const baseURL = process.env.STAGING_BASE_URL || undefined;

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
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'staging',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
