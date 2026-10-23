/// <reference types="vitest/config" />
import adapter from '@sveltejs/adapter-cloudflare';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, loadEnv } from 'vite';
import { minify } from 'terser';

import { cspSources } from './src/lib/security/headers.js';

// Build-time commit SHA for the deployed-build identity surface
// (`src/lib/build.ts` -> `GET /api/build`). GitHub Actions always sets
// `GITHUB_SHA`, so staging builds bake the exact head with no workflow
// change; `BUILD_SHA` wins when explicitly provided (e.g. promotion lanes).
// Falls back to explicit "unknown", never silent. No node types needed.
// `||` (not `??`): an empty BUILD_SHA (e.g. an unset optional workflow
// input) must fall through to GITHUB_SHA instead of baking "unknown".
const buildSha =
	(globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env
		?.BUILD_SHA ||
	(globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env
		?.GITHUB_SHA ||
	'unknown';

export default defineConfig(({ command, mode }) => {
	// SvelteKit's `csp` option is baked in at build time, so the API origin
	// must come from the build environment (process env or an `.env` file),
	// not a platform runtime var. `cspSources` returns header-ready quoted
	// keywords (`'self'`); the `csp` option takes bare keywords and adds the
	// quotes (and per-request nonces) itself, so strip them here.
	// `URL.pathname` is percent-encoded (`My%20Projects`) and carries a
	// leading slash before drive letters on Windows (`/C:/...`), so
	// `loadEnv` silently misses `.env` while SvelteKit loads it with the
	// real path — the client then uses the API origin while the compiled
	// CSP stays `'self'`-only. Decode here (this package has no node
	// types, so `fileURLToPath` from `node:url` is unavailable).
	const rawEnvDir = decodeURIComponent(new URL('.', import.meta.url).pathname);
	const envDir = rawEnvDir.replace(/^\/([A-Za-z]:\/)/, '$1');
	// Production builds stay strict-https; dev/test/serve allow http
	// loopback so `workers/api` local development
	// (`http://localhost|127.0.0.1|::1`, see its README) does not abort
	// `vitest`, `vite dev` or `svelte-check` at startup.
	const isProductionBuild = command === 'build' || mode === 'production';
	const apiSources = cspSources(loadEnv(mode, envDir, '').PUBLIC_API_BASE_URL ?? '', {
		allowHttpLoopback: !isProductionBuild
	}).map(
		(source) => source.replaceAll("'", '') as 'self' | `${string}://${string}.${string}`
	);

	return {
		define: {
			__BUILD_SHA__: JSON.stringify(buildSha)
		},
		plugins: [
			// Compress before final chunk hashes are computed. The complementary
			// Terser/Oxc passes save raw bytes without changing the bundle budget,
			// removing features, or enabling unsafe/property-mangling transforms.
			{
				name: 'compress-production-chunks',
				apply: 'build',
				enforce: 'post',
				renderChunk: {
					order: 'post',
					async handler(code, _chunk, options) {
						if (options.format !== 'es') return null;
						const result = await minify(code, {
							module: true,
							ecma: 2020,
							compress: { passes: 3 },
							sourceMap: { asObject: true }
						});
						if (result.code === undefined) {
							throw new Error('Production compression returned no code');
						}
						return { code: result.code, map: result.map ? JSON.stringify(result.map) : null };
					}
				}
			},
			sveltekit({
				compilerOptions: {
					// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
					runes: ({ filename }) =>
						filename.split(/[/\\]/).includes('node_modules') ? undefined : true
				},

				// Cloudflare Workers (serverless), via the tracked wrangler.toml
				// in this directory; local dev/checks run without deploying.
				adapter: adapter(),

				// Nonce mode: every inline script/style SvelteKit emits carries
				// a per-request nonce, so scripts need neither 'unsafe-inline'
				// nor 'unsafe-eval'. `style-src-attr 'unsafe-inline'` covers
				// style attributes (the app.html wrapper, Svelte `style:`
				// directives), which nonces cannot; style *elements* stay
				// nonce-only. `script-src-attr` allows exactly one inline
				// handler: Svelte 5 SSR emits `onerror="this.__e=event"`
				// capture attributes for non-delegatable events (the artwork
				// `<img>` fallback in `src/lib/player/Artwork.svelte`), and
				// the browser blocks them without `unsafe-hashes` plus their
				// hash — the image error is then silently lost and the
				// fallback never runs. Recompute after a Svelte upgrade if
				// the artwork e2e tests fail:
				// echo -n 'this.__e=event' | openssl dgst -sha256 -binary | openssl base64 -A
				// `data:`/`blob:` on img-src match the artwork
				// sources `src/lib/player/artwork.ts` accepts.
				csp: {
					mode: 'nonce',
					directives: {
						'default-src': ['self'],
						'script-src': ['self'],
						'script-src-attr': [
							'unsafe-hashes',
							'sha256-7dQwUgLau1NFCCGjfn9FsYptB6ZtWxJin6VohGIu20I='
						],
						'style-src': ['self'],
						'style-src-attr': ['unsafe-inline'],
						'img-src': ['self', 'data:', 'blob:'],
						'media-src': apiSources,
						'connect-src': apiSources,
						'font-src': ['self'],
						'worker-src': ['self'],
						'manifest-src': ['self'],
						'object-src': ['none'],
						'base-uri': ['self'],
						'form-action': ['self'],
						'frame-ancestors': ['none']
						// No upgrade-insecure-requests: every source list is
						// 'self' or an explicit https: origin, which already
						// blocks http: subresources on an https page.
					}
				}
			})
		],
		test: {
			include: ['src/**/*.test.ts']
		}
	};
});
