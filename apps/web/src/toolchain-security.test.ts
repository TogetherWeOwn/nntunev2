import { describe, expect, it } from 'vitest';
import lock from '../pnpm-lock.yaml?raw';
import policy from '../pnpm-workspace.yaml?raw';
import manifest from '../package.json';

describe('dependency install policy', () => {
	it('delays new resolutions by three days and refuses install scripts', () => {
		expect(policy).toMatch(/^minimumReleaseAge: 4320$/m);
		expect(policy).toMatch(/^ignoreScripts: true$/m);
	});

	it('explicitly generates SvelteKit files before commands that need them', () => {
		for (const command of ['dev', 'build', 'check', 'test'] as const) {
			expect(manifest.scripts[command]).toMatch(/^svelte-kit sync && /);
		}
	});
});

/**
 * Wrangler -> Miniflare pulls undici in transitively, so no manifest pins it.
 * Undici 7.x below 7.30.1 carries high-severity advisories (GHSA-xjj6-f33q-929c
 * and others); this fails if the lockfile ever resolves back to one of them.
 */
const PATCHED = [7, 30, 1];

describe('undici resolution', () => {
	const versions = [
		...lock.matchAll(/^ {2}'?undici@(\d+)\.(\d+)\.(\d+)/gm),
	].map((m) => m.slice(1, 4).map(Number));

	it('is present in the lockfile', () => {
		expect(versions.length).toBeGreaterThan(0);
	});

	it('resolves only patched 7.x versions', () => {
		for (const [major, minor, patch] of versions) {
			const version = `${major}.${minor}.${patch}`;
			expect(major, version).toBe(PATCHED[0]);
			expect(minor * 1e6 + patch, version).toBeGreaterThanOrEqual(
				PATCHED[1] * 1e6 + PATCHED[2],
			);
		}
	});
});
