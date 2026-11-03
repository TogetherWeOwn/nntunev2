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

// Only the retired import hooks are excluded; every other shared hook
// must still have a route pin while the end-to-end contract is migrated.
type ActiveTestId = Exclude<keyof typeof TESTIDS, 'importFile' | 'importStatus'>;
const routeForTestId: Record<ActiveTestId, string> = {
	appRoot: '../routes/+page.svelte',
	signupEmail: '../routes/signup/+page.svelte',
	signupSubmit: '../routes/signup/+page.svelte',
	signupError: '../routes/signup/+page.svelte',
	byopHost: '../routes/onboarding/byop/+page.svelte',
	byopPort: '../routes/onboarding/byop/+page.svelte',
	byopUser: '../routes/onboarding/byop/+page.svelte',
	byopPass: '../routes/onboarding/byop/+page.svelte',
	byopTls: '../routes/onboarding/byop/+page.svelte',
	byopConnections: '../routes/onboarding/byop/+page.svelte',
	byopTest: '../routes/onboarding/byop/+page.svelte',
	byopSave: '../routes/onboarding/byop/+page.svelte',
	byopStatus: '../routes/onboarding/byop/+page.svelte',
	libraryItem: '../routes/library/+page.svelte',
	libraryList: '../routes/library/+page.svelte',
	libraryStaleBadge: '../routes/library/+page.svelte',
	audioPlayer: '../routes/library/+page.svelte',
	usageMeter: '../routes/library/+page.svelte',
	// Today the demo player carries these; the player-bar build (design
	// spec v1) moves them to the persistent player bar and repoints this
	// map in the same PR.
	nowPlaying: '../routes/demo/player/+page.svelte',
	playerNext: '../routes/demo/player/+page.svelte',
	queuePanel: '../routes/demo/player/+page.svelte',
	planLimitNotice: '../routes/library/+page.svelte'
};

describe('retired user NZB import', () => {
	it('has no route, navigation link, or import control in any route source', () => {
		expect(sources['../routes/library/import/+page.svelte']).toBeUndefined();
		for (const source of Object.values(sources)) {
			expect(source).not.toContain('/library/import');
			expect(source).not.toMatch(/data-testid=["']import-(?:file|status)["']/);
			expect(source).not.toMatch(/accept=["'][^"']*\.nzb/i);
		}
	});
});

describe('e2e NNTP port allowlist', () => {
	it('matches the app/API allowlist (no drift between the copies)', () => {
		expect([...SUPPORTED_NNTP_PORTS]).toEqual([...ALLOWED_NNTP_PORTS]);
	});
});

describe('e2e contract hooks', () => {
	for (const [key, route] of Object.entries(routeForTestId) as [ActiveTestId, string][]) {
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
