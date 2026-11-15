import { afterEach, expect, test, vi } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { createTrackListExport } from '#lib/player/trackListExport.js';
import { createLibraryRefresher } from '#lib/library/refresh.js';
import { loadFavorites, saveFavorites, clearFavoritesAtAuthBoundary, selectFavoriteItems } from '#lib/library/favorites.js';
import {
	clearRecentFindsAtAuthBoundary,
	loadRecentFinds,
	recordRecentFind,
	saveRecentFinds
} from '#lib/library/recent-finds.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';
import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
import { displayArtist, displayTitle, orderLibraryItems } from '#lib/library/order.js';
import { clearMiniPlayerHint, setMiniPlayerHint } from '#lib/shell/mini-player.js';
import { filterLibraryItems } from '#lib/library/filter.js';
import { createPlayerFacade, liftPlayerProjections } from './player-harness.js';
import { libraryTabTitle } from '#lib/library/tab-title.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { queueViewActions } from '#lib/player/queueViewActions.js';
import { endTrackDrag } from '#lib/interactions/track-drag.js';
import {
	appendResultLabel,
	appendSelection,
	clearSelection,
	createSelection,
	hiddenSelectedCount,
	pruneSelection,
	selectionCountLabel,
	toggleSelected
} from '#lib/library/selection.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const ITEMS = [
	{ id: 'row-mike', title: 'Mike Song' },
	{ id: 'row-zulu', title: 'Zulu Song' },
	{ id: 'row-alpha', title: 'Alpha Song' }
];

/**
 * Run the real page script with plain state cells (no DOM). Only browser
 * lifecycle/storage and unused presentation bindings are substituted; queue
 * operations and the refresher, including onStateChange, remain real.
 *
 * The browser suite covers rendered live regions and event delivery. These
 * tests isolate each auth clear from the reconciliation backstop: an empty
 * queue can still have a previous move announcement, and reconciliation has
 * no tracks to drop. Reverting just that handler to `queue = clearQueue(queue)`
 * must leave stale text here, even though the ordinary browser case passes.
 */
/**
 * Body of the first `<script>` block. Plain string search (no regexp) so
 * HTML tag-filter checks do not apply; the end tag may carry whitespace
 * (`</script >`).
 */
function extractFirstScript(source: string): string | undefined {
	const lower = source.toLowerCase();
	const openStart = lower.indexOf('<script');
	if (openStart === -1) return undefined;
	const openEnd = source.indexOf('>', openStart + '<script'.length);
	if (openEnd === -1) return undefined;
	let closeStart = lower.indexOf('</script', openEnd + 1);
	while (closeStart !== -1) {
		const closeEnd = source.indexOf('>', closeStart + '</script'.length);
		if (closeEnd === -1) return undefined;
		if (source.slice(closeStart + '</script'.length, closeEnd).trim() === '') {
			return source.slice(openEnd + 1, closeStart);
		}
		closeStart = lower.indexOf('</script', closeStart + 1);
	}
	return undefined;
}

