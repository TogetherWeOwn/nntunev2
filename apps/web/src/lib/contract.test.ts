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

// Retired user-import hooks (importFile/importStatus, TWO-1639) stay in the
// e2e contract until the parked e2e upload legs are removed by their own
// card, so this map is intentionally partial: it pins only the hooks the
// web app still renders, and the loop below iterates the map (not TESTIDS)
// so retired e2e-only keys never fail the web suite.
const routeForTestId: Partial<Record<keyof typeof TESTIDS, string>> = {
	appRoot: '../routes/+page.svelte',
	signupEmail: '../routes/signup/+page.svelte',
	signupSubmit: '../routes/signup/+page.svelte',
	signupError: '../routes/signup/+page.svelte',
	libraryItem: '../routes/library/+page.svelte',
	libraryList: '../routes/library/+page.svelte',
	libraryStaleBadge: '../routes/library/+page.svelte',
	audioPlayer: '../routes/library/+page.svelte',
	usageMeter: '../routes/library/+page.svelte',
	planLimitNotice: '../routes/library/+page.svelte'
};

describe('e2e contract hooks', () => {
	for (const [key, route] of Object.entries(routeForTestId) as [keyof typeof TESTIDS, string][]) {
		const testid = TESTIDS[key];
		it(`declares data-testid="${testid}" in its route markup (${key})`, () => {
			const source = sources[route];
			expect(source, `contract key "${key}": expected route source "${route}"`).toBeDefined();
			expect(
				source,
				`contract key "${key}": expected route source "${route}" to declare data-testid="${testid}" — ` +
					'update app + e2e/helpers/contract.ts together (see e2e/README.md).'
			).toContain(`data-testid="${testid}"`);
		});
	}
});
