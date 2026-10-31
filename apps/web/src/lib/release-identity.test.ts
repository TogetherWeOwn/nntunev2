import { afterEach, describe, expect, it, vi } from 'vitest';
import { BUILD_SHA } from './build';
import { APP_VERSION } from './version';

// Mock only Kit's per-request private-env boundary. Endpoints and their JSON
// responses remain real; no platform stub may conceal a broken Worker fallback.
const privateBinding = vi.hoisted(() => ({ version: undefined as string | undefined }));
vi.mock('$app/env/private', () => ({
	get DEPLOYED_RELEASE_VERSION() { return privateBinding.version; }
}));
afterEach(() => { privateBinding.version = undefined; });

// The glob makes an absent new endpoint an acceptance assertion, not an
// import/setup failure. Existing identity endpoints execute without endpoint mocks.
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

type VariableDeclaration = {
	public?: boolean;
	static?: boolean;
	schema?: {
		'~standard': {
			validate: (value: unknown) =>
				| { value?: unknown; issues?: readonly unknown[] }
				| Promise<{ value?: unknown; issues?: readonly unknown[] }>;
		};
	};
};
const envModules = import.meta.glob('../env.ts', { eager: true }) as Record<
	string, { variables: Record<string, VariableDeclaration> }
>;

// Execute the actual defineEnvVars declaration and its schema. This is a
// compiler-policy boundary, not a grep for spellings or a replica of the schema.
async function assertPrivateRuntimeRelease(variables: Record<string, VariableDeclaration>) {
	const declaration = variables['DEPLOYED_RELEASE_VERSION'];
	expect(declaration, 'the release binding must be registered for Kit compilation').toBeDefined();
	expect(declaration!.public, 'release metadata is server-only').toBe(false);
	expect(declaration!.static, 'canary/stable cannot be baked into executable bytes').toBe(false);
	// defineEnvVars exposes Standard Schema after normalizing its callback.
	const schema = declaration!.schema?.['~standard'];
	expect(schema?.validate).toBeTypeOf('function');
	for (const value of [undefined, '2026.1028.0-canary.3', '2026.1028.0']) {
		const result = await schema!.validate(value);
		expect(result.issues, 'the binding is optional, not a required deploy secret').toBeUndefined();
		expect(result.value).toBe(value);
	}
}

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
	])('absent-platform requests read the private runtime fallback without a rebuild (%j)', async (input) => {
		const handler = get('release');
		for (const version of ['2026.1028.0-canary.3', '2026.1028.0', undefined]) {
			privateBinding.version = version;
			const response = await handler(input);
			expect(response.status).toBe(200);
			expect(await response.json()).toEqual({ version: version ?? null });
			expect(response.headers.get('cache-control')).toContain('no-store');
			// Runtime fallback must not change either independent identity.
			expect(await (await get('build')(input)).json()).toEqual({ sha: BUILD_SHA });
			expect(await (await get('health')(input)).json()).toEqual({ ok: true, version: APP_VERSION });
		}
	});

	it('an explicit platform binding wins over a different private fallback', async () => {
		privateBinding.version = '2026.1028.0';
		expect(await (await get('release')(event('2026.1028.0-canary.3'))).json())
			.toEqual({ version: '2026.1028.0-canary.3' });
	});

	it('Kit registers the binding as optional private and nonstatic', async () => {
		await assertPrivateRuntimeRelease(envModules['../env.ts']!.variables);
	});

	it('removal, public, static and required-schema declaration mutants are rejected', async () => {
		const variables = envModules['../env.ts']!.variables;
		await assertPrivateRuntimeRelease(variables);
		const declaration = variables['DEPLOYED_RELEASE_VERSION']!;
		const missing = { ...variables };
		delete missing['DEPLOYED_RELEASE_VERSION'];
		for (const mutant of [
			missing,
			{ ...variables, DEPLOYED_RELEASE_VERSION: { ...declaration, public: true } },
			{ ...variables, DEPLOYED_RELEASE_VERSION: { ...declaration, static: true } },
			{ ...variables, DEPLOYED_RELEASE_VERSION: {
				...declaration, schema: { '~standard': { validate: () => ({ value: 'baked fallback' }) } }
			} }
		]) {
			await expect(assertPrivateRuntimeRelease(mutant)).rejects.toThrow();
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
