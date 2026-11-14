import { describe, expect, it } from 'vitest';

// Single source of truth lives in the e2e harness: if the app drifts
// (renamed hook), this test names the missing contract key — the failure
// IS the signal, not noise. Test-only import; never bundled into the app.
import { TESTIDS } from '../../../../e2e/helpers/contract.js';

// Raw route markup via Vite glob: no node:fs, no @types/node needed.
const sources = import.meta.glob(['../routes/**/*.svelte', './interactions/*.svelte', './player/*.svelte'], {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

// Legacy API fixtures retain the retired hooks; active web hooks still
// require a markup pin while the end-to-end contract is migrated.
type ActiveTestId = Exclude<keyof typeof TESTIDS, 'importFile' | 'importStatus' | `byop${string}`>;
const routeForTestId: Record<ActiveTestId, string> = {
	appRoot: '../routes/+page.svelte',
	signupEmail: '../routes/signup/+page.svelte',
	signupSubmit: '../routes/signup/+page.svelte',
	signupError: '../routes/signup/+page.svelte',
	libraryItem: '../routes/library/+page.svelte',
	libraryList: '../routes/library/+page.svelte',
	libraryStaleBadge: '../routes/library/+page.svelte',
	trackContextMenu: './interactions/ContextMenu.svelte',
	trackMore: '../routes/library/+page.svelte',
	favoriteMore: '../routes/library/+page.svelte',
	trackSelectionToolbar: '../routes/library/+page.svelte',
	queueDropTarget: './player/PlayerQueue.svelte',
	queueMore: './player/PlayerQueue.svelte',
	queueSelectionToolbar: './player/PlayerQueue.svelte',
	audioPlayer: '../routes/library/+page.svelte',
	usageMeter: '../routes/library/+page.svelte',
	// EL TWO-5795 (demo/player retirement): the retired route no longer owns
	// these hooks. now-playing/player-next live in the product PlayerQueue;
	// the player-bar build (design spec v1) moves them to the persistent
	// player bar and repoints this map in the same PR. queuePanel stays
	// pinned below and skips until its QueueView owner lands (slice 4).
	nowPlaying: './player/PlayerQueue.svelte',
	playerNext: './player/PlayerQueue.svelte',
	queuePanel: '../routes/demo/player/+page.svelte',
	planLimitNotice: '../routes/library/+page.svelte',
	// The W10 app shell declares all four in the root layout: the top bar
	// renders in both shells (app + public header), the rail/tab bar render
	// one per breakpoint, and the mini-player renders while a selection
	// hint is set.
	shellTopbar: '../routes/+layout.svelte',
	shellRail: '../routes/+layout.svelte',
	shellTabbar: '../routes/+layout.svelte',
	miniPlayer: '../routes/+layout.svelte'
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

describe('retired provider credential routes', () => {
	it('has no provider form, route, navigation link, or vault call in active web source', () => {
		const webSources = import.meta.glob(['../routes/**/*.{svelte,ts}', './api/client.ts'], {
			query: '?raw', import: 'default', eager: true
		}) as Record<string, string>;
		for (const [path, source] of Object.entries(webSources)) {
			if (path.endsWith('.test.ts')) continue;
			expect(path).not.toMatch(/\/onboarding\/(?:byop|provider)\//);
			expect(path).not.toContain('/account/provider/');
			expect(source).not.toMatch(/\/(?:onboarding\/(?:byop|provider)|account\/provider)\b/);
			expect(source).not.toContain('/v1/vault');
			expect(source).not.toMatch(/data-testid=["']byop-/);
		}
	});
});

describe('e2e contract hooks', () => {
	for (const [key, route] of Object.entries(routeForTestId) as [ActiveTestId, string][]) {
		const testid = TESTIDS[key];
		// EL TWO-5795: queuePanel's owner (QueueView) lands with the persistent
	// queue panel; un-skip then (slice 4 repoints the entry above).
	const run = key === 'queuePanel' ? it.skip : it;
	run(`declares data-testid="${testid}" in its route markup (${key})`, () => {
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
