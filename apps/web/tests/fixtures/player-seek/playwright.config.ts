import { defineConfig } from '@playwright/test';

// The two specs also run in the main web-e2e shards. The focused command
// needs only the per-worker fixture server and installed Chromium.
export default defineConfig({
	testDir: '../..',
	testMatch: ['player-range-seek.spec.ts', 'player-seek.spec.ts'],
	use: { trace: 'retain-on-failure' }
});