function pageScript() {
	const script = extractFirstScript(sources['./+page.svelte']);
	if (!script) throw new Error('Library page script missing');
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	// The route reads queue/repeat/audio from the layout-owned player session.
	const facade = createPlayerFacade();
	// Synchronous membership cases start after the authenticated-load bootstrap.
	facade.initializeFavorites();
	const lifted = liftPlayerProjections(
		parsed, parsed.statements.filter((statement) => !ts.isImportDeclaration(statement))
	);
	const compiled = ts.transpileModule(`let $player = __facade.get();\n${lifted.body}\n${lifted.sync}`, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	let mount: (() => void | (() => void)) | undefined;
	let teardown: (() => void) | undefined;
	const bindings = {
		createTrackListExport,
		formatQueueText: queueFunctions.formatQueueText,
		formatQueueM3U: queueFunctions.formatQueueM3U,
		addTrack: queueFunctions.addTrack,
		clearQueue: queueFunctions.clearQueue,
		createQueue: queueFunctions.createQueue,
		moveTrack: queueFunctions.moveTrack,
		nextTrack: queueFunctions.nextTrack,
		playNext: queueFunctions.playNext,
		removeTrack: queueFunctions.removeTrack,
		queueCurrentTrack: queueFunctions.currentTrack,
		selectQueueIndex: queueFunctions.selectTrack,
		shuffleUpcoming: queueFunctions.shuffleUpcoming,
		loadFavorites,
		saveFavorites,
		clearFavoritesAtAuthBoundary,
		clearRecentFindsAtAuthBoundary,
		loadRecentFinds,
		recordRecentFind,
		saveRecentFinds,
		selectFavoriteItems,
		createLibraryRefresher,
		displayArtist,
		displayTitle,
		clearMiniPlayerHint,
		setMiniPlayerHint,
		orderLibraryItems,
		filterLibraryItems,
		libraryTabTitle,
		endTrackDrag,
		appendResultLabel,
		appendSelection,
		clearSelection,
		createSelection,
		hiddenSelectedCount,
		pruneSelection,
		selectionCountLabel,
		toggleSelected,
		LIBRARY_AUTH_TRANSITION_KEY,
		subscribeLocalAuthTransition,
		usePlayerSession: () => facade,
		// Host-owner adapter only: run the mounted QueueView's real move action.
		moveQueuedTrack: queueViewActions(facade).onmove,
		__facade: facade,
		$effect: () => {},
		browserStorage: () => null,
		clearSnapshot: () => {},
		invalidateLibraryCache: () => {},
		clearSignedInEmail: () => {},
		resolveStreamUrl,
		onMount: (callback: () => void | (() => void)) => { mount = callback; },
		afterNavigate: () => {},
		tick: () => Promise.resolve(),
		$state: <T>(value: T) => value,
		$derived: <T>(value: T) => value
	};
	const page = new Function(...Object.keys(bindings), `${compiled}
		return {
			refresh, selectTrack, moveQueuedTrack, toggleFavorite,
			handleAuthTransition, handleLocalAuthTransition, unstarFromShelf, confirmClearFavorites,
			read: () => ({ queue, announcement: player.get().queueFeedback, items, loadError }),
			readFavorites: () => ({ favorites, announcement: favoritesAnnouncement }),
			// Fixture setup only: do not call a non-move page edit, which would
			// clear the announcement before the handler under test ever runs.
			emptyQueueWithoutEdit: () => { const text = player.get().queueFeedback; player.setQueue(clearQueue(queue)); player.setQueueFeedback(text); },
			setFavoriteAnnouncementForFixture: (announcement) => {
				favoritesAnnouncement = announcement;
			}
		};
	`)(...Object.values(bindings)) as {
		refresh: () => Promise<void>;
		selectTrack: (id: string) => void;
		moveQueuedTrack: (from: number, to: number) => void;
		toggleFavorite: (id: string) => void;
		unstarFromShelf: (id: string) => void;
		confirmClearFavorites: () => void;
		handleAuthTransition: (event: Pick<StorageEvent, 'key'>) => void;
		handleLocalAuthTransition: () => void;
		emptyQueueWithoutEdit: () => void;
		setFavoriteAnnouncementForFixture: (announcement: string) => void;
		read: () => {
			queue: queueFunctions.QueueState;
			announcement: string;
			items: { id: string; title?: string }[];
			loadError: string | null;
		};
		readFavorites: () => {
			favorites: string[];
			announcement: string;
		};
	};
	return {
		...page, player: facade,
		mountForFixture: () => {
			const cleanup = mount?.();
			teardown = typeof cleanup === 'function' ? cleanup : undefined;
		},
		disposeForFixture: () => { teardown?.(); teardown = undefined; }
	};
}

afterEach(() => vi.unstubAllGlobals());

function mountedWindow() {
	vi.stubGlobal('window', { location: { search: '' },
		localStorage: { getItem: () => null }, addEventListener: vi.fn(), removeEventListener: vi.fn() });
}

// These exercise the real route producer/refresher and lifecycle with DOM/audio
// doubles only. Actual menu gestures and native decoding have separate browsers.
test('the mounted authenticated producer copies private queued evidence and teardown retires insertion, not Play', async () => {
	mountedWindow();
	vi.stubGlobal('fetch', async () => Response.json([{ id: 'dawn', title: 'Dawn Chorus', streamUrl: '/accepted' }]));
	const page = pageScript();
	page.mountForFixture();
	await vi.waitFor(() => expect(page.read().items).toHaveLength(1));
	const actions = queueViewActions(page.player);
	actions.oninsert(['dawn'], null);
	expect(page.read().queue.tracks).toEqual([{ id: 'dawn', title: 'Dawn Chorus', artist: '', streamUrl: '' }]);
	expect(page.player.snapshot().queueFeedback).toBe('Added 1 to the queue.');
	page.disposeForFixture();
	page.player.editQueue({ type: 'clear' });
	actions.oninsert(['dawn'], null);
	expect(page.read().queue.tracks).toEqual([]);
	// A new mounted route is required for new ingress, not an old retained closure.
	page.player.destroy();
});

test('accepted source input survives actual route cleanup and a late disposed refresh cannot replace it or suspend the queue', async () => {
	mountedWindow();
	let release!: (response: Response) => void;
	let held = false;
	vi.stubGlobal('fetch', () => held ? new Promise<Response>((resolve) => { release = resolve; })
		: Promise.resolve(Response.json([{ id: 'dawn', title: 'Dawn Chorus', streamUrl: '/accepted' }])));
	const page = pageScript();
	page.mountForFixture();
	await vi.waitFor(() => expect(page.read().items).toHaveLength(1));
	queueViewActions(page.player).oninsert(['dawn'], null);
	held = true;
	const refreshing = page.refresh();
	expect(page.player.snapshot().queueLiveRegions).toBe(false);
	const before = page.player.snapshot().queue;
	queueViewActions(page.player).oninsert(['dawn'], null);
	expect(page.player.snapshot().queue).toBe(before);
	page.disposeForFixture();
	expect(page.player.snapshot().queueLiveRegions).toBe(true);
	release(Response.json([{ id: 'dawn', title: 'Dawn Chorus', streamUrl: '/disposed' }]));
	await refreshing;
	expect(page.player.snapshot().queueLiveRegions).toBe(true);
	expect(page.read().queue).toBe(before);
	await page.player.playQueuedTrack('dawn');
	expect(page.read().queue.tracks[0].streamUrl).toBe('/accepted');
	expect(page.player.snapshot().queueFeedback).toBe('Selected Dawn Chorus, paused: playback was blocked.');
	page.player.destroy();
});

test.each(['empty', 'changed', 'removed', 'transient', '401', '403', '404'] as const)(
	'the real authenticated refresh %s has the exact queued-input lifetime', async (change) => {
		mountedWindow();
		let next: typeof change | 'initial' = 'initial';
		vi.stubGlobal('fetch', async () => {
			if (next === 'transient') throw new TypeError('network unavailable');
			if (next === '401' || next === '403' || next === '404') return new Response(null, { status: Number(next) });
			return Response.json(next === 'removed' ? [] : [{ id: 'dawn', title: 'Dawn Chorus',
				streamUrl: next === 'empty' ? '' : next === 'changed' ? '/replacement' : '/accepted' }]);
		});
		const page = pageScript();
		page.mountForFixture();
		await vi.waitFor(() => expect(page.read().items).toHaveLength(1));
		queueViewActions(page.player).oninsert(['dawn'], null);
		expect(page.read().queue.tracks[0].streamUrl).toBe('');
		next = change;
		await page.refresh();
		if (change === 'removed' || change === '401' || change === '403' || change === '404') {
			expect(page.read().queue.tracks).toEqual([]);
			await page.player.playQueuedTrack('dawn');
			expect(page.player.snapshot().queueFeedback).toBe('');
		} else {
			page.disposeForFixture();
			await page.player.playQueuedTrack('dawn');
			expect(page.read().queue.tracks[0].streamUrl).toBe(change === 'empty' ? '' : change === 'changed' ? '/replacement' : '/accepted');
			expect(page.player.snapshot().queueFeedback).toBe(change === 'empty'
				? 'Selected Dawn Chorus; no stream to play.' : 'Selected Dawn Chorus, paused: playback was blocked.');
		}
		page.disposeForFixture();
		page.player.destroy();
	});

for (const boundary of ['storage', 'local'] as const) {
	test(`${boundary} auth clears an announcement even when the queue is already empty`, async () => {
		let payload = ITEMS;
		vi.stubGlobal('fetch', async () => new Response(JSON.stringify(payload), {
			headers: { 'content-type': 'application/json' }
		}));
		const page = pageScript();
		await page.refresh();
		for (const item of ITEMS) page.selectTrack(item.id);
		page.moveQueuedTrack(2, 1);
		expect(page.read().queue.tracks.map((track) => track.id)).toEqual([
			'row-mike', 'row-alpha', 'row-zulu'
		]);
		expect(page.read().announcement).toContain('Alpha Song');
		page.emptyQueueWithoutEdit();
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.read().announcement).toContain('Alpha Song');

		if (boundary === 'storage') {
			page.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });
		} else {
			page.handleLocalAuthTransition();
		}

		expect(page.read().items).toEqual([]);
		expect(page.read().loadError).toMatch(/session changed/i);
		expect(page.read().announcement).toBe('');
		payload = [{ id: 'row-bravo', title: 'Bravo Song' }];
		await page.refresh();
		expect(page.read().items).toEqual([{ id: 'row-bravo', title: 'Bravo Song' }]);
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.read().announcement).toBe('');
	});
}

