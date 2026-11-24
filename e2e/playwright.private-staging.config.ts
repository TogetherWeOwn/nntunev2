import { defineConfig, devices } from '@playwright/test';

// Explicit post-deploy qualification only. Keep it outside the default
// staging suite and offline fixtures. The output contains fixed verdicts.
export default defineConfig({
  testDir: './private-staging',
  reporter: 'dot',
  outputDir: 'private-staging-results',
  workers: 1,
  retries: 0,
  timeout: 12 * 60_000,
  expect: { timeout: 15_000 },
  use: {
    ...devices['Desktop Chrome'],
    trace: 'off',
    screenshot: 'off',
    video: 'off',
    serviceWorkers: 'block',
  },
});
