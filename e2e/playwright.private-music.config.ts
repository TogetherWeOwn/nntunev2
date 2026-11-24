import { defineConfig } from '@playwright/test';

// Fixture tests only. This config has no staging origin, browser fixtures
// in use, or admission reporter. The staging config discovers ./tests only.
export default defineConfig({
  testDir: './fixtures/private-music',
  reporter: 'list',
  retries: 0,
});
