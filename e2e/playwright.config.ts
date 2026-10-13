import { defineConfig, devices } from '@playwright/test';
import { accessHeaders } from './helpers/access.js';

// Staging base URL is injected by CI (secrets.STAGING_BASE_URL) or a local
// shell export. It is intentionally optional at config load: specs skip with
// a clear notice when staging is not configured (pre-deploy), and fail
// closed once it is. See e2e/README.md for the activation progression.
const baseURL = process.env.STAGING_BASE_URL || undefined;

// Cloudflare Access service-token headers at context level so EVERY page
// navigation carries them — specs must never rely on per-call headers for
// browser loads. Half-set pair throws here (fail-closed at startup).
const accessHeaderMap = accessHeaders();

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
    ...(Object.keys(accessHeaderMap).length > 0 ? { extraHTTPHeaders: accessHeaderMap } : {}),
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
