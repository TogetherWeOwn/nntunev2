import { describe, expect, it } from 'vitest';
import { createQueue, type QueueState, type Track } from '../player/queue.js';
import * as queueModule from '../player/queue.js';
import type { LibraryItem } from './cache.js';
import { displayArtist, displayTitle } from './order.js';
import {
	appendSelection,
	createSelection,
	pruneSelection,
	selectSelectedItems,
	toggleSelected
} from './selection.js';

/**
 * Test-Author owned RED acceptance for SELECTEXPORT: export the selected
 * loaded tracks, in checkbox activation order, as a downloaded M3U or as
 * copied text from the library page selection UI. The Builder wires
 * `apps/web/src/routes/library/+page.svelte` to satisfy this file plus
 * `apps/web/tests/library-selection-export.spec.ts`, and must not edit
 * either file. Suspected defects in either file go back to the Test Author.
 *
 * LOCKED CONTRACT (single formatting core, no second formatter):
 * - Export bytes come from the existing shared core in
 *   `apps/web/src/lib/player/queue.ts`: `formatQueueText` and
 *   `formatQueueM3U`, applied over the selected loaded tracks resolved in
 *   ACTIVATION order (the `selectedIds` order the page keeps), each mapped
 *   to a `Track` with the same display fields the queue already uses
 *   (`displayTitle` / `displayArtist ?? ''`, no stream URL). Activation
 *   order is the export order — never loaded order, never the Selected-only
 *   display order (`selectSelectedItems` answers in loaded order and the
 *   ordering/find steps reshape display further), and never queue order.
 *   Selected ids no longer present in the loaded snapshot contribute
 *   nothing (pruning already owns them); unchecking then re-checking a row
 *   moves it to the end of the next export (`toggleSelected` appends).
 * - The id guard inside the shared M3U formatter is the export-safety
 *   refusal. It must be applied before EITHER action produces output: a
 *   selected track whose id the guard refuses means no clipboard write
 *   and no download, only a truthful in-page error (see the browser
 *   spec). Note the text formatter alone never sees the guard, so the
 *   copy path must refuse explicitly instead of assuming it throws.
 * - The shared formatters render an empty list as `''` / `'#EXTM3U'`
 *   (U05 below); the page must nevertheless never present an empty
 *   selection export as a successful copy or download.
 *
 * LOCKED CONTRACT (library page selection UI hooks; behavior in the
 * browser spec):
 * - `selection-export-copy`: native button, accessible name exactly
 *   "Copy selected as text".
 * - `selection-export-download`: native button, accessible name exactly
 *   "Download selected (.m3u)"; the download is named `selected.m3u`.
 * - `selection-export-status`: polite status region inside `<main>`
 *   (`aria-live="polite"` or `role="status"`, never assertive/alert).
 *   Copy success matches /copied/i and never claims a saved file or a
 *   download; download initiation matches /download/i and never claims
 *   the file was saved or that anything was copied; the unsafe-id error
 *   is worded as a failure, never as either success.
 * - `selection-export-fallback`: labeled readonly input/textarea holding
 *   the exact numbered text when the Clipboard API is absent or rejects.
 * - `selection-export-instructions`: manual-copy instructions shown with
 *   the fallback, naming a copy gesture, never claiming success.
 * - No `document.execCommand` path. At most one live Blob URL is retained
 *   (proven in the browser, not by source shape).
 *
 * The U-cases pin what "equals the shared formatting" means, byte for
 * byte, against the real core: they pass on the current tree and must
 * stay green, so the browser spec's literal expectations can never drift
 * from the core unnoticed. The H-cases are RED NOW: the library page has
 * no selection-export hooks and does not reference the shared formatters,
 * so each fails at its assertion with a NEXT action. The C-cases are
 * controls over the existing selection helpers, including the traps an
 * export must not fall into.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/library/selectionExport.acceptance.test.ts
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
	'SELECTEXPORT: expected the shared queue export core to stay available from queue.ts (actual: missing). ' +
	'NEXT: keep formatQueueText/formatQueueM3U as the single formatting core and reuse them for the selection; ' +
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

/**
 * The export mapping: resolve the selection against the loaded rows in
 * ACTIVATION order (first loaded occurrence of an id wins, unknown ids
 * contribute nothing), exactly the resolution `appendSelection` performs.
 */
