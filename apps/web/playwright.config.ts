import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: './tests',
	reporter: process.env['NNTUNE_E2E_HTML_REPORT'] === 'true' ? [['dot'], ['html']] : undefined,
	use: { baseURL: 'http://localhost:4173', trace: 'retain-on-failure' },
	webServer: {
		command: 'pnpm build && pnpm preview --port 4173 --strictPort',
		// This server has no history backend; the real-route fixture covers the enabled path.
		env: { PUBLIC_LISTEN_HISTORY_ENABLED: 'false' },
		port: 4173,
		reuseExistingServer: !process.env['CI'],
		timeout: 180_000
	}
});
