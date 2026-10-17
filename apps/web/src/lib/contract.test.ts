import { describe, expect, it } from 'vitest';

// Single source of truth lives in the e2e harness: if the app drifts
// (renamed hook), this test names the missing contract key — the failure
// IS the signal, not noise. Test-only import; never bundled into the app.
import { SUPPORTED_NNTP_PORTS, TESTIDS } from '../../../../e2e/helpers/contract.js';
import { ALLOWED_NNTP_PORTS } from './vault-validation.js';

// Raw route markup via Vite glob: no node:fs, no @types/node needed.
const sources = import.meta.glob('../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const routeForTestId: Record<keyof typeof TESTIDS, string> = {
	appRoot: '../routes/+page.svelte',
	signupEmail: '../routes/signup/+page.svelte',
	signupSubmit: '../routes/signup/+page.svelte',
	signupError: '../routes/signup/+page.svelte',
	byopHost: '../routes/onboarding/byop/+page.svelte',
	byopPort: '../routes/onboarding/byop/+page.svelte',
	byopUser: '../routes/onboarding/byop/+page.svelte',
	byopPass: '../routes/onboarding/byop/+page.svelte',
	byopTls: '../routes/onboarding/byop/+page.svelte',
	byopTest: '../routes/onboarding/byop/+page.svelte',
	byopSave: '../routes/onboarding/byop/+page.svelte',
	byopStatus: '../routes/onboarding/byop/+page.svelte',
	importFile: '../routes/library/import/+page.svelte',
	importStatus: '../routes/library/import/+page.svelte',
	libraryItem: '../routes/library/+page.svelte',
	libraryList: '../routes/library/+page.svelte',
	libraryStaleBadge: '../routes/library/+page.svelte',
	audioPlayer: '../routes/library/+page.svelte',
	usageMeter: '../routes/library/+page.svelte',
	planLimitNotice: '../routes/library/+page.svelte'
};

describe('e2e NNTP port allowlist', () => {
	it('matches the app/API allowlist (no drift between the copies)', () => {
		expect([...SUPPORTED_NNTP_PORTS]).toEqual([...ALLOWED_NNTP_PORTS]);
	});
});

describe('e2e contract hooks', () => {
	for (const [key, testid] of Object.entries(TESTIDS) as [keyof typeof TESTIDS, string][]) {
		it(`declares data-testid="${testid}" in its route markup (${key})`, () => {
			const route = routeForTestId[key];
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
