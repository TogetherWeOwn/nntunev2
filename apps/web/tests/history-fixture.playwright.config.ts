import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: './history-fixture',
	testMatch: '**/*.fixture.ts',
	use: { baseURL: 'http://127.0.0.1:43827', trace: 'retain-on-failure' },
	webServer: {
		command: 'pnpm exec svelte-kit sync && pnpm exec vite build --config tests/history-fixture.vite.config.ts && pnpm exec vite preview --config tests/history-fixture.vite.config.ts --host 127.0.0.1 --port 43827 --strictPort',
		port: 43827,
		reuseExistingServer: false,
		timeout: 180_000
	}
});