function selectedQueue(items: readonly LibraryItem[], selectedIds: readonly string[]): QueueState {
	const resolved = selectedIds
		.map((id) => items.find((item) => item.id === id) ?? null)
		.filter((item): item is LibraryItem => item !== null);
	return createQueue(resolved.map(asTrack));
}

// Loaded (API) order is z/a/m/b/d on purpose: the activation order below
// is d/z/m, loaded order of the same subset is z/m/d, title order would
// be d/m/z, so a formatter fed any other list cannot match these literals.
const ITEMS: LibraryItem[] = [
	{ id: 'sel-z', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'sel-a', title: 'Alpha Song', artist: 'Cedar' },
	{
		id: 'sel-m',
		title: 'Mike Song',
		artist: 'Harbor Lights',
		streamUrl: 'https://stream.example/mike?token=SEKRET_TOKEN'
	},
	{ id: 'sel-b', title: 'Bravo Song', artist: 'Solo Act' },
	{ id: 'sel-d', title: 'Delta Song', artist: 'Echo Band' }
];
const ACTIVATED = ['sel-d', 'sel-z', 'sel-m'];

const ACTIVATION_TEXT =
	'1. Delta Song — Echo Band\n2. Zulu Song — Harbor Lights\n3. Mike Song — Harbor Lights';
const ACTIVATION_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Delta Song — Echo Band\nsel-d\n' +
	'#EXTINF:-1,Zulu Song — Harbor Lights\nsel-z\n' +
	'#EXTINF:-1,Mike Song — Harbor Lights\nsel-m';

// Uncheck sel-z, then re-check it: activation order becomes d/m/z.
const REACTIVATED_TEXT =
	'1. Delta Song — Echo Band\n2. Mike Song — Harbor Lights\n3. Zulu Song — Harbor Lights';
const REACTIVATED_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Delta Song — Echo Band\nsel-d\n' +
	'#EXTINF:-1,Mike Song — Harbor Lights\nsel-m\n' +
	'#EXTINF:-1,Zulu Song — Harbor Lights\nsel-z';

// Label edge cases, listed in a load order that differs from the
// activation order used in U04. `gone-mid` and `lbl-skip` are never
// exported: the first is absent from the loaded rows, the second is
// loaded but not selected.
const EDGE_ITEMS: LibraryItem[] = [
	{ id: 'lbl-unicode', title: 'Björk 🦢', artist: 'Sigur Rós 日本語' },
	{ id: 'lbl-skip', title: 'Skipped Song', artist: 'Never Picked' },
	{ id: 'lbl-solo', title: 'Solo Flight', artist: '   ' },
	{ id: 'lbl-blank-title', title: '   ', artist: 'Ghost Choir' },
	{ id: 'lbl-ctrl', title: 'One\nTwo\tThree', artist: 'A\0B' }
];
const EDGE_ACTIVATED = ['lbl-ctrl', 'gone-mid', 'lbl-blank-title', 'lbl-solo', 'lbl-unicode'];
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
// the selection UI, so svelte sources scan together; the browser spec
// proves where they actually render.
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
		'SELECTEXPORT: expected the library route source to exist. NEXT: do not move the library route; add the selection-export actions to its selection UI.'
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

