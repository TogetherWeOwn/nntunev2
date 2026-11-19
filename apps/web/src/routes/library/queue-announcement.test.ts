import { afterEach, expect, test, vi } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { createQueueShare } from '#lib/player/queueShare.js';
import { createPlayerController } from '#lib/player/controller.js';
import { createFavoritesExport } from './favoritesExport.js';
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
import { libraryTabTitle } from '#lib/library/tab-title.js';
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

function pageScript(onEndTrackDrag = endTrackDrag) {
	const script = extractFirstScript(sources['./+page.svelte']);
	if (!script) throw new Error('Library page script missing');
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const body = parsed.statements
		.filter((statement) => !ts.isImportDeclaration(statement))
		.map((statement) => ts.isVariableStatement(statement) && statement.declarationList.declarations.some(
			(declaration) => declaration.name.getText(parsed) === 'queue'
		) ? 'let queue; queueController.subscribe((value) => { queue = value; });' : statement.getText(parsed))
		.join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const bindings = {
		addTrack: queueFunctions.addTrack,
		clearQueue: queueFunctions.clearQueue,
		createQueue: queueFunctions.createQueue,
		createQueueShare, createPlayerController,
		createFavoritesExport,
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
		endTrackDrag: onEndTrackDrag,
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
		browserStorage: () => null,
		clearSnapshot: () => {},
		invalidateLibraryCache: () => {},
		clearSignedInEmail: () => {},
		resolveStreamUrl: () => null,
		onMount: () => {},
		tick: () => Promise.resolve(),
		$state: Object.assign(<T>(value: T) => value, { raw: <T>(value: T) => value }),
		$derived: <T>(value: T) => value
	};
	return new Function(...Object.keys(bindings), `${compiled}
		return {
			refresh, selectTrack, moveQueuedTrack, toggleFavorite,
			dispose: refresher.dispose, subscribeQueue: queueController.subscribe,
			handleAuthTransition, handleLocalAuthTransition, unstarFromShelf, confirmClearFavorites,
			read: () => ({ queue, announcement: queueAnnouncement, items, loadError }),
			readFavorites: () => ({ favorites, announcement: favoritesAnnouncement }),
			// Fixture setup only: do not call a non-move page edit, which would
			// clear the announcement before the handler under test ever runs.
			emptyQueueWithoutEdit: () => {
				const next = clearQueue(queue);
				queueController.publish(next, queueController.begin(next, 'clear'));
			},
			setFavoriteAnnouncementForFixture: (announcement) => {
				favoritesAnnouncement = announcement;
			}
		};
	`)(...Object.values(bindings)) as {
		refresh: () => Promise<void>;
		dispose: () => void;
		subscribeQueue: (listener: (queue: queueFunctions.QueueState) => void) => () => void;
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
}

afterEach(() => vi.unstubAllGlobals());

test.each(['handleLocalAuthTransition', 'dispose'] as const)(
	'a deferred response cannot publish to the Library route after %s',
	async (revoke) => {
		const dragCleanup = vi.fn(endTrackDrag);
		const page = pageScript(dragCleanup);
		let resolve!: (response: Response) => void;
		const response = new Promise<Response>((accept) => { resolve = accept; });
		vi.stubGlobal('fetch', () => response);

		const pending = page.refresh();
		page[revoke]();
		const before = dragCleanup.mock.calls.length;
		resolve(Response.json(ITEMS));
		await pending;

		expect(dragCleanup).toHaveBeenCalledTimes(before);
		expect(page.read().items).toEqual([]);
	}
);

test('disposal during queue reconciliation stops the remaining refresh effects', async () => {
	const page = pageScript();
	vi.stubGlobal('fetch', async () => Response.json(ITEMS));
	await page.refresh();
	page.selectTrack(ITEMS[0].id);
	const search = vi.fn(() => '');
	vi.stubGlobal('window', { location: { get search() { return search(); } } });
	const unsubscribe = page.subscribeQueue((queue) => {
		if (queue.tracks.length === 0) page.dispose();
	});
	vi.stubGlobal('fetch', async () => Response.json(ITEMS.slice(1)));
	await page.refresh();
	unsubscribe();

	expect(page.read().queue.tracks).toEqual([]);
	expect(search).not.toHaveBeenCalled();
});

test('cached fallback does not resolve share input on refresh completion', async () => {
	const page = pageScript();
	vi.stubGlobal('fetch', async () => Response.json(ITEMS));
	await page.refresh();
	const search = vi.fn(() => '');
	vi.stubGlobal('window', { location: { get search() { return search(); } } });
	vi.stubGlobal('fetch', async () => { throw new TypeError('network unavailable'); });
	await page.refresh();

	expect(page.read().items).toEqual(ITEMS);
	expect(search).not.toHaveBeenCalled();
	vi.stubGlobal('fetch', async () => Response.json(ITEMS));
	await page.refresh();
	expect(search).toHaveBeenCalledOnce();
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
