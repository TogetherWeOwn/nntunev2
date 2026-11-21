import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: './tests',
	reporter: process.env['NNTUNE_E2E_HTML_REPORT'] === 'true' ? [['dot'], ['html']] : undefined,
	use: { baseURL: 'http://localhost:4173', trace: 'retain-on-failure' },
	webServer: {
		command: 'pnpm build --mode browser-fixtures && pnpm preview --port 4173 --strictPort',
		port: 4173,
		reuseExistingServer: !process.env['CI'],
		timeout: 180_000
	}
});
