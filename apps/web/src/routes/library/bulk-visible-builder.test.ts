import { expect, test } from 'vitest';
import ts from 'typescript';
import { displayArtist, displayTitle, orderLibraryItems, type LibraryOrder } from '#lib/library/order.js';
import { selectFavoriteItems } from '#lib/library/favorites.js';
import { filterLibraryItems } from '#lib/library/filter.js';
import { createQueue, currentTrack as queueCurrentTrack, type QueueState } from '#lib/player/queue.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';

// Builder-owned integration regression: the merged favorites/query projections
// must bound both the bulk action and its disabled/already-queued states.
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const ITEMS = [
	{ id: 'alpha', title: 'Alpha Song', artist: 'North Harbor' },
	{ id: 'middle', title: 'Middle Song', artist: 'South Bay' },
	{ id: 'zulu', title: 'Zulu Song', artist: 'North Harbor' }
];

function actions(view: 'all' | 'favorites', favorites: string[], queue: QueueState, findQuery = '', order: LibraryOrder = 'original') {
	const source = sources['./+page.svelte'];
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const parsed = ts.createSourceFile('library.ts', source.slice(start, source.indexOf('</script', start)), ts.ScriptTarget.Latest, true);
	const handlers = new Set([
		'missingDisplayedTracks', 'queuedTrackFor', 'replaceQueue',
		'addDisplayedTracksToQueue', 'clearShareFeedback', 'resetRestartOnSourceChange'
	]);
	const functions = parsed.statements.filter((node) => ts.isFunctionDeclaration(node) && handlers.has(node.name?.text ?? ''));
	// Queue replacement clears real share state when the current source changes.
	// Include those dependencies without substituting or weakening the handler.
	const stateNames = new Set(['shareGeneration', 'shareStatus', 'shareFallbackUrl', 'pendingShareSeek', 'restartCanSeek', 'restartStatus', 'jumpFeedback']);
	const state = parsed.statements.filter((statement) => ts.isVariableStatement(statement) && statement.declarationList.declarations.some(
		(declaration) => stateNames.has(declaration.name.getText(parsed))
	));
	const derived = ['orderedItems', 'displayedItems', 'visibleItems', 'bulkMissing', 'bulkDisabled', 'bulkAllQueued'].map((name) => {
		const node = parsed.statements.find((statement) => ts.isVariableStatement(statement) && statement.declarationList.declarations.some(
			(declaration) => declaration.name.getText(parsed) === name
		));
		if (!node) throw new Error(`Missing library derived value: ${name}`);
		return node;
	});
	const body = [...functions, ...state, ...derived].map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const bindings = {
		view, favorites, queue, findQuery, findScope: 'both', order, items: ITEMS,
		queuedIds: new Set(queue.tracks.map((track) => track.id)),
		libraryUnavailable: false, queueAnnouncement: 'Previous outcome',
		displayArtist, displayTitle, selectFavoriteItems, orderLibraryItems, filterLibraryItems,
		queueCurrentTrack, resolveStreamUrl,
		$state: <T>(value: T) => value,
		$derived: <T>(value: T) => value
	};
	return new Function(...Object.keys(bindings), `${compiled}
		return {
			disabled: bulkDisabled, allQueued: bulkAllQueued,
			add: addDisplayedTracksToQueue,
			read: () => ({ queue, announcement: queueAnnouncement })
		};
	`)(...Object.values(bindings)) as {
		disabled: boolean; allQueued: boolean; add: () => void;
		read: () => { queue: QueueState; announcement: string };
	};
}

const queued = (ids: string[]) => createQueue(ids.map((id) => ({ id, title: id, artist: '', streamUrl: '' })));

test('Favorites-only bulk appends visible missing IDs in displayed order, preserving a hidden prefix/current', () => {
	const before = { ...queued(['outside', 'alpha']), currentIndex: 1 };
	const page = actions('favorites', ['zulu', 'alpha'], before);
	expect(page.disabled).toBe(false);
	page.add();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['outside', 'alpha', 'zulu']);
	expect(page.read().queue.currentIndex).toBe(1);
	expect(page.read().announcement).toBe('Added 1 track to the queue.');
	expect(before.tracks.map((track) => track.id)).toEqual(['outside', 'alpha']);
});

test('an empty Favorites-only view disables bulk despite hidden loaded tracks', () => {
	const before = queued([]);
	const page = actions('favorites', [], before);
	expect(page.disabled).toBe(true);
	expect(page.allQueued).toBe(false);
	page.add();
	expect(page.read()).toEqual({ queue: before, announcement: 'Previous outcome' });
});

test('all visible favorites already queued disables bulk while hidden tracks stay missing', () => {
	const before = queued(['zulu', 'alpha']);
	const page = actions('favorites', ['alpha', 'zulu'], before);
	expect(page.disabled).toBe(true);
	expect(page.allQueued).toBe(true);
	page.add();
	expect(page.read().queue).toBe(before);
	expect(page.read().announcement).toBe('Previous outcome');
});

test('All tracks bulk still appends the full displayed ordering', () => {
	const page = actions('all', ['zulu'], queued([]));
	page.add();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['alpha', 'middle', 'zulu']);
	expect(page.read().announcement).toBe('Added 3 tracks to the queue.');
});

test('a title query excludes hidden loaded tracks from the append and added count', () => {
	const before = { ...queued(['outside', 'alpha']), currentIndex: 1 };
	const page = actions('all', [], before, 'zULu');
	page.add();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['outside', 'alpha', 'zulu']);
	expect(page.read().queue.currentIndex).toBe(1);
	expect(page.read().announcement).toBe('Added 1 track to the queue.');
});

test('an artist query composes with Favorites-only and descending displayed order', () => {
	const page = actions('favorites', ['alpha', 'zulu'], queued([]), 'north', 'title-desc');
	page.add();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['zulu', 'alpha']);
	expect(page.read().announcement).toBe('Added 2 tracks to the queue.');
});

test('a zero-match query disables bulk without an all-queued hint or queue mutation', () => {
	const before = queued(['alpha']);
	const page = actions('all', [], before, 'no matching title or artist');
	expect(page.disabled).toBe(true);
	expect(page.allQueued).toBe(false);
	page.add();
	expect(page.read().queue).toBe(before);
	expect(page.read().announcement).toBe('Previous outcome');
});

test('all query matches already queued disables bulk while hidden matches remain missing', () => {
	const before = queued(['zulu']);
	const page = actions('all', [], before, 'Zulu Song');
	expect(page.disabled).toBe(true);
	expect(page.allQueued).toBe(true);
	page.add();
	expect(page.read().queue).toBe(before);
	expect(page.read().announcement).toBe('Previous outcome');
});
