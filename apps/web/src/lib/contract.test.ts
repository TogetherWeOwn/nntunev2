import { describe, expect, it } from 'vitest';

// Single source of truth lives in the e2e harness: if the app drifts
// (renamed hook), this test names the missing contract key — the failure
// IS the signal, not noise. Test-only import; never bundled into the app.
import { TESTIDS } from '../../../../e2e/helpers/contract.js';

// Raw route markup via Vite glob: no node:fs, no @types/node needed.
const sources = import.meta.glob('../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

describe('e2e contract hooks', () => {
	const haystack = Object.values(sources).join('\n');

	for (const [key, testid] of Object.entries(TESTIDS)) {
		it(`renders data-testid="${testid}" (${key})`, () => {
			expect(
				haystack,
				`contract key "${key}": no route renders data-testid="${testid}" — ` +
					'update app + e2e/helpers/contract.ts together (see e2e/README.md).'
			).toContain(`data-testid="${testid}"`);
		});
	}
});
