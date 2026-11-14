import { describe, expect, it } from 'vitest';

// Single source of truth lives in the e2e harness: if the app drifts
// (renamed hook), this test names the missing contract key — the failure
// IS the signal, not noise. Test-only import; never bundled into the app.
import { TESTIDS } from '../../../../e2e/helpers/contract.js';

// Acceptance-owner protection remains in force. Engineering Lead authorized
// a one-time same-PR exception for player/queue physical owner mappings and
// connection assertions only, including the six incoming intent connections.
// Hook values and behavior vectors stay unchanged; independent equivalence
// review against both baselines is required.
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
	audioPlayer: './player/PlayerAudio.svelte',
	playerBar: './player/PlayerTransport.svelte',
	playerPlayPause: './player/PlayerTransport.svelte',
	playerSeek: './player/PlayerTransport.svelte',
	playerVolume: './player/PlayerTransport.svelte',
	trackContextMenu: './interactions/ContextMenu.svelte',
	trackMore: '../routes/library/+page.svelte',
	favoriteMore: '../routes/library/+page.svelte',
	trackSelectionToolbar: '../routes/library/+page.svelte',
	queueDropTarget: './player/PlayerQueue.svelte',
	queueMore: './player/PlayerQueue.svelte',
	queueSelectionToolbar: './player/PlayerQueue.svelte',
	usageMeter: '../routes/library/+page.svelte',
	// Playable selections use the layout-owned transport; the library queue
	// retains its empty/unavailable fallback without duplicating either hook.
	nowPlaying: './player/PlayerTransport.svelte',
	playerNext: './player/PlayerTransport.svelte',
	queuePanel: './player/QueueView.svelte',
	planLimitNotice: '../routes/library/+page.svelte',
	// The W10 app shell declares the top bar, rail and tab bar in the root
	// layout: the top bar renders in both shells (app + public header) and
	// the rail/tab bar render one per breakpoint. The compact transport owns
	// the mini-player hook (C3): one play/favorite surface at every width.
	shellTopbar: '../routes/+layout.svelte',
	shellRail: '../routes/+layout.svelte',
	shellTabbar: '../routes/+layout.svelte',
	miniPlayer: './player/PlayerTransport.svelte'
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
	it('mounts the production player owners from the persistent layout', () => {
		const layout = sources['../routes/+layout.svelte'];
		expect(layout).toContain("import PlayerAudio from '#lib/player/PlayerAudio.svelte'");
		expect(layout).toContain('const player = createPlayerSession({ flush: tick })');
		expect(layout).toContain('providePlayerSession(player)');
		// Authorized same-PR owner-wiring pin: presentation props keep this decoder mount.
		expect(layout).toContain('<PlayerAudio {queueToggle} expanded={nowPlayingRoute} viewHeader={nowPlayingHeader} onNowPlayingOpen={noteNowPlayingOpen} />');
		const audio = sources['./player/PlayerAudio.svelte'];
		expect(audio).toContain('const player = usePlayerSession()');
		expect(audio).toContain('player.attach(audio)');
		expect(layout).toContain("import QueueView from '#lib/player/QueueView.svelte'");
		expect(layout).toContain('<QueueView />');
		expect(layout).toContain('{@render queueContent()}');
		expect(layout).toContain('{#if queueRoute}');
		expect(layout).toContain('{#if queueOpen}');
		expect(sources['../routes/library/+page.svelte']).not.toContain('<QueueView');
		const view = sources['./player/QueueView.svelte'];
		expect(view).toContain('const player = usePlayerSession()');
		expect(view).toContain("import PlayerQueue from './PlayerQueue.svelte'");
		expect(view).toContain('const queue = $derived($player.queue)');
		expect(view).toMatch(/<PlayerQueue\s+state=\{queue\}/);
		expect(view).toContain('const actions = queueViewActions(player)');
		for (const action of ['next', 'shuffle', 'move', 'playnext', 'remove', 'clear', 'select', 'copylink', 'keepcurrentandupcoming', 'clearupcoming']) {
			expect(view).toContain(`on${action}={actions.on${action}}`);
		}
		for (const action of ['insert', 'reorder', 'playnextids', 'removeids', 'playnow', 'favoriteids']) {
			expect(view).toContain(`on${action}={actions.on${action}}`);
		}
		expect(view).toContain('<QueueExport />');
	});

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
