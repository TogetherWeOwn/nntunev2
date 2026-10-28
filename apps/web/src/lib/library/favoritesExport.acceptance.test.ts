import { describe, expect, it } from 'vitest';
import { createQueue, type QueueState, type Track } from '../player/queue.js';
import * as queueModule from '../player/queue.js';
import type { LibraryItem } from './cache.js';
import { selectFavoriteItems } from './favorites.js';
import { moveFavoriteId } from './favorites-order.js';
import { displayArtist, displayTitle } from './order.js';

/**
 * Test-Author owned RED acceptance for FAVEXPORT: export the device-local
 * favorites, in their arranged (star/move) order, as a downloaded M3U or as
 * copied text from the library page Favorites section. The Builder wires
 * `apps/web/src/routes/library/+page.svelte` to satisfy this file plus
 * `apps/web/tests/library-favorites-export.spec.ts`, and must not edit
 * either file. Suspected defects in either file go back to the Test Author.
 *
 * LOCKED CONTRACT (single formatting core, no second formatter):
 * - Export bytes come from the existing shared core in
 *   `apps/web/src/lib/player/queue.ts`: `formatQueueText` and
 *   `formatQueueM3U`, applied over the favorites the section lists
 *   (`favoriteTracks`: stored ids resolved against the loaded library, in
 *   stored/arranged order), each mapped to a `Track` with the same display
 *   fields the queue already uses (`displayTitle` / `displayArtist ?? ''`,
 *   no stream URL). Stored ids absent from the loaded library are skipped
 *   in the bytes (there is no title to show for them) and stay stored.
 * - The id guard inside the shared M3U formatter is the export-safety
 *   refusal. It must be applied before EITHER action produces output: a
 *   favorite whose id the guard refuses means no clipboard write and no
 *   download, only a truthful in-page error (see the browser spec). Note
 *   the text formatter alone never sees the guard, so the copy path must
 *   refuse explicitly instead of assuming the formatter throws.
 * - The shared formatters render an empty list as `''` / `'#EXTM3U'`
 *   (U05 below); the page must nevertheless never present an empty
 *   favorites export as a successful copy or download.
 *
 * LOCKED CONTRACT (library page Favorites section hooks; behavior in the
 * browser spec):
 * - `favorites-export-copy`: native button, accessible name exactly
 *   "Copy favorites as text".
 * - `favorites-export-download`: native button, accessible name exactly
 *   "Download favorites (.m3u)"; the download is named `favorites.m3u`.
 * - `favorites-export-status`: polite status region inside `<main>`
 *   (`aria-live="polite"` or `role="status"`, never assertive/alert).
 *   Copy success matches /copied/i and never claims a saved file or a
 *   download; download initiation matches /download/i and never claims
 *   the file was saved or that anything was copied; the unsafe-id error
 *   is worded as a failure, never as either success.
 * - `favorites-export-fallback`: labeled readonly input/textarea holding
 *   the exact numbered text when the Clipboard API is absent or rejects.
 * - `favorites-export-instructions`: manual-copy instructions shown with
 *   the fallback, naming a copy gesture, never claiming success.
 * - No `document.execCommand` path. At most one live Blob URL is retained
 *   (proven in the browser, not by source shape).
 *
 * The U-cases pin what "equals the shared formatting" means, byte for
 * byte, against the real core: they pass on the current tree and must
 * stay green, so the browser spec's literal expectations can never drift
 * from the core unnoticed. The H-cases are RED NOW: the library page has
 * no favorites-export hooks and does not reference the shared formatters,
 * so each fails at its assertion with a NEXT action. The C-cases are
 * controls over the existing arrangement helpers.
 */

// Namespace access (never a static named import of the formatters) keeps
// this file compiling if a sibling change ever renames the core: the
// failure is then an assertion with a NEXT action, not an import error.
type QueueExporters = {
	formatQueueText?: unknown;
	formatQueueM3U?: unknown;
};

const { formatQueueText, formatQueueM3U } = queueModule as unknown as QueueExporters;

type FormatQueue = (state: QueueState) => string;

