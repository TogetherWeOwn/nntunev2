import { afterEach, describe, expect, it, vi } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { createLibraryRefresher } from '#lib/library/refresh.js';
import {
	loadFavorites,
	saveFavorites,
	clearFavoritesAtAuthBoundary
} from '#lib/library/favorites.js';
import {
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_AUTH_TRANSITION_KEY,
	LIBRARY_FAVORITES_KEY,
	browserStorage,
	clearSnapshot,
	invalidateLibraryCache,
	type LibraryItem,
	type SnapshotStorage
} from '#lib/library/cache.js';
import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
import { displayArtist, displayTitle, orderLibraryItems } from '#lib/library/order.js';
import { clearSignedInEmail } from '#lib/signed-in-email.js';

/**
 * Acceptance: the arranged library queue is restored across reload.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * queue persist/restore implementation on top of this branch and must make
 * these tests pass WITHOUT editing this file; a wrong test comes back to
 * the acceptance lane instead of being fixed beside the implementation.
 *
 * Storage contract the build implements (mirrored by the offline
 * `library-queue-restore` browser spec, which pins the same key):
 * - The library queue persists tab-locally in `sessionStorage` under
 *   `nntune.library.queue.v1` as `serializeQueue` JSON:
 *   `{"orderIds": string[], "currentId": string | null}`.
 * - Persist synchronously on every queue change (select/add, move, next,
 *   play-next, remove, clear, refresh-reconcile drops). Clearing the queue
 *   persists `{"orderIds": [], "currentId": null}`; auth transitions
 *   REMOVE the key (absent, never an empty payload).
 * - Restore runs only after a successful library load (never during
 *   loading, error, 404-unavailable, or sign-in-required), against the
 *   freshly loaded catalogue: a valid payload restores order + currentId
 *   by stable id (first item when currentId is missing/not stored);
 *   unknown ids, duplicate ids, an invalid shape, invalid JSON, or a
 *   missing key all restore an EMPTY queue without throwing — never the
 *   full catalogue, never invented tracks.
 * - The key is purged at every auth/account transition that clears the
 *   live queue: the page's storage-event + local handlers and
 *   `invalidateLibraryCache()` (the 401/403/sign-out entry point). A
 *   cross-tab clear is never resurrected by stale storage.
 * - Restore never autoplays, seeks, fetches, or changes favorites.
 *
 * Harness: the REAL page script (`./+page.svelte`, imports stripped) with
 * the REAL queue functions, the REAL refresher (fetch mocked, never
 * network), and REAL storage/cache/favorites helpers against memory
 * storage. Only presentation bindings (`$state`/`$derived` as identity),
 * `onMount`, and `resolveStreamUrl` are substituted. RED here means the
 * persist/restore behavior is missing — never a missing import.
 *
 * Implementation-surface contract (the harness strips page imports, so the
 * page script may only reference the bound names below plus the
 * `LIBRARY_QUEUE_KEY` literal): persist via `serializeQueue` +
 * `browserStorage('session')`, restore inline in `onStateChange` after a
 * successful load with `createQueue` + JSON validation, purge via
 * `browserStorage('session')` removal. Persist INLINE in the two queue
 * writers (`replaceQueue`, `moveQueuedTrack`) — no `$effect`-based persist
 * (the harness has no reactivity) — and keep the page-script function
 * names (`refresh`, `selectTrack`, `moveQueuedTrack`,
 * `handleAuthTransition`, `handleLocalAuthTransition`) stable. A build
 * that needs a new page-script import must send this file back to the
 * acceptance lane for a harness update instead of editing it.
 *
 * RED-vs-guard split (verified on the pre-implementation base): the
 * persist (U01-U05), positive-restore (U06-U08), gated-restore (U15, U16),
 * drift-storage (U18, U19), purge (U21, U22, U25, U26) cases are
 * assertion-RED. The empty/unknown/duplicate/corrupt/shape guards
 * (U09-U14, U17), retention guards (U20, U23, U24), and side-effect guards
 * (U27, U28) already pass on the always-empty base queue and pin the
 * "never invent / never clobber / never over-purge" invariants against a
 * naive implementation (e.g. reusing `restoreQueue`'s full-catalogue
 * fallback would fail U09-U12).
 */

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

/** Session-storage key the build persists the library queue under. */
const QUEUE_KEY = 'nntune.library.queue.v1';

