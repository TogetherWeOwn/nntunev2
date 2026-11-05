import { describe, expect, it } from 'vitest';

// Single source of truth lives in the e2e harness: if the app drifts
// (renamed hook), this test names the missing contract key — the failure
// IS the signal, not noise. Test-only import; never bundled into the app.
import { SUPPORTED_NNTP_PORTS, TESTIDS } from '../../../../e2e/helpers/contract.js';
import { ALLOWED_NNTP_PORTS } from './vault-validation.js';

// Acceptance-owner protection remains in force. Engineering Lead authorized
// a one-time same-PR exception for player/queue physical owner mappings and
// connection assertions only. Hook values and behavior vectors stay unchanged;
// independent equivalence review is required.
const sources = import.meta.glob(['../routes/**/*.svelte', './player/*.svelte'], {
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
	byopConnections: '../routes/onboarding/byop/+page.svelte',
	byopTest: '../routes/onboarding/byop/+page.svelte',
	byopSave: '../routes/onboarding/byop/+page.svelte',
	byopStatus: '../routes/onboarding/byop/+page.svelte',
	importFile: '../routes/library/import/+page.svelte',
	importStatus: '../routes/library/import/+page.svelte',
	libraryItem: '../routes/library/+page.svelte',
	libraryList: '../routes/library/+page.svelte',
	libraryStaleBadge: '../routes/library/+page.svelte',
	audioPlayer: './player/PlayerAudio.svelte',
	playerBar: './player/PlayerTransport.svelte',
	playerPlayPause: './player/PlayerTransport.svelte',
	playerSeek: './player/PlayerTransport.svelte',
	playerVolume: './player/PlayerTransport.svelte',
	usageMeter: '../routes/library/+page.svelte',
	// Playable selections use the layout-owned transport; the library queue
	// retains its empty/unavailable fallback without duplicating either hook.
	nowPlaying: './player/PlayerTransport.svelte',
	playerNext: './player/PlayerTransport.svelte',
	queuePanel: './player/QueueView.svelte',
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

describe('e2e NNTP port allowlist', () => {
	it('matches the app/API allowlist (no drift between the copies)', () => {
		expect([...SUPPORTED_NNTP_PORTS]).toEqual([...ALLOWED_NNTP_PORTS]);
	});
});

describe('e2e contract hooks', () => {
	it('mounts the production player owners from the persistent layout', () => {
		const layout = sources['../routes/+layout.svelte'];
		expect(layout).toContain("import PlayerAudio from '#lib/player/PlayerAudio.svelte'");
		expect(layout).toContain('const player = createPlayerSession({ flush: tick })');
		expect(layout).toContain('providePlayerSession(player)');
		// Authorized same-PR owner-wiring pin: presentation props keep this decoder mount.
		expect(layout).toContain('<PlayerAudio {queueToggle} expanded={nowPlayingRoute} viewHeader={nowPlayingHeader} />');
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
		expect(view).toMatch(/<PlayerQueue\s+state=\{\$player\.queue\}/);
		expect(view).toContain('const actions = queueViewActions(player)');
		for (const action of ['next', 'shuffle', 'move', 'playnext', 'remove', 'clear', 'select', 'copylink', 'keepcurrentandupcoming', 'clearupcoming']) {
			expect(view).toContain(`on${action}={actions.on${action}}`);
		}
		expect(view).toContain('<QueueExport />');
	});

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
