import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';

// A test-run-owned server: never reuse another worktree's shared preview port.
const port = Number(process.env['SOUND_ACCEPTANCE_PORT'] ?? '43817');
const cwd = fileURLToPath(new URL('../../', import.meta.url));

export default defineConfig({
	testDir: '..',
	workers: 1,
	retries: 0,
	timeout: 20_000,
	expect: { timeout: 3_000 },
	use: { baseURL: `http://127.0.0.1:${port}`, trace: 'retain-on-failure' },
	webServer: {
		cwd,
		command: `pnpm build && pnpm preview --host 127.0.0.1 --port ${port} --strictPort`,
		url: `http://127.0.0.1:${port}/demo/player`,
		reuseExistingServer: false,
		timeout: 180_000
	}
});
