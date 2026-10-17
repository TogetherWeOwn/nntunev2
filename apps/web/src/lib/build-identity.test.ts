import { describe, expect, it } from 'vitest';

/**
 * Deployed-build identity acceptance contract (test-first).
 *
 * The Builder implements this contract on this branch without editing this
 * file. Dedicated surface (separate from `/api/health`):
 *
 *   GET /api/build -> 200 `{ sha: "<40 lowercase hex>" }` on staging builds,
 *   or `{ sha: "unknown" }` when no build SHA is available (local dev,
 *   preview without env, misconfigured CI). The `sha` key is always present,
 *   never null/empty/missing: the fallback is explicit, never silent.
 *
 * Lib surface (mirrors `src/lib/version.ts`):
 *
 *   src/lib/build.ts exports `BUILD_SHA: string` (40-hex or `"unknown"`),
 *   `parseBuildSha(value: unknown): string` (40-hex or `"unknown"`), and
 *   `isBuildShaKnown(sha?: string): boolean` (true iff 40-hex).
 *
 * `/api/health` semantics are unchanged: exactly `{ ok, version }` (pinned
 * by `tests/smoke.spec.ts` and `src/lib/version.test.ts`, untouched).
 */

// Raw sources via Vite glob: no node:fs, no @types/node needed (same pattern
// as src/lib/contract.test.ts). Missing files yield empty records, so the
// wiring suite fails with a builder message instead of a resolve error, and
// svelte-check stays green before the surface exists.
const libSources = import.meta.glob('./build.ts', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const routeSources = import.meta.glob('../routes/api/build/+server.ts', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const libModules = import.meta.glob('./build.ts', { eager: true }) as Record<
	string,
	{
		BUILD_SHA: string;
		parseBuildSha: (v: unknown) => string;
		isBuildShaKnown: (s?: string) => boolean;
	}
>;

// Local replica of the contract validators so the documented shape is pinned
// even before the Builder lands `src/lib/build.ts`. The wiring suite below
// asserts the real module matches this same behavior.
const SHA40 = /^[0-9a-f]{40}$/;
function contractParse(value: unknown): string {
	return typeof value === 'string' && SHA40.test(value) ? value : 'unknown';
}
function contractIsKnown(sha?: string): boolean {
	return typeof sha === 'string' && SHA40.test(sha);
}

describe('build-identity contract (documented shape)', () => {
	it('accepts a 40-char lowercase hex SHA', () => {
		expect(contractParse('a'.repeat(40))).toBe('a'.repeat(40));
		expect(contractIsKnown('0123456789abcdef0123456789abcdef01234567')).toBe(true);
	});

	it('falls back to explicit "unknown" and is never silent', () => {
		for (const missing of [undefined, null, '', '  ', 0, 42, {}, []] as unknown[]) {
			expect(contractParse(missing), JSON.stringify(missing)).toBe('unknown');
		}
		// Never null/empty/missing: the fallback is a non-empty string.
		expect(contractParse(undefined).length).toBeGreaterThan(0);
		expect(contractIsKnown('unknown')).toBe(false);
		expect(contractIsKnown(undefined)).toBe(false);
	});

	it('rejects non-canonical SHAs (uppercase, short, long, non-hex)', () => {
		for (const bad of [
			'A'.repeat(40),
			'a'.repeat(39),
			'a'.repeat(41),
			'g'.repeat(40),
			' a'.repeat(20),
			'a'.repeat(40) + '\n'
		]) {
			expect(contractParse(bad), bad).toBe('unknown');
			expect(contractIsKnown(bad)).toBe(false);
		}
	});

	it('exposes no secret, hostname, or binding material in the shape', () => {
		// The surface is exactly `{ sha }`: no room for secrets, hostnames,
		// tokens, or binding names. Any value that looks like one fails.
		const leaks = [
			'localhost',
			'myhost.example.com',
			'AKIAIOSFODNN7EXAMPLE',
			'xoxb-token-value',
			'-----BEGIN PRIVATE KEY-----'
		];
		for (const leak of leaks) {
			expect(contractParse(leak), leak).toBe('unknown');
		}
		// A 40-hex sha itself carries no hostname/secret structure.
		expect('a'.repeat(40)).not.toMatch(/localhost|secret|token|key|host/i);
	});
});

describe('build-identity wiring (Builder implements)', () => {
	it('ships src/lib/build.ts with BUILD_SHA, parseBuildSha, isBuildShaKnown', () => {
		const text = libSources['./build.ts'];
		expect(
			text,
			`missing src/lib/build.ts: Builder adds BUILD_SHA/parseBuildSha/isBuildShaKnown per the header contract`
		).toBeDefined();
		expect(text).toMatch(/export\s+(const|let)\s+BUILD_SHA/);
		expect(text).toMatch(/export\s+function\s+parseBuildSha/);
		expect(text).toMatch(/export\s+function\s+isBuildShaKnown/);
		// No secret/hostname/binding material baked into the lib surface.
		expect(text).not.toMatch(/SECRET|API_KEY|TOKEN|PASSWORD|PRIVATE_KEY/i);
		expect(text).not.toMatch(/os\.hostname|hostname\(\)|\.local\b/i);
	});

	it('ships GET /api/build serving exactly { sha }', () => {
		const text = routeSources['../routes/api/build/+server.ts'];
		expect(
			text,
			`missing src/routes/api/build/+server.ts: Builder adds GET returning exactly { sha }`
		).toBeDefined();
		expect(text).toMatch(/export\s+(const|async\s+function)\s+GET/);
		expect(text).toMatch(/BUILD_SHA|parseBuildSha|build/i);
		// Exactly { sha }: no version/ok/hostname/secret keys on this surface.
		expect(text).not.toMatch(/APP_VERSION|hostname|secret|token|binding/i);
	});

	it('real parseBuildSha matches the contract (40-hex or "unknown")', () => {
		const mod = libModules['./build.ts'];
		expect(
			mod,
			'src/lib/build.ts not implemented yet: Builder implements it per the header contract'
		).toBeDefined();
		expect(mod.parseBuildSha('b'.repeat(40))).toBe('b'.repeat(40));
		for (const missing of [undefined, null, '', 42] as unknown[]) {
			expect(mod.parseBuildSha(missing)).toBe('unknown');
		}
		expect(mod.isBuildShaKnown('b'.repeat(40))).toBe(true);
		expect(mod.isBuildShaKnown('unknown')).toBe(false);
		expect(mod.BUILD_SHA === 'unknown' || SHA40.test(mod.BUILD_SHA)).toBe(true);
		// Exact-head guard (review): when the test environment carries a
		// build SHA, the baked BUILD_SHA must equal it — otherwise a dropped
		// `__BUILD_SHA__` define would stay green while staging serves
		// "unknown". Same env chain as vite.config.ts; no node types.
		const testEnv = (globalThis as { process?: { env?: Record<string, string | undefined> } })
			.process?.env;
		const expectedSha = testEnv?.BUILD_SHA || testEnv?.GITHUB_SHA || undefined;
		if (expectedSha) {
			expect(mod.BUILD_SHA, `BUILD_SHA must equal the baked build SHA ${expectedSha}`).toBe(
				expectedSha
			);
		}
	});
});