const CATALOGUE: LibraryItem[] = [
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' },
	{ id: 'row-zulu', title: 'Zulu Song' },
	{ id: 'row-alpha', title: 'Alpha Song', artist: 'Queue Artist' }
];

const NEXT_CATALOGUE: LibraryItem[] = [
	{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' }
];

function memoryStorage(seed: Record<string, string> = {}): SnapshotStorage {
	const map = new Map(Object.entries(seed));
	return {
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => {
			map.set(key, value);
		},
		removeItem: (key) => {
			map.delete(key);
		}
	};
}

function jsonResponse(payload: unknown, status = 200): Response {
	return new Response(JSON.stringify(payload), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

/** Mock a library fetch resolving to `payload` with an HTTP `status`. */
function stubLibraryFetch(payload: unknown = CATALOGUE, status = 200): void {
	vi.stubGlobal('fetch', async () => jsonResponse(payload, status));
}

interface PageHarness {
	refresh: () => Promise<void>;
	selectTrack: (id: string) => void;
	moveQueuedTrack: (from: number, to: number) => void;
	nextUi: () => void;
	playNextUi: (index: number) => void;
	removeUi: (index: number) => void;
	clearUi: () => void;
	handleAuthTransition: (event: Pick<StorageEvent, 'key'>) => void;
	handleLocalAuthTransition: () => void;
	session: SnapshotStorage;
	local: SnapshotStorage;
	read: () => {
		queue: queueFunctions.QueueState;
		items: LibraryItem[];
		loading: boolean;
		loadError: string | null;
		libraryUnavailable: boolean;
		requiresSignIn: boolean;
		favorites: string[];
		announcement: string;
	};
}

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

/**
 * Run the real page script with memory storage and mocked fetch. Every
 * harness instance owns fresh session/local maps, installed as `window`
 * storage so the REAL `browserStorage`/`invalidateLibraryCache` paths run.
 */
function pageHarness(): PageHarness {
	const script = extractFirstScript(sources['./+page.svelte'] ?? '');
	if (!script) throw new Error('Library page script missing');
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const body = parsed.statements
		.filter((statement) => !ts.isImportDeclaration(statement))
		.map((statement) => statement.getText(parsed))
		.join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const session = memoryStorage();
	const local = memoryStorage();
	vi.stubGlobal('window', { sessionStorage: session, localStorage: local });
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
		shuffleUpcoming: queueFunctions.shuffleUpcoming,
		serializeQueue: queueFunctions.serializeQueue,
		restoreQueue: queueFunctions.restoreQueue,
		loadFavorites,
		saveFavorites,
		clearFavoritesAtAuthBoundary,
		createLibraryRefresher,
		displayArtist,
		displayTitle,
		orderLibraryItems,
		LIBRARY_AUTH_TRANSITION_KEY,
		LIBRARY_QUEUE_KEY: QUEUE_KEY,
		subscribeLocalAuthTransition,
		browserStorage,
		clearSnapshot,
		invalidateLibraryCache,
		clearSignedInEmail,
		resolveStreamUrl: () => null,
		onMount: () => {},
		$state: <T>(value: T) => value,
		$derived: <T>(value: T) => value
	};
	const page = new Function(...Object.keys(bindings), `${compiled}
		return {
			refresh, selectTrack, moveQueuedTrack,
			nextUi: () => replaceQueue(nextTrack(queue)),
			playNextUi: (index) => replaceQueue(playNext(queue, index)),
			removeUi: (index) => replaceQueue(removeTrack(queue, index)),
			clearUi: () => replaceQueue(clearQueue(queue)),
			handleAuthTransition, handleLocalAuthTransition,
			read: () => ({
				queue, items, loading, loadError, libraryUnavailable,
				requiresSignIn, favorites, announcement: queueAnnouncement
			})
		};
	`)(...Object.values(bindings)) as Omit<PageHarness, 'session' | 'local'>;
	return { ...page, session, local };
}

function queueIds(page: PageHarness): string[] {
	return page.read().queue.tracks.map((track) => track.id);
}

function currentId(page: PageHarness): string | null {
	return queueFunctions.currentTrack(page.read().queue)?.id ?? null;
}

/** Parsed persisted payload, or null when the key is absent. */
function storedQueue(session: SnapshotStorage): unknown {
	const raw = session.getItem(QUEUE_KEY);
	return raw === null ? null : (JSON.parse(raw) as unknown);
}

function seedQueue(session: SnapshotStorage, value: unknown): void {
	session.setItem(QUEUE_KEY, typeof value === 'string' ? value : JSON.stringify(value));
}

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('persist on every queue change', () => {
	it('[LIBQR-U01] selecting rows persists orderIds + currentId', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');

		expect(storedQueue(page.session)).toEqual({
			orderIds: ['row-mike', 'row-zulu'],
			currentId: 'row-mike'
		});
	});

	it('[LIBQR-U02] moving a queued track persists the reordered queue', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');
		page.selectTrack('row-alpha');
		page.moveQueuedTrack(2, 0);

		expect(queueIds(page)).toEqual(['row-alpha', 'row-mike', 'row-zulu']);
		expect(storedQueue(page.session)).toEqual({
			orderIds: ['row-alpha', 'row-mike', 'row-zulu'],
			currentId: 'row-mike'
		});
	});

	it('[LIBQR-U03] clearing the queue persists an explicitly empty payload', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.clearUi();

		expect(queueIds(page)).toEqual([]);
		expect(storedQueue(page.session)).toEqual({ orderIds: [], currentId: null });
	});

	it('[LIBQR-U04] removing a queued track persists the remaining queue', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');
		page.selectTrack('row-alpha');
		page.removeUi(0);

		expect(queueIds(page)).toEqual(['row-zulu', 'row-alpha']);
		expect(storedQueue(page.session)).toEqual({
			orderIds: ['row-zulu', 'row-alpha'],
			currentId: 'row-zulu'
		});
	});

	it('[LIBQR-U05] advancing the selection persists the new currentId', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');
		page.nextUi();

		expect(currentId(page)).toBe('row-zulu');
		expect(storedQueue(page.session)).toEqual({
			orderIds: ['row-mike', 'row-zulu'],
			currentId: 'row-zulu'
		});
	});
});

