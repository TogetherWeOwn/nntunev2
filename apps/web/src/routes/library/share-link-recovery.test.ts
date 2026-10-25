import { afterEach, expect, test, vi } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { createLibraryRefresher } from '#lib/library/refresh.js';
import { loadFavorites, selectFavoriteItems } from '#lib/library/favorites.js';
import { displayArtist, displayTitle, orderLibraryItems } from '#lib/library/order.js';
import { createSelection, pruneSelection } from '#lib/library/selection.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { resolveShareTarget } from '#lib/player/shareTimestamp.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const ITEMS = [{ id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' }];

/** Execute the real route script, refresher notifications and share helpers.
 * Only lifecycle/storage and Svelte state cells are substituted. Browser cases
 * in library-share-link-lifecycle.spec.ts cover navigation and rendered media.
 */
function pageScript(search: string) {
	vi.stubGlobal('window', { location: { search } });
	const source = sources['./+page.svelte'];
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const script = source.slice(start, source.indexOf('</script', start));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const body = parsed.statements.filter((statement) => !ts.isImportDeclaration(statement))
		.map((statement) => statement.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const bindings = {
		addTrack: queueFunctions.addTrack,
		clearQueue: queueFunctions.clearQueue,
		createQueue: queueFunctions.createQueue,
		queueCurrentTrack: queueFunctions.currentTrack,
		selectQueueIndex: queueFunctions.selectTrack,
		createLibraryRefresher, loadFavorites, selectFavoriteItems,
		displayArtist, displayTitle, orderLibraryItems, createSelection, pruneSelection,
		resolveStreamUrl, resolveShareTarget,
		browserStorage: () => null,
		invalidateLibraryCache: () => {},
		onMount: () => {},
		afterNavigate: () => {},
		tick: () => Promise.resolve(),
		$state: <T>(value: T) => value,
		$derived: <T>(value: T) => value
	};
	return new Function(...Object.keys(bindings), `${compiled}
		return {
			refresh, applyShareLink, resetShareAtAuthBoundary, selectTrack,
			applyPendingShareSeek,
			setAudio: (element) => { audioEl = element; },
			read: () => ({ queue, shareInvalid, handledShareSearch, pendingShareSeek, loadError })
		};
	`)(...Object.values(bindings)) as {
		refresh: () => Promise<void>;
		applyShareLink: () => void;
		resetShareAtAuthBoundary: () => void;
		selectTrack: (id: string) => void;
		applyPendingShareSeek: () => void;
		setAudio: (element: { currentTime: number; duration: number }) => void;
		read: () => {
			queue: queueFunctions.QueueState;
			shareInvalid: boolean;
			handledShareSearch: string | null;
			pendingShareSeek: { seconds: number } | null;
			loadError: string | null;
		};
	};
}

function serve(payload = ITEMS) {
	vi.stubGlobal('fetch', async () => new Response(JSON.stringify(payload), {
		headers: { 'content-type': 'application/json' }
	}));
}

afterEach(() => vi.unstubAllGlobals());

for (const status of [500, 404]) {
	test(`a valid initial link survives an empty ${status} load and applies once on Retry`, async () => {
		vi.stubGlobal('fetch', async () => new Response(null, { status }));
		const page = pageScript('?track=coral&t=90');
		await page.refresh();
		expect(page.read().handledShareSearch).toBeNull();
		expect(page.read().shareInvalid).toBe(false);
		expect(page.read().queue.tracks).toEqual([]);

		serve();
		await page.refresh();
		expect(queueFunctions.currentTrack(page.read().queue)?.id).toBe('coral');
		expect(page.read().shareInvalid).toBe(false);
		expect(page.read().pendingShareSeek?.seconds).toBe(90);
		const audio = { currentTime: 0, duration: 60 };
		page.setAudio(audio);
		page.applyPendingShareSeek();
		expect(audio.currentTime).toBe(60);
		audio.currentTime = 12;
		await page.refresh();
		page.applyPendingShareSeek();
		expect(audio.currentTime).toBe(12);
	});
}

test('a valid same-route navigation while the empty load is failed survives Retry', async () => {
	vi.stubGlobal('fetch', async () => { throw new TypeError('synthetic network refusal'); });
	const page = pageScript('');
	await page.refresh();
	window.location.search = '?track=coral&t=9';
	page.applyShareLink();
	expect(page.read().handledShareSearch).toBeNull();
	expect(page.read().shareInvalid).toBe(false);
	serve();
	await page.refresh();
	expect(queueFunctions.currentTrack(page.read().queue)?.id).toBe('coral');
	expect(page.read().pendingShareSeek?.seconds).toBe(9);
	expect(page.read().shareInvalid).toBe(false);
});

for (const search of ['?track=missing&t=9', '?track=coral&t=bad']) {
	test(`Retry still rejects invalid parameters atomically: ${search}`, async () => {
		vi.stubGlobal('fetch', async () => new Response(null, { status: 500 }));
		const page = pageScript(search);
		await page.refresh();
		serve();
		await page.refresh();
		expect(page.read().shareInvalid).toBe(true);
		expect(page.read().handledShareSearch).toBe(search);
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.read().pendingShareSeek).toBeNull();
	});
}

test('a successfully loaded empty library rejects the target and does not replay on Refresh', async () => {
	serve([]);
	const page = pageScript('?track=coral&t=9');
	await page.refresh();
	expect(page.read().shareInvalid).toBe(true);
	expect(page.read().handledShareSearch).toBe('?track=coral&t=9');
	serve();
	await page.refresh();
	expect(page.read().queue.tracks).toEqual([]);
});

for (const status of [401, 403]) {
	test(`${status} invalidation still fences the link from the next account's Retry`, async () => {
		vi.stubGlobal('fetch', async () => new Response(null, { status }));
		const page = pageScript('?track=coral&t=9');
		await page.refresh();
		serve();
		await page.refresh();
		expect(page.read().shareInvalid).toBe(false);
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.read().pendingShareSeek).toBeNull();
	});
}