test('favorite membership edits clear a prior arrangement announcement', () => {
	const page = pageScript();
	page.setFavoriteAnnouncementForFixture('Mike Song moved to 2 of 3');

	page.toggleFavorite('row-mike');

	expect(page.readFavorites()).toEqual({ favorites: ['row-mike'], announcement: '' });

	page.setFavoriteAnnouncementForFixture('Mike Song moved to 1 of 1');
	page.toggleFavorite('row-mike');

	expect(page.readFavorites()).toEqual({ favorites: [], announcement: '' });
});

for (const boundary of ['storage', 'local', '401', '403'] as const) {
	test(`${boundary} boundary clears a previous account's favorite arrangement announcement`, async () => {
		let status = 200;
		vi.stubGlobal('fetch', async () => new Response(JSON.stringify(ITEMS), {
			status,
			headers: { 'content-type': 'application/json' }
		}));
		const page = pageScript();
		await page.refresh();
		page.toggleFavorite('row-mike');
		page.setFavoriteAnnouncementForFixture('Mike Song moved to 2 of 3');

		if (boundary === 'storage') {
			page.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });
		} else if (boundary === 'local') {
			page.handleLocalAuthTransition();
		} else {
			status = Number(boundary);
			await page.refresh();
		}

		expect(page.readFavorites()).toEqual({ favorites: [], announcement: '' });
		expect(page.read().items).toEqual([]);
		expect(page.read().loadError).not.toBeNull();
	});
}

for (const edit of ['unstar', 'clear'] as const) {
	test(`shelf ${edit} clears a prior arrangement announcement`, () => {
		const page = pageScript();
		page.toggleFavorite('row-mike');
		page.toggleFavorite('row-zulu');
		page.setFavoriteAnnouncementForFixture('Mike Song moved to 2 of 2');

		if (edit === 'unstar') page.unstarFromShelf('row-mike');
		else page.confirmClearFavorites();

		expect(page.readFavorites()).toEqual({
			favorites: edit === 'unstar' ? ['row-zulu'] : [],
			announcement: ''
		});
	});
}