describe('restore after a successful library load', () => {
	it('[LIBQR-U06] a valid payload restores order + current by stable id', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-alpha', 'row-mike'],
			currentId: 'row-alpha'
		});

		await page.refresh();

		expect(queueIds(page)).toEqual(['row-zulu', 'row-alpha', 'row-mike']);
		expect(currentId(page)).toBe('row-alpha');
	});

	it('[LIBQR-U07] a missing currentId restores with the first item current', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, { orderIds: ['row-zulu', 'row-alpha'], currentId: null });

		await page.refresh();

		expect(queueIds(page)).toEqual(['row-zulu', 'row-alpha']);
		expect(currentId(page)).toBe('row-zulu');
	});

	it('[LIBQR-U08] a currentId outside the stored order restores first current', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-alpha'],
			currentId: 'row-nope'
		});

		await page.refresh();

		expect(queueIds(page)).toEqual(['row-zulu', 'row-alpha']);
		expect(currentId(page)).toBe('row-zulu');
	});

	it('[LIBQR-U09] unknown ids restore an empty queue, never invented tracks', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-nope'],
			currentId: 'row-zulu'
		});

		await page.refresh();

		expect(queueIds(page)).toEqual([]);
		expect(currentId(page)).toBeNull();
		expect(page.read().items).toHaveLength(CATALOGUE.length);
	});

	it('[LIBQR-U10] duplicate ids restore an empty queue, never the catalogue', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-zulu'],
			currentId: 'row-zulu'
		});

		await page.refresh();

		expect(queueIds(page)).toEqual([]);
		expect(currentId(page)).toBeNull();
	});

	it('[LIBQR-U11] corrupt JSON restores an empty queue without throwing', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, '{not-json');

		await expect(page.refresh()).resolves.toBeUndefined();
		expect(queueIds(page)).toEqual([]);
		expect(currentId(page)).toBeNull();
	});

	it('[LIBQR-U12] invalid payload shapes restore an empty queue', async () => {
		const shapes: unknown[] = [
			{ orderIds: 'row-zulu', currentId: 'row-zulu' },
			{ currentId: 'row-zulu' },
			{ orderIds: ['row-zulu', 42], currentId: 'row-zulu' },
			['row-zulu']
		];
		for (const shape of shapes) {
			stubLibraryFetch();
			const page = pageHarness();
			seedQueue(page.session, shape);

			await page.refresh();

			expect(queueIds(page), `shape ${JSON.stringify(shape)} must restore empty`).toEqual(
				[]
			);
			expect(currentId(page)).toBeNull();
		}
	});

	it('[LIBQR-U13] an explicitly empty payload restores an empty queue', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, { orderIds: [], currentId: null });

		await page.refresh();

		expect(queueIds(page)).toEqual([]);
		expect(currentId(page)).toBeNull();
	});

	it('[LIBQR-U14] a missing key restores an empty queue, never the catalogue', async () => {
		stubLibraryFetch();
		const page = pageHarness();

		await page.refresh();

		expect(page.session.getItem(QUEUE_KEY)).toBeNull();
		expect(queueIds(page)).toEqual([]);
		expect(currentId(page)).toBeNull();
	});
});