describe('shared core over the activated selection (pass now, must stay green)', () => {
	it('[SELECTEXPORT-U01] numbered text follows activation order, not loaded or title order', () => {
		const { formatQueueText } = exporters();
		expect(formatQueueText(selectedQueue(ITEMS, ACTIVATED))).toBe(ACTIVATION_TEXT);
	});

	it('[SELECTEXPORT-U02] M3U renders #EXTM3U + #EXTINF + bare stable ids in activation order', () => {
		const { formatQueueM3U } = exporters();
		const m3u = formatQueueM3U(selectedQueue(ITEMS, ACTIVATED));
		expect(m3u).toBe(ACTIVATION_M3U);
		for (const leaked of ['stream.example', 'SEKRET_TOKEN', 'https://']) {
			expect(m3u, `M3U bytes must not contain "${leaked}"`).not.toContain(leaked);
		}
	});

	it('[SELECTEXPORT-U03] uncheck then re-check moves the row to the end of the export order', () => {
		const { formatQueueText, formatQueueM3U } = exporters();
		let selection = createSelection();
		for (const id of ACTIVATED) selection = toggleSelected(selection, id);
		selection = toggleSelected(selection, 'sel-z');
		selection = toggleSelected(selection, 'sel-z');
		expect(selection).toEqual(['sel-d', 'sel-m', 'sel-z']);
		const state = selectedQueue(ITEMS, selection);
		expect(formatQueueText(state)).toBe(REACTIVATED_TEXT);
		expect(formatQueueM3U(state)).toBe(REACTIVATED_M3U);
	});

	it('[SELECTEXPORT-U04] label fallbacks/sanitizing match the core; absent and unselected ids are skipped', () => {
		const { formatQueueText, formatQueueM3U } = exporters();
		const state = selectedQueue(EDGE_ITEMS, EDGE_ACTIVATED);
		expect(formatQueueText(state)).toBe(EDGE_TEXT);
		const m3u = formatQueueM3U(state);
		expect(m3u).toBe(EDGE_M3U);
		expect(m3u, 'absent selected ids must not be invented').not.toContain('gone-mid');
		expect(m3u, 'unselected loaded rows must not be exported').not.toContain('lbl-skip');
	});

	it('[SELECTEXPORT-U05] the shared M3U guard refuses an unsafe selected id', () => {
		const { formatQueueM3U } = exporters();
		const state = selectedQueue(
			[...ITEMS, { id: 'bad/id', title: 'Bad Slash', artist: 'Guard' }],
			['sel-z', 'bad/id']
		);
		expect(() => formatQueueM3U(state)).toThrow(/unsafe track id/i);
	});

	it('[SELECTEXPORT-U06] the core renders an empty list as empty text / bare header (page must not sell it as success)', () => {
		const { formatQueueText, formatQueueM3U } = exporters();
		expect(formatQueueText(createQueue([]))).toBe('');
		expect(formatQueueM3U(createQueue([]))).toBe('#EXTM3U');
	});
});

