import { expect, test } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { LIBRARY_QUEUE_KEY } from '#lib/library/cache.js';

// Builder-owned integration coverage: removing persistence from any rendered
// removal, clear or direct-selection handler must leave the wrong saved payload.
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

function actions() {
	const source = sources['./+page.svelte'];
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const parsed = ts.createSourceFile('library.ts', source.slice(start, source.indexOf('</script', start)), ts.ScriptTarget.Latest, true);
	const handlers = new Set([
		'removeQueuedTrack', 'clearQueuedTracks', 'selectQueuedTrack',
		'replaceQueue', 'persistQueue', 'clearShareFeedback', 'resetRestartOnSourceChange'
	]);
	const body = parsed.statements.filter((node) =>
		ts.isFunctionDeclaration(node) && handlers.has(node.name?.text ?? '')
	).map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const queue = queueFunctions.createQueue([
		{ id: 'alpha', title: 'Alpha', artist: '', streamUrl: '' },
		{ id: 'bravo', title: 'Bravo', artist: '', streamUrl: '' },
		{ id: 'charlie', title: 'Charlie', artist: '', streamUrl: '' }
	]);
	const storage = new Map([[LIBRARY_QUEUE_KEY, queueFunctions.serializeQueue(queue)]]);
	const bindings = {
		queue, queueInitialized: true, LIBRARY_QUEUE_KEY,
		queueAnnouncement: '', queueAnnouncementVersion: 0,
		shareGeneration: 0, shareStatus: '', shareFallbackUrl: null, pendingShareSeek: null,
		restartCanSeek: false, restartStatus: '', playEpoch: 0,
		browserStorage: () => ({
			setItem: (key: string, value: string) => storage.set(key, value),
			removeItem: (key: string) => storage.delete(key)
		}),
		removeTrack: queueFunctions.removeTrack, clearQueue: queueFunctions.clearQueue,
		selectQueueIndex: queueFunctions.selectTrack,
		queueCurrentTrack: queueFunctions.currentTrack, serializeQueue: queueFunctions.serializeQueue
	};
	const page = new Function(...Object.keys(bindings), `${compiled}
		return { removeQueuedTrack, clearQueuedTracks, selectQueuedTrack,
			read: () => ({ queue, announcement: queueAnnouncement }) };
	`)(...Object.values(bindings)) as {
		removeQueuedTrack: (index: number) => void;
		clearQueuedTracks: () => void;
		selectQueuedTrack: (index: number) => void;
		read: () => { queue: queueFunctions.QueueState; announcement: string };
	};
	return { ...page, stored: () => JSON.parse(storage.get(LIBRARY_QUEUE_KEY)!) };
}

test('rendered remove handler persists the surviving arrangement and current ID', () => {
	const page = actions();
	page.removeQueuedTrack(0);
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['bravo', 'charlie']);
	expect(page.stored()).toEqual({ orderIds: ['bravo', 'charlie'], currentId: 'bravo' });
	expect(page.read().announcement).toBe('Alpha removed from the queue.');
});

test('rendered clear handler persists an explicitly empty queue', () => {
	const page = actions();
	page.clearQueuedTracks();
	expect(page.read().queue.tracks).toEqual([]);
	expect(page.stored()).toEqual({ orderIds: [], currentId: null });
	expect(page.read().announcement).toBe('Queue cleared.');
});

test('rendered direct-selection handler persists the stable current ID without reordering', () => {
	const page = actions();
	page.selectQueuedTrack(2);
	expect(page.read().queue.currentIndex).toBe(2);
	expect(page.stored()).toEqual({ orderIds: ['alpha', 'bravo', 'charlie'], currentId: 'charlie' });
	expect(page.read().announcement).toBe('Selected Charlie');
});
