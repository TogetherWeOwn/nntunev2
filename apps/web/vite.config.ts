/// <reference types="vitest/config" />
import adapter from '@sveltejs/adapter-cloudflare';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// Build-time commit SHA for the deployed-build identity surface
// (`src/lib/build.ts` -> `GET /api/build`). GitHub Actions always sets
// `GITHUB_SHA`, so staging builds bake the exact head with no workflow
// change; `BUILD_SHA` wins when explicitly provided (e.g. promotion lanes).
// Falls back to explicit "unknown", never silent. No node types needed.
const buildSha =
	(globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env
		?.BUILD_SHA ??
	(globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env
		?.GITHUB_SHA ??
	'unknown';

export default defineConfig({
	define: {
		__BUILD_SHA__: JSON.stringify(buildSha)
	},
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			// Cloudflare Workers (serverless), via the tracked wrangler.toml
			// in this directory; local dev/checks run without deploying.
			adapter: adapter()
		})
	],
	test: {
		include: ['src/**/*.test.ts']
	}
});
