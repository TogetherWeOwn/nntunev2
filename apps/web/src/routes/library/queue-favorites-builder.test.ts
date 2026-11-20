import { expect, test } from 'vitest';
import ts from 'typescript';
import { addTrack, clearQueue, createQueue, selectTrack as selectQueueIndex, type QueueState } from '#lib/player/queue.js';
import { displayArtist, displayTitle } from '#lib/library/order.js';
import { readMiniPlayerHint } from '#lib/shell/mini-player.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { createPlayerController } from '#lib/player/controller.svelte.js';
import type { LibraryItem } from '#lib/library/cache.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const ITEMS: LibraryItem[] = [
	{ id: 'zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'alpha', title: 'Alpha Song' },
	{ id: 'mike', title: 'Mike Song' }
];

/** Execute the real shelf resolver and handlers, without a DOM or storage.
 * The browser acceptance covers Svelte reactivity and the rendered action.
 * Missing bulk behavior is a no-op so RED is an outcome assertion, not an
 * undefined-function/import error. No queue/resolution logic is reimplemented.
 */
function pageActions(favorites: string[], queue = createQueue([]), announcement = '') {
	const source = sources['./+page.svelte'];
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const script = source.slice(start, source.indexOf('</script', start));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const handlers = new Set([
		'queuedTrackFor', 'playableTrackFor', 'selectTrack', 'queueFavorites', 'clearShareFeedback'
	]);
	// Row selection now clears transient link feedback. Execute that real
	// dependency with its real initial state, not a no-op handler substitute.
	const state = new Set([
		'favoriteTracks', 'favoritesAction', 'shareGeneration', 'shareStatus', 'shareFallbackUrl', 'pendingShareSeek',
		'restartCanSeek', 'restartStatus', 'jumpStatus', 'queueRevision', 'player', 'commitQueue', 'replaceQueue', 'queueController', 'media'
	]);
	const body = parsed.statements.filter((statement) =>
		ts.isFunctionDeclaration(statement)
			? handlers.has(statement.name?.text ?? '')
			: ts.isVariableStatement(statement) && statement.declarationList.declarations.some(
				(declaration) => state.has(declaration.name.getText(parsed))
			)
	).map((statement) => statement.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const bindings = {
		items: ITEMS, favorites, queue, queueAnnouncement: announcement,
		addTrack, resolveStreamUrl, displayArtist, displayTitle,
		createQueue: () => queue, createPlayerController,
		$state: <T>(value: T) => value, $derived: <T>(value: T) => value
	};
	return new Function(...Object.keys(bindings), `
		${compiled}
		const library = queueController.prepareLibrary([...items, ...queue.tracks]);
		player.commitQueue(library.next, 'replace', { sources: library.sources, isCurrent: () => true });
		queueController.subscribe((value) => { queue = value; });
		return {
			queueAll: typeof queueFavorites === 'function' ? queueFavorites : () => {},
			replaceQueue, commitQueue, subscribe: queueController.subscribe,
			read: () => ({ queue, announcement: queueAnnouncement })
		};
	`)(...Object.values(bindings)) as {
		queueAll: () => void;
		replaceQueue: (queue: QueueState) => boolean;
		commitQueue: (queue: QueueState, kind: 'clear') => boolean;
		subscribe: (callback: (queue: QueueState) => void) => () => void;
		read: () => { queue: QueueState; announcement: string };
	};
}

test('queues shelf favorites in star order, not library/title order, with display-only metadata', () => {
	const page = pageActions(['mike', 'zulu', 'alpha']);
	page.queueAll();
	expect(page.read().queue).toEqual({
		tracks: [
			{ id: 'mike', title: 'Mike Song', artist: '', streamUrl: '' },
			{ id: 'zulu', title: 'Zulu Song', artist: 'Harbor Lights', streamUrl: '' },
			{ id: 'alpha', title: 'Alpha Song', artist: '', streamUrl: '' }
		], currentIndex: 0
	});
	expect(page.read().announcement).toMatch(/queued 3.*0 already queued/i);
	expect(readMiniPlayerHint()?.trackId).toBe('mike');
	expect(readMiniPlayerHint()?.title).toBe('Mike Song');
});

test('reentrant replacement stops the superseded favorites action', () => {
	const page = pageActions(['mike', 'alpha'], createQueue([]), 'Earlier outcome');
	const newer = createQueue([{ id: 'zulu', title: 'Zulu Song', artist: '', streamUrl: '' }]);
	const unsubscribe = page.subscribe((value) => {
		if (value.tracks.some((track) => track.id === 'mike')) page.replaceQueue(newer);
	});
	page.queueAll();
	expect(page.read().queue).toBe(newer);
	expect(page.read().announcement).toBe('');
	unsubscribe();
});

test('a nested favorites action keeps its newer no-op announcement', () => {
	const page = pageActions(['mike']);
	let reacted = false;
	const unsubscribe = page.subscribe((value) => {
		if (!reacted && value.tracks.some((track) => track.id === 'mike')) {
			reacted = true;
			page.queueAll();
		}
	});
	page.queueAll();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['mike']);
	expect(page.read().announcement).toMatch(/^queued 0 favorites, 1 already queued/i);
	unsubscribe();
});

