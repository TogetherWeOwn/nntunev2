import { defineConfig } from '@playwright/test';
import base from '../playwright.config.js';

/** Test-only isolation: never reuse a different worktree's default-port preview. */
export default defineConfig({
	...base,
	testDir: '.',
	use: { ...base.use, baseURL: 'http://localhost:43819' },
	webServer: {
		command: 'pnpm build && pnpm preview --port 43819 --strictPort',
		cwd: decodeURIComponent(new URL('..', import.meta.url).pathname),
		port: 43819,
		reuseExistingServer: false,
		timeout: 180_000
	}
});