describe('selection-export hooks are declared (RED until the Builder wires them)', () => {
	it('[SELECTEXPORT-H01] declares the copy action hook (selection-export-copy)', () => {
		expect(
			combinedSvelte(),
			'SELECTEXPORT: expected data-testid="selection-export-copy" in the library selection UI markup (actual: missing). NEXT: Builder adds a native <button data-testid="selection-export-copy">Copy selected as text</button> beside the selection actions (see the selection-export browser spec LOCKED CONTRACT).'
		).toContain('data-testid="selection-export-copy"');
	});

	it('[SELECTEXPORT-H02] declares the download action hook (selection-export-download)', () => {
		expect(
			combinedSvelte(),
			'SELECTEXPORT: expected data-testid="selection-export-download" in the library selection UI markup (actual: missing). NEXT: Builder adds a native <button data-testid="selection-export-download">Download selected (.m3u)</button> beside the selection actions (see the selection-export browser spec LOCKED CONTRACT).'
		).toContain('data-testid="selection-export-download"');
	});

	it('[SELECTEXPORT-H03] declares the polite status region (selection-export-status)', () => {
		expect(
			combinedSvelte(),
			'SELECTEXPORT: expected data-testid="selection-export-status" in the library selection UI markup (actual: missing). NEXT: Builder adds the aria-live polite confirmation/error region inside <main> (never assertive, never a false success claim).'
		).toContain('data-testid="selection-export-status"');
	});

	it('[SELECTEXPORT-H04] declares the readonly fallback text (selection-export-fallback)', () => {
		expect(
			combinedSvelte(),
			'SELECTEXPORT: expected data-testid="selection-export-fallback" in the library selection UI markup (actual: missing). NEXT: Builder adds the labeled readonly fallback holding the exact numbered text when the Clipboard API is absent or rejects.'
		).toContain('data-testid="selection-export-fallback"');
	});

	it('[SELECTEXPORT-H05] declares actionable export instructions (selection-export-instructions)', () => {
		expect(
			combinedSvelte(),
			'SELECTEXPORT: expected data-testid="selection-export-instructions" in the library selection UI markup (actual: missing). NEXT: Builder adds manual copy instructions shown with the fallback (no success claim).'
		).toContain('data-testid="selection-export-instructions"');
	});

	it('[SELECTEXPORT-H06] declares the exact visible labels for both actions', () => {
		const source = combinedSvelte();
		expect(
			source,
			'SELECTEXPORT: expected the visible label "Copy selected as text" (actual: missing). NEXT: Builder labels the copy button exactly that.'
		).toContain('Copy selected as text');
		expect(
			source,
			'SELECTEXPORT: expected the visible label "Download selected (.m3u)" (actual: missing). NEXT: Builder labels the download button exactly that.'
		).toContain('Download selected (.m3u)');
	});

	it('[SELECTEXPORT-H07] names the downloaded file selected.m3u', () => {
		expect(
			combinedLogic(),
			'SELECTEXPORT: expected the download filename "selected.m3u" in the library export code (actual: missing). NEXT: Builder sets the transient anchor download name to selected.m3u.'
		).toContain('selected.m3u');
	});
});

describe('selection-export source contract', () => {
	it('[SELECTEXPORT-H08] reuses formatQueueText/formatQueueM3U as the single export source (RED now)', () => {
		const source = combinedLogic();
		expect(
			source,
			'SELECTEXPORT: expected the library selection export to reuse formatQueueText (actual: no reference outside tests). NEXT: Builder imports formatQueueText from the player queue module and builds the copied text with it over the activation-ordered selection (no cloned formatting).'
		).toContain('formatQueueText');
		expect(
			source,
			'SELECTEXPORT: expected the library selection export to reuse formatQueueM3U (actual: no reference outside tests). NEXT: Builder imports formatQueueM3U from the player queue module and builds the downloaded bytes with it over the activation-ordered selection (no cloned formatting).'
		).toContain('formatQueueM3U');
	});

	it('[SELECTEXPORT-H09] never uses a legacy execCommand fallback (control: passes now, must stay green)', () => {
		expect(
			pageSource(),
			'SELECTEXPORT: the library page must never call document.execCommand (no legacy automatic fallback).'
		).not.toContain('execCommand');
	});
});

describe('selection controls (pass now, must stay green)', () => {
	it('[SELECTEXPORT-C01] appendSelection already resolves the selection in activation order', () => {
		// The existing append path is the proof of the resolution the export
		// owes: activation order d/z/m over loaded rows z/a/m/b/d.
		const { queue, added, skipped } = appendSelection(createQueue([]), ACTIVATED, ITEMS);
		expect(added).toBe(3);
		expect(skipped).toBe(0);
		expect(queue.tracks.map((track) => track.id)).toEqual(['sel-d', 'sel-z', 'sel-m']);
	});

	it('[SELECTEXPORT-C02] the Selected-only projection is loaded order, NOT activation order (export must not reuse it as-is)', () => {
		expect(selectSelectedItems(ITEMS, ACTIVATED).map((item) => item.id)).toEqual([
			'sel-z',
			'sel-m',
			'sel-d'
		]);
	});

	it('[SELECTEXPORT-C03] pruning drops vanished ids while preserving activation order', () => {
		expect(pruneSelection(['sel-d', 'gone', 'sel-z', 'sel-m'], ITEMS.map((item) => item.id))).toEqual([
			'sel-d',
			'sel-z',
			'sel-m'
		]);
	});
});