const MISSING_CORE =
	'FAVEXPORT: expected the shared queue export core to stay available from queue.ts (actual: missing). ' +
	'NEXT: keep formatQueueText/formatQueueM3U as the single formatting core and reuse them for favorites; ' +
	'if a sibling refactor changed their signature, route the drift to the Test Author instead of cloning a formatter.';

function exporters(): { formatQueueText: FormatQueue; formatQueueM3U: FormatQueue } {
	expect(typeof formatQueueText, `${MISSING_CORE} (formatQueueText)`).toBe('function');
	expect(typeof formatQueueM3U, `${MISSING_CORE} (formatQueueM3U)`).toBe('function');
	return {
		formatQueueText: formatQueueText as FormatQueue,
		formatQueueM3U: formatQueueM3U as FormatQueue
	};
}

/** The display-field mapping the library queue already uses for a row. */
function asTrack(item: LibraryItem): Track {
	return {
		id: item.id,
		title: displayTitle(item),
		artist: displayArtist(item) ?? '',
		streamUrl: ''
	};
}

function favoritesQueue(items: readonly LibraryItem[], favoriteIds: readonly string[]): QueueState {
	const resolved = favoriteIds
		.map((id) => items.find((item) => item.id === id) ?? null)
		.filter((item): item is LibraryItem => item !== null);
	return createQueue(resolved.map(asTrack));
}

// Load (API) order is z/a/m on purpose: arranged favorites order below is
// z/m/a, title order would be a/m/z, so a formatter fed the wrong list
// cannot accidentally match these literals.
const ITEMS: LibraryItem[] = [
	{
		id: 'fav-z',
		title: 'Zulu Song',
		artist: 'Harbor Lights',
		streamUrl: 'https://stream.example/zulu?token=SEKRET_TOKEN'
	},
	{ id: 'fav-a', title: 'Alpha Song', artist: 'Cedar' },
	{ id: 'fav-m', title: 'Mike Song', artist: 'Harbor Lights' }
];
const ARRANGED = ['fav-z', 'fav-m', 'fav-a'];

const ARRANGED_TEXT =
	'1. Zulu Song — Harbor Lights\n2. Mike Song — Harbor Lights\n3. Alpha Song — Cedar';
const ARRANGED_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Zulu Song — Harbor Lights\nfav-z\n' +
	'#EXTINF:-1,Mike Song — Harbor Lights\nfav-m\n' +
	'#EXTINF:-1,Alpha Song — Cedar\nfav-a';

// Label edge cases, listed in a load order that differs from the stored
// favorites order used in U03 (which also interleaves absent ids).
const EDGE_ITEMS: LibraryItem[] = [
	{ id: 'lbl-unicode', title: 'Björk 🦢', artist: 'Sigur Rós 日本語' },
	{ id: 'lbl-solo', title: 'Solo Flight', artist: '   ' },
	{ id: 'lbl-blank-title', title: '   ', artist: 'Ghost Choir' },
	{ id: 'lbl-ctrl', title: 'One\nTwo\tThree', artist: 'A\0B' }
];
const EDGE_STORED = ['gone-first', 'lbl-ctrl', 'lbl-blank-title', 'gone-mid', 'lbl-solo', 'lbl-unicode'];
const EDGE_TEXT =
	'1. One Two Three — A B\n' +
	'2. lbl-blank-title — Ghost Choir\n' +
	'3. Solo Flight\n' +
	'4. Björk 🦢 — Sigur Rós 日本語';
const EDGE_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,One Two Three — A B\nlbl-ctrl\n' +
	'#EXTINF:-1,lbl-blank-title — Ghost Choir\nlbl-blank-title\n' +
	'#EXTINF:-1,Solo Flight\nlbl-solo\n' +
	'#EXTINF:-1,Björk 🦢 — Sigur Rós 日本語\nlbl-unicode';

