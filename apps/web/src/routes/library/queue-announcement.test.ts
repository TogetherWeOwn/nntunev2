import { afterEach, expect, test, vi } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { createLibraryRefresher } from '#lib/library/refresh.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';
import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';

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
	const body = parsed.statements
		.filter((statement) => !ts.isImportDeclaration(statement))
		.map((statement) => statement.getText(parsed))
		.join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const bindings = {
		addTrack: queueFunctions.addTrack,
		clearQueue: queueFunctions.clearQueue,
		createQueue: queueFunctions.createQueue,
		moveTrack: queueFunctions.moveTrack,
		nextTrack: queueFunctions.nextTrack,
		playNext: queueFunctions.playNext,
		removeTrack: queueFunctions.removeTrack,
		queueCurrentTrack: queueFunctions.currentTrack,
		selectQueueIndex: queueFunctions.selectTrack,
		createLibraryRefresher,
		LIBRARY_AUTH_TRANSITION_KEY,
		subscribeLocalAuthTransition,
		browserStorage: () => null,
		clearSnapshot: () => {},
		invalidateLibraryCache: () => {},
		clearSignedInEmail: () => {},
		resolveStreamUrl: () => null,
		onMount: () => {},
		$state: <T>(value: T) => value,
		$derived: <T>(value: T) => value
	};
	return new Function(...Object.keys(bindings), `${compiled}
		return {
			refresh, selectTrack, moveQueuedTrack,
			handleAuthTransition, handleLocalAuthTransition,
			read: () => ({ queue, announcement: queueAnnouncement, items, loadError }),
			// Fixture setup only: do not call a non-move page edit, which would
			// clear the announcement before the handler under test ever runs.
			emptyQueueWithoutEdit: () => { queue = clearQueue(queue); }
		};
	`)(...Object.values(bindings)) as {
		refresh: () => Promise<void>;
		selectTrack: (id: string) => void;
		moveQueuedTrack: (from: number, to: number) => void;
		handleAuthTransition: (event: Pick<StorageEvent, 'key'>) => void;
		handleLocalAuthTransition: () => void;
		emptyQueueWithoutEdit: () => void;
		read: () => {
			queue: queueFunctions.QueueState;
			announcement: string;
			items: { id: string; title?: string }[];
			loadError: string | null;
		};
	};
}

afterEach(() => vi.unstubAllGlobals());

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