test('keeps already queued tracks in place and preserves a non-first current selection', () => {
	const original = selectQueueIndex(createQueue([
		{ id: 'other', title: 'Other', artist: '', streamUrl: '' },
		{ id: 'zulu', title: 'Zulu Song', artist: 'Harbor Lights', streamUrl: '' }
	]), 1);
	const page = pageActions(['mike', 'zulu', 'alpha'], original);
	page.queueAll();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['other', 'zulu', 'mike', 'alpha']);
	expect(page.read().queue.currentIndex).toBe(1);
	expect(original.tracks.map((track) => track.id)).toEqual(['other', 'zulu']);
	expect(page.read().announcement).toMatch(/queued 2.*1 already queued/i);
});

test('skips unresolved favorite ids without counting or inventing tracks', () => {
	const page = pageActions(['missing-before', 'mike', 'missing-between', 'alpha', 'missing-after']);
	page.queueAll();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['mike', 'alpha']);
	expect(page.read().announcement).toMatch(/queued 2.*0 already queued/i);
});

for (const favorites of [[], ['missing-only']]) {
	test(`no resolved favorites leaves the queue and previous announcement untouched (${favorites.length} stored)`, () => {
		const original = createQueue([{ id: 'zulu', title: 'Zulu Song', artist: '', streamUrl: '' }]);
		const page = pageActions(favorites, original, 'Previous queue outcome');
		page.queueAll();
		expect(page.read().queue).toBe(original);
		expect(page.read().announcement).toBe('Previous queue outcome');
	});
}

test('repeated activation replaces the outcome and does not duplicate or reorder the queue', () => {
	const page = pageActions(['mike', 'alpha']);
	page.queueAll();
	expect(page.read().announcement).toMatch(/queued 2.*0 already queued/i);
	const queued = page.read().queue;
	page.queueAll();
	expect(page.read().queue).toBe(queued);
	expect(page.read().announcement).toMatch(/^queued 0.*2 already queued/i);
});

test('a clear subscriber running multi-row queueFavorites leaves its queue and count final', () => {
	const original = createQueue([{ id: 'other', title: 'Other', artist: '', streamUrl: '' }]);
	const page = pageActions(['mike', 'zulu', 'alpha'], original, 'Earlier outcome');
	let reacted = 0;
	const unsubscribe = page.subscribe((value) => {
		if (value.tracks.length === 0) { reacted += 1; page.queueAll(); }
	});
	expect(page.commitQueue(clearQueue(original), 'clear')).toBe(false);
	expect(reacted).toBe(1);
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['mike', 'zulu', 'alpha']);
	expect(page.read().queue.currentIndex).toBe(0);
	expect(page.read().announcement).toMatch(/^queued 3 favorites, 0 already queued/i);
	unsubscribe();
});
