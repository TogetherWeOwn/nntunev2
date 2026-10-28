import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: './tests',
	// Fixed hostile-preview measurements require one worker, without overlapping shapes.
	workers: 1,
	use: { baseURL: 'http://localhost:4173', trace: 'retain-on-failure' },
	webServer: {
		command: 'pnpm build && pnpm preview --port 4173 --strictPort',
		port: 4173,
		reuseExistingServer: !process.env['CI'],
		timeout: 180_000
	}
});