// Raw sources via Vite glob: no node:fs, no @types/node needed. The hooks
// may live inline in the library page or in a component it renders inside
// the Favorites section, so svelte sources scan together; the browser
// spec proves where they actually render.
const svelteSources = import.meta.glob('../**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;
const pageSources = import.meta.glob('../../routes/library/+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;
const libraryModuleSources = import.meta.glob('./*.ts', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;
const routeModuleSources = import.meta.glob('../../routes/library/*.ts', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function pageSource(): string {
	const source = pageSources['../../routes/library/+page.svelte'];
	expect(
		source,
		'FAVEXPORT: expected the library route source to exist. NEXT: do not move the library route; add the favorites-export actions to its Favorites section.'
	).toBeDefined();
	return source ?? '';
}

function combinedSvelte(): string {
	return [pageSource(), ...Object.values(svelteSources)].join('\n');
}

function combinedLogic(): string {
	const modules = Object.entries({ ...libraryModuleSources, ...routeModuleSources })
		.filter(([path]) => !path.includes('.test.'))
		.map(([, source]) => source);
	return [combinedSvelte(), ...modules].join('\n');
}

describe('shared core over arranged favorites (pass now, must stay green)', () => {
	it('[FAVEXPORT-U01] numbered text follows arranged favorites order, not load order', () => {
		const { formatQueueText } = exporters();
		expect(formatQueueText(favoritesQueue(ITEMS, ARRANGED))).toBe(ARRANGED_TEXT);
	});

	it('[FAVEXPORT-U02] M3U renders #EXTM3U + #EXTINF + bare stable ids in arranged order', () => {
		const { formatQueueM3U } = exporters();
		const m3u = formatQueueM3U(favoritesQueue(ITEMS, ARRANGED));
		expect(m3u).toBe(ARRANGED_M3U);
		for (const leaked of ['stream.example', 'SEKRET_TOKEN', 'https://']) {
			expect(m3u, `M3U bytes must not contain "${leaked}"`).not.toContain(leaked);
		}
	});

	it('[FAVEXPORT-U03] label fallbacks/sanitizing match the core and absent ids are skipped', () => {
		const { formatQueueText, formatQueueM3U } = exporters();
		const state = favoritesQueue(EDGE_ITEMS, EDGE_STORED);
		expect(formatQueueText(state)).toBe(EDGE_TEXT);
		expect(formatQueueM3U(state)).toBe(EDGE_M3U);
	});

	it('[FAVEXPORT-U04] the shared M3U guard refuses an unsafe favorite id', () => {
		const { formatQueueM3U } = exporters();
		const state = favoritesQueue(
			[...ITEMS, { id: 'bad/id', title: 'Bad Slash', artist: 'Guard' }],
			['fav-z', 'bad/id']
		);
		expect(() => formatQueueM3U(state)).toThrow(/unsafe track id/i);
	});

	it('[FAVEXPORT-U05] the core renders an empty list as empty text / bare header (page must not sell it as success)', () => {
		const { formatQueueText, formatQueueM3U } = exporters();
		expect(formatQueueText(createQueue([]))).toBe('');
		expect(formatQueueM3U(createQueue([]))).toBe('#EXTM3U');
	});
});

describe('favorites-export hooks are declared (RED until the Builder wires them)', () => {
	it('[FAVEXPORT-H01] declares the copy action hook (favorites-export-copy)', () => {
		expect(
			combinedSvelte(),
			'FAVEXPORT: expected data-testid="favorites-export-copy" in the library Favorites section markup (actual: missing). NEXT: Builder adds a native <button data-testid="favorites-export-copy">Copy favorites as text</button> (see the favorites-export browser spec LOCKED CONTRACT).'
		).toContain('data-testid="favorites-export-copy"');
	});

	it('[FAVEXPORT-H02] declares the download action hook (favorites-export-download)', () => {
		expect(
			combinedSvelte(),
			'FAVEXPORT: expected data-testid="favorites-export-download" in the library Favorites section markup (actual: missing). NEXT: Builder adds a native <button data-testid="favorites-export-download">Download favorites (.m3u)</button> (see the favorites-export browser spec LOCKED CONTRACT).'
		).toContain('data-testid="favorites-export-download"');
	});

	it('[FAVEXPORT-H03] declares the polite status region (favorites-export-status)', () => {
		expect(
			combinedSvelte(),
			'FAVEXPORT: expected data-testid="favorites-export-status" in the library Favorites section markup (actual: missing). NEXT: Builder adds the aria-live polite confirmation/error region inside <main> (never assertive, never a false success claim).'
		).toContain('data-testid="favorites-export-status"');
	});

	it('[FAVEXPORT-H04] declares the readonly fallback text (favorites-export-fallback)', () => {
		expect(
			combinedSvelte(),
			'FAVEXPORT: expected data-testid="favorites-export-fallback" in the library Favorites section markup (actual: missing). NEXT: Builder adds the labeled readonly fallback holding the exact numbered text when the Clipboard API is absent or rejects.'
		).toContain('data-testid="favorites-export-fallback"');
	});

	it('[FAVEXPORT-H05] declares actionable export instructions (favorites-export-instructions)', () => {
		expect(
			combinedSvelte(),
			'FAVEXPORT: expected data-testid="favorites-export-instructions" in the library Favorites section markup (actual: missing). NEXT: Builder adds manual copy instructions shown with the fallback (no success claim).'
		).toContain('data-testid="favorites-export-instructions"');
	});

	it('[FAVEXPORT-H06] declares the exact visible labels for both actions', () => {
		const source = combinedSvelte();
		expect(
			source,
			'FAVEXPORT: expected the visible label "Copy favorites as text" (actual: missing). NEXT: Builder labels the copy button exactly that.'
		).toContain('Copy favorites as text');
		expect(
			source,
			'FAVEXPORT: expected the visible label "Download favorites (.m3u)" (actual: missing). NEXT: Builder labels the download button exactly that.'
		).toContain('Download favorites (.m3u)');
	});

	it('[FAVEXPORT-H07] names the downloaded file favorites.m3u', () => {
		expect(
			combinedLogic(),
			'FAVEXPORT: expected the download filename "favorites.m3u" in the library export code (actual: missing). NEXT: Builder sets the transient anchor download name to favorites.m3u.'
		).toContain('favorites.m3u');
	});
});

describe('favorites-export source contract', () => {
	it('[FAVEXPORT-H08] reuses formatQueueText/formatQueueM3U as the single export source (RED now)', () => {
		const source = combinedLogic();
		expect(
			source,
			'FAVEXPORT: expected the library favorites export to reuse formatQueueText (actual: no reference outside tests). NEXT: Builder imports formatQueueText from the player queue module and builds the copied text with it (no cloned formatting).'
		).toContain('formatQueueText');
		expect(
			source,
			'FAVEXPORT: expected the library favorites export to reuse formatQueueM3U (actual: no reference outside tests). NEXT: Builder imports formatQueueM3U from the player queue module and builds the downloaded bytes with it (no cloned formatting).'
		).toContain('formatQueueM3U');
	});

	it('[FAVEXPORT-H09] never uses a legacy execCommand fallback (control: passes now, must stay green)', () => {
		expect(
			pageSource(),
			'FAVEXPORT: the library page must never call document.execCommand (no legacy automatic fallback).'
		).not.toContain('execCommand');
	});
});

describe('arrangement controls (pass now, must stay green)', () => {
	it('[FAVEXPORT-C01] moving a favorite swaps visible neighbors in their stored slots, hidden ids keep theirs', () => {
		expect(
			moveFavoriteId(['gone', 'fav-z', 'fav-a', 'fav-m'], ['fav-z', 'fav-a', 'fav-m'], 'fav-m', 'up')
		).toEqual(['gone', 'fav-z', 'fav-m', 'fav-a']);
	});

	it('[FAVEXPORT-C02] the load-order projection is NOT arranged order (export must not reuse it as-is)', () => {
		// selectFavoriteItems answers in loaded-list order; the export owes
		// the stored/arranged order instead. This control documents the trap.
		expect(selectFavoriteItems(ITEMS, ['fav-m', 'fav-z']).map((item) => item.id)).toEqual([
			'fav-z',
			'fav-m'
		]);
	});
});
