import { describe, expect, it } from 'vitest';
import { BUILD_SHA } from './build';
import { APP_VERSION } from './version';

// The glob makes an absent new endpoint an acceptance assertion, not an
// import/setup failure. Existing identity endpoints execute without mocks.
type Handler = (event: unknown) => Response | Promise<Response>;
const routes = import.meta.glob('../routes/api/{release,build,health}/+server.ts', {
	eager: true
}) as Record<string, { GET: Handler }>;
const declarations = import.meta.glob('../app.d.ts', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const configSources = import.meta.glob('../../vite.config.ts', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const packages = import.meta.glob('../../package.json', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

function get(path: 'release' | 'build' | 'health'): Handler {
	const handler = routes[`../routes/api/${path}/+server.ts`]?.GET;
	expect(handler, `GET /api/${path} must expose its separate identity contract`).toBeTypeOf('function');
	return handler!;
}

function event(version: string) {
	return {
		request: new Request('https://release.example.test/api/release'),
		platform: {
			env: {
				DEPLOYED_RELEASE_VERSION: version,
				BUILD_SHA: 'f'.repeat(40),
				APP_VERSION: 'not-the-package-version'
			}
		}
	};
}

describe('runtime release metadata is not compiled build or package identity', () => {
	it('the same loaded handler reads canary then stable from each request platform binding', async () => {
		const handler = get('release');
		for (const version of ['2026.1028.0-canary.3', '2026.1028.0']) {
			const response = await handler(event(version));
			expect(response.status).toBe(200);
			expect(await response.json()).toEqual({ version });
			expect(response.headers.get('cache-control')).toContain('no-store');
		}
	});

	it.each([
		{},
		{ platform: undefined },
		{ platform: { env: {} } }
	])('absence of a runtime release binding is explicit null (%j)', async (input) => {
		const response = await get('release')(input);
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual({ version: null });
		expect(response.headers.get('cache-control')).toContain('no-store');
	});

	it('runtime release and fake runtime SHA cannot expand or override baked /api/build', async () => {
		const handler = get('build');
		for (const version of ['2026.1028.0-canary.3', '2026.1028.0']) {
			const response = await handler(event(version));
			expect(await response.json()).toEqual({ sha: BUILD_SHA });
			expect(response.headers.get('cache-control')).toContain('no-store');
		}
	});

	it('runtime release cannot expand /api/health or replace its internal package version', async () => {
		const handler = get('health');
		for (const version of ['2026.1028.0-canary.3', '2026.1028.0']) {
			const response = await handler(event(version));
			expect(await response.json()).toEqual({ ok: true, version: APP_VERSION });
			expect(response.headers.get('cache-control')).toContain('no-store');
		}
		const pkg = JSON.parse(packages['../../package.json']!) as { version: string };
		expect(APP_VERSION).toBe(pkg.version);
		expect(pkg.version, 'application release labeling must not reset package compatibility').not.toBe('0.0.0');
	});

	it('the platform declares the release binding explicitly instead of hiding it in an untyped env bag', () => {
		const source = (declarations['../app.d.ts'] ?? '')
			.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
		expect(source).toMatch(/interface\s+Platform\b/);
		expect(source).toMatch(/\benv\??\s*:/);
		expect(source).toMatch(/\bDEPLOYED_RELEASE_VERSION\??\s*:\s*string\b/);
	});

	it('release identity does not gain a Vite compile-time define alongside baked BUILD_SHA', () => {
		const source = configSources['../../vite.config.ts']!;
		expect(source).toContain('__BUILD_SHA__');
		expect(source).not.toMatch(/__\w*RELEASE\w*__|(?:VITE_|PUBLIC_)RELEASE_VERSION/);
		expect(source).not.toMatch(/["']?DEPLOYED_RELEASE_VERSION["']?\s*:/);
	});
});