describe('restore gating across load states', () => {
	it('[LIBQR-U15] a failed load restores nothing; Retry restores', async () => {
		stubLibraryFetch({ error: true }, 500);
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-mike'],
			currentId: 'row-mike'
		});

		await page.refresh();

		expect(page.read().loadError).toMatch(/not reachable/i);
		expect(queueIds(page)).toEqual([]);

		stubLibraryFetch();
		await page.refresh();

		expect(page.read().loadError).toBeNull();
		expect(queueIds(page)).toEqual(['row-zulu', 'row-mike']);
		expect(currentId(page)).toBe('row-mike');
	});

	it('[LIBQR-U16] nothing restores while loading; success restores', async () => {
		let resolveFetch!: (response: Response) => void;
		vi.stubGlobal(
			'fetch',
			() => new Promise<Response>((resolve) => {
				resolveFetch = resolve;
			})
		);
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-mike'],
			currentId: 'row-mike'
		});

		const pending = page.refresh();
		expect(page.read().loading).toBe(true);
		expect(queueIds(page)).toEqual([]);

		resolveFetch(jsonResponse(CATALOGUE));
		await pending;

		expect(page.read().loading).toBe(false);
		expect(queueIds(page)).toEqual(['row-zulu', 'row-mike']);
		expect(currentId(page)).toBe('row-mike');
	});

	it('[LIBQR-U17] an unavailable library (404) restores nothing', async () => {
		stubLibraryFetch({ error: true }, 404);
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-mike'],
			currentId: 'row-mike'
		});

		await page.refresh();

		expect(page.read().libraryUnavailable).toBe(true);
		expect(queueIds(page)).toEqual([]);
		expect(currentId(page)).toBeNull();
	});
});

describe('catalogue drift across refresh', () => {
	it('[LIBQR-U18] a refresh that drops catalogue rows drops + persists the kept queue', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');
		page.selectTrack('row-alpha');

		stubLibraryFetch([CATALOGUE[0], CATALOGUE[1]]);
		await page.refresh();

		expect(queueIds(page)).toEqual(['row-mike', 'row-zulu']);
		expect(storedQueue(page.session)).toEqual({
			orderIds: ['row-mike', 'row-zulu'],
			currentId: 'row-mike'
		});
	});

	it('[LIBQR-U19] drift keeps the current track when it survives, else the first kept', async () => {
		stubLibraryFetch();
		const survived = pageHarness();
		await survived.refresh();
		survived.selectTrack('row-mike');
		survived.selectTrack('row-zulu');
		survived.selectTrack('row-alpha');
		survived.nextUi();
		expect(currentId(survived)).toBe('row-zulu');

		stubLibraryFetch([CATALOGUE[1], CATALOGUE[2]]);
		await survived.refresh();

		expect(queueIds(survived)).toEqual(['row-zulu', 'row-alpha']);
		expect(currentId(survived)).toBe('row-zulu');
		expect(storedQueue(survived.session)).toEqual({
			orderIds: ['row-zulu', 'row-alpha'],
			currentId: 'row-zulu'
		});

		stubLibraryFetch();
		const dropped = pageHarness();
		await dropped.refresh();
		dropped.selectTrack('row-mike');
		dropped.selectTrack('row-zulu');
		expect(currentId(dropped)).toBe('row-mike');

		stubLibraryFetch([CATALOGUE[1]]);
		await dropped.refresh();

		expect(queueIds(dropped)).toEqual(['row-zulu']);
		expect(currentId(dropped)).toBe('row-zulu');
		expect(storedQueue(dropped.session)).toEqual({
			orderIds: ['row-zulu'],
			currentId: 'row-zulu'
		});
	});

	it('[LIBQR-U20] a same-catalogue refresh preserves arranged order + current', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');
		page.selectTrack('row-alpha');
		page.moveQueuedTrack(2, 0);
		page.nextUi();
		expect(queueIds(page)).toEqual(['row-alpha', 'row-mike', 'row-zulu']);
		expect(currentId(page)).toBe('row-zulu');

		stubLibraryFetch();
		await page.refresh();

		expect(queueIds(page)).toEqual(['row-alpha', 'row-mike', 'row-zulu']);
		expect(currentId(page)).toBe('row-zulu');
	});
});

