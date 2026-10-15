import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

import { GET } from './+server.js';

/**
 * Acceptance pin for [TOG-25176](/TOG/issues/TOG-25176) (roadmap R1):
 * `GET /api/health` must answer `{ok:true, version:<pkg>}` and must not
 * leak internal identifiers. Test files only — the Builder turns these
 * green on the build card and never edits this file.
 */

function packageVersion(): string {
	const url = new URL('../../../../package.json', import.meta.url);
	const pkg = JSON.parse(readFileSync(url, 'utf8')) as { version?: unknown };
	expect(typeof pkg.version, 'apps/web/package.json must carry a version string').toBe('string');
	return pkg.version as string;
}

describe('GET /api/health contract (TOG-25176 R1)', () => {
	it('returns 200 with ok:true', async () => {
		const res = await GET({} as never);

		expect(res.status).toBe(200);
		const body = (await res.json()) as Record<string, unknown>;
		expect(body.ok).toBe(true);
	});

	it('pins a non-empty version field equal to the web package version', async () => {
		const res = await GET({} as never);
		const body = (await res.json()) as Record<string, unknown>;

		expect(
			typeof body.version === 'string' && body.version.length > 0,
			`health body missing non-empty version field: ${JSON.stringify(body)}`
		).toBe(true);
		expect(body.version).toBe(packageVersion());
	});

	it('exposes only the public contract keys (no internal identifiers)', async () => {
		const res = await GET({} as never);
		const body = (await res.json()) as Record<string, unknown>;

		const allowed = new Set(['ok', 'version']);
		for (const key of Object.keys(body)) {
			expect(allowed.has(key), `health leaks unexpected key "${key}": ${JSON.stringify(body)}`).toBe(
				true
			);
		}
	});
});