describe('purge at auth/account transitions', () => {
	it('[LIBQR-U21] a delivered transition clears the live queue and removes the key', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');
		seedQueue(page.session, {
			orderIds: ['row-mike', 'row-zulu'],
			currentId: 'row-mike'
		});
		expect(queueIds(page)).toHaveLength(2);

		page.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });

		expect(queueIds(page)).toEqual([]);
		expect(page.session.getItem(QUEUE_KEY)).toBeNull();
	});

	it('[LIBQR-U22] a local transition clears the live queue and removes the key', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		seedQueue(page.session, { orderIds: ['row-mike'], currentId: 'row-mike' });

		page.handleLocalAuthTransition();

		expect(queueIds(page)).toEqual([]);
		expect(page.session.getItem(QUEUE_KEY)).toBeNull();
	});

	it('[LIBQR-U23] an unrelated storage key purges nothing', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		const before = JSON.stringify({ orderIds: ['row-mike'], currentId: 'row-mike' });
		page.session.setItem(QUEUE_KEY, before);

		page.handleAuthTransition({ key: 'unrelated-key' });

		expect(page.session.getItem(QUEUE_KEY)).toBe(before);
		expect(queueIds(page)).toEqual(['row-mike']);
	});

	it('[LIBQR-U24] a post-purge refresh with a new catalogue resurrects nothing', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-mike'],
			currentId: 'row-mike'
		});
		await page.refresh();
		page.selectTrack('row-mike');

		page.handleLocalAuthTransition();
		stubLibraryFetch(NEXT_CATALOGUE);
		await page.refresh();

		expect(page.read().items).toEqual(NEXT_CATALOGUE);
		expect(queueIds(page)).toEqual([]);
	});

	it('[LIBQR-U25] invalidateLibraryCache removes the persisted queue', () => {
		const session = memoryStorage();
		const local = memoryStorage();
		vi.stubGlobal('window', { sessionStorage: session, localStorage: local });
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'account-a');
		seedQueue(session, { orderIds: ['row-mike'], currentId: 'row-mike' });

		expect(() => invalidateLibraryCache()).not.toThrow();
		expect(session.getItem(QUEUE_KEY)).toBeNull();
	});

	it('[LIBQR-U26] a 401 response purges the persisted queue and drops the live queue', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		await page.refresh();
		page.selectTrack('row-mike');
		page.selectTrack('row-zulu');
		seedQueue(page.session, {
			orderIds: ['row-mike', 'row-zulu'],
			currentId: 'row-mike'
		});

		stubLibraryFetch({ error: true }, 401);
		await page.refresh();

		expect(page.read().requiresSignIn).toBe(true);
		expect(queueIds(page)).toEqual([]);
		expect(page.session.getItem(QUEUE_KEY)).toBeNull();
	});
});

describe('restore side effects', () => {
	it('[LIBQR-U27] restore never changes favorites', async () => {
		stubLibraryFetch();
		const page = pageHarness();
		page.session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'ctx-a');
		page.local.setItem(`${LIBRARY_FAVORITES_KEY}.ctx-a`, JSON.stringify(['row-mike']));
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-mike'],
			currentId: 'row-mike'
		});

		await page.refresh();

		expect(page.read().favorites).toEqual(['row-mike']);
		expect(page.local.getItem(`${LIBRARY_FAVORITES_KEY}.ctx-a`)).toBe(
			JSON.stringify(['row-mike'])
		);
	});

	it('[LIBQR-U28] restore issues no fetch beyond the library load', async () => {
		let calls = 0;
		vi.stubGlobal('fetch', async () => {
			calls += 1;
			return jsonResponse(CATALOGUE);
		});
		const page = pageHarness();
		seedQueue(page.session, {
			orderIds: ['row-zulu', 'row-mike'],
			currentId: 'row-mike'
		});

		await page.refresh();

		expect(calls).toBe(1);
	});
});
