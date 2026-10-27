import { describe, expect, it } from 'vitest';

import type { LibraryItem } from './cache.js';
import { filterLibraryItems } from './filter.js';
import { orderLibraryItems } from './order.js';
import * as selectionModule from './selection.js';

/**
 * Acceptance: review only selected tracks — pure projection half.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build makes these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead. The mocked-browser half is
 * `apps/web/tests/library-selected-view.spec.ts`.
 *
 * Contract the build must provide — ONE pure, nonmutating projection the
 * library page, the existing Append selected action and any displayed-tracks
 * action all consume (no duplicate selected-list derivation in a component):
 * - `selectSelectedItems(items, selectedIds)` exported from EITHER the
 *   existing `apps/web/src/lib/library/selection.ts` (preferred: the module
 *   that already owns the selected-ID helpers) OR a new
 *   `apps/web/src/lib/library/selected-view.ts`. The loader below checks
 *   `selection.js` first, then `selected-view.js`, so the build picks one
 *   home without touching this file.
 * - Signature: `(items: readonly LibraryItem[], selectedIds: readonly string[])
 *   => LibraryItem[]` — items first, matching `filterLibraryItems(items,
 *   query)` and `orderLibraryItems(items, order)` so the three pure steps
 *   compose in one pipeline.
 * - Keeps exactly the rows whose `id` is a member of `selectedIds`, in the
 *   ORDER OF THE INPUT LIST (the canonical loaded snapshot as handed in),
 *   never in selection activation order. Display order must not silently
 *   become append order: Append selected keeps its existing activation-order
 *   contract, pinned in the browser half.
 * - Exact, case-sensitive ID membership. Selected IDs absent from the input
 *   (unknown or pruned) contribute nothing. Duplicate selected IDs never
 *   duplicate a row. The projection never dedupes the snapshot itself: two
 *   loaded rows sharing a selected ID both survive, in input order (queue
 *   dedupe stays the append path's job).
 * - Non-string or empty selected entries are ignored without throwing.
 * - Always a NEW array holding the SAME object references; neither input
 *   (array, items, or selected IDs) is mutated — frozen inputs are safe.
 * - Pure: no storage, network, clock, randomness, or `window`.
 *
 * RED shape: every SV-U case loads the projection through `requireProject`,
 * which turns a missing export into an assertion failure naming both
 * canonical pointers — RED means the behavior is missing, never that the
 * harness cannot load. The `selected-view.js` specifier is deliberately a
 * `string` variable so TypeScript defers resolution to runtime and
 * `pnpm check` stays green before that optional file exists. The
 * no-module-required self-checks below pass on the bare baseline.
 *
 * Case IDs (stable): SV-U01 loaded order over activation order, SV-U02
 * unknown/pruned and duplicate IDs, SV-U03 empty inputs, SV-U04 exact
 * membership and invalid entries, SV-U05 immutability and identity, SV-U06
 * find composition, SV-U07 ordering composition.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/library/selected-view-acceptance.test.ts
 */

type ProjectFn = (items: readonly LibraryItem[], selectedIds: readonly string[]) => LibraryItem[];

const ALT_SPECIFIER: string = './selected-view.js';

async function loadProject(): Promise<ProjectFn | undefined> {
	const fromSelection = (selectionModule as unknown as Record<string, unknown>)[
		'selectSelectedItems'
	];
	if (typeof fromSelection === 'function') return fromSelection as ProjectFn;
	try {
		const created: unknown = await import(ALT_SPECIFIER);
		const fn = (created as Record<string, unknown>)['selectSelectedItems'];
		if (typeof fn === 'function') return fn as ProjectFn;
	} catch {
		// Optional module not created: the export is simply missing.
	}
	return undefined;
}

const MISSING =
	'missing behavior: implement selectSelectedItems(items, selectedIds) in ' +
	'apps/web/src/lib/library/selection.ts (preferred) or ' +
	'apps/web/src/lib/library/selected-view.ts — contract header of ' +
	'apps/web/src/lib/library/selected-view-acceptance.test.ts';

async function requireProject(caseId: string): Promise<ProjectFn> {
	const fn = await loadProject();
	expect(fn, `${caseId}: ${MISSING}`).toBeDefined();
	return fn as ProjectFn;
}

const ids = (items: readonly LibraryItem[]) => items.map((item) => item.id);

// Loaded order is deliberately neither title order nor any activation order
// used below, so a sorted or activation-ordered projection fails loudly.
const LOADED: readonly LibraryItem[] = [
	{ id: 'sv-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'sv-alpha', title: 'Alpha Song', artist: 'Solo Act' },
	{ id: 'sv-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'sv-bravo', title: 'Bravo Song', artist: 'Solo Act' },
	{ id: 'sv-delta', title: 'Delta Song', artist: 'Echo Band' }
];

describe('selected-view fixtures (harness self-checks, no new module required)', () => {
	it('fixture IDs are unique and the existing pure steps load and run', () => {
		const loadedIds = ids(LOADED);
		expect(new Set(loadedIds).size).toBe(loadedIds.length);
		// The composition partners this projection must join already work:
		expect(ids(filterLibraryItems(LOADED, 'harbor'))).toEqual(['sv-zulu', 'sv-mike']);
		expect(ids(orderLibraryItems(LOADED, 'title'))).toEqual([
			'sv-alpha',
			'sv-bravo',
			'sv-delta',
			'sv-mike',
			'sv-zulu'
		]);
		expect(ids(orderLibraryItems(LOADED, 'original'))).toEqual(loadedIds);
	});
});

describe('selected-only pure projection', () => {
	it('[SV-U01] keeps a noncontiguous selection in loaded-list order, never activation order', async () => {
		const project = await requireProject('SV-U01');
		// Activation order (how the boxes were checked) runs against loaded
		// order at every step; the projection must follow the loaded list.
		const activated = ['sv-delta', 'sv-zulu', 'sv-mike'];
		expect(ids(project(LOADED, activated))).toEqual(['sv-zulu', 'sv-mike', 'sv-delta']);
		// The same membership activated in another order renders identically.
		expect(ids(project(LOADED, ['sv-mike', 'sv-delta', 'sv-zulu']))).toEqual([
			'sv-zulu',
			'sv-mike',
			'sv-delta'
		]);
		expect(ids(project(LOADED, ['sv-bravo']))).toEqual(['sv-bravo']);
	});

	it('[SV-U02] ignores unknown/pruned and duplicate selected IDs without duplicating rows', async () => {
		const project = await requireProject('SV-U02');
		expect(ids(project(LOADED, ['sv-mike', 'sv-not-loaded', 'sv-pruned', 'sv-zulu']))).toEqual([
			'sv-zulu',
			'sv-mike'
		]);
		expect(ids(project(LOADED, ['sv-mike', 'sv-mike', 'sv-mike']))).toEqual(['sv-mike']);
		// The projection does not dedupe the snapshot: both loaded rows that
		// carry a selected ID survive, in input order.
		const withDuplicateLoaded = [
			{ id: 'sv-zulu', title: 'Zulu Song' },
			{ id: 'sv-other', title: 'Other Song' },
			{ id: 'sv-zulu', title: 'Zulu Song (second copy)' }
		];
		const kept = project(withDuplicateLoaded, ['sv-zulu']);
		expect(kept.map((item) => item.title)).toEqual(['Zulu Song', 'Zulu Song (second copy)']);
		expect(kept[0]).toBe(withDuplicateLoaded[0]);
		expect(kept[1]).toBe(withDuplicateLoaded[2]);
	});

	it('[SV-U03] empty selection and empty input both project to a new empty array', async () => {
		const project = await requireProject('SV-U03');
		const noneSelected = project(LOADED, []);
		expect(noneSelected).toEqual([]);
		expect(noneSelected).not.toBe(LOADED);
		expect(project([], ['sv-zulu'])).toEqual([]);
		expect(project([], [])).toEqual([]);
		const all = project(LOADED, ids(LOADED));
		expect(ids(all)).toEqual(ids(LOADED));
		expect(all).not.toBe(LOADED);
	});

	it('[SV-U04] membership is exact and case-sensitive; invalid entries never throw', async () => {
		const project = await requireProject('SV-U04');
		expect(ids(project(LOADED, ['SV-ZULU', 'Sv-Mike', ' sv-delta ']))).toEqual([]);
		const messy = ['sv-alpha', '', '   ', null, undefined, 42, true, {}, ['sv-zulu']] as unknown as string[];
		let result: LibraryItem[] | undefined;
		expect(() => {
			result = project(LOADED, messy);
		}, 'invalid selected entries must not throw').not.toThrow();
		expect(ids(result as LibraryItem[])).toEqual(['sv-alpha']);
	});

	it('[SV-U05] never mutates frozen inputs and returns the same row references', async () => {
		const project = await requireProject('SV-U05');
		const items = Object.freeze([
			Object.freeze({ id: 'sv-zulu', title: 'Zulu Song', artist: 'Harbor Lights' }),
			Object.freeze({ id: 'sv-alpha', title: 'Alpha Song' }),
			Object.freeze({ id: 'sv-mike', title: 'Mike Song', artist: 'Harbor Lights' })
		]);
		const selected = Object.freeze(['sv-mike', 'sv-zulu']);
		const beforeItems = JSON.stringify(items);
		const beforeSelected = JSON.stringify(selected);
		const result = project(items, selected);
		expect(ids(result)).toEqual(['sv-zulu', 'sv-mike']);
		expect(result).not.toBe(items);
		expect(result[0]).toBe(items[0]);
		expect(result[1]).toBe(items[2]);
		expect(JSON.stringify(items)).toBe(beforeItems);
		expect(JSON.stringify(selected)).toBe(beforeSelected);
		// Repeating the projection is stable: no hidden state carries over.
		expect(ids(project(items, selected))).toEqual(['sv-zulu', 'sv-mike']);
	});

	it('[SV-U06] composes with the existing find matcher in either pipeline order', async () => {
		const project = await requireProject('SV-U06');
		const selected = ['sv-delta', 'sv-zulu', 'sv-mike'];
		// Find "harbor" over the projection: only the selected Harbor rows.
		expect(ids(filterLibraryItems(project(LOADED, selected), 'harbor'))).toEqual([
			'sv-zulu',
			'sv-mike'
		]);
		// Projecting the find result gives the same rows in the same order.
		expect(ids(project(filterLibraryItems(LOADED, 'harbor'), selected))).toEqual([
			'sv-zulu',
			'sv-mike'
		]);
		// A find hit that is not selected never appears; an empty find is a no-op.
		expect(ids(filterLibraryItems(project(LOADED, selected), 'solo'))).toEqual([]);
		expect(ids(filterLibraryItems(project(LOADED, selected), ''))).toEqual([
			'sv-zulu',
			'sv-mike',
			'sv-delta'
		]);
	});

	it('[SV-U07] composes with existing ordering: display order comes from the ordering step', async () => {
		const project = await requireProject('SV-U07');
		const selected = ['sv-delta', 'sv-zulu', 'sv-mike'];
		// Projection first, ordering second (the page pipeline): title order
		// inside the selected subset only.
		expect(ids(orderLibraryItems(project(LOADED, selected), 'title'))).toEqual([
			'sv-delta',
			'sv-mike',
			'sv-zulu'
		]);
		expect(ids(orderLibraryItems(project(LOADED, selected), 'title-desc'))).toEqual([
			'sv-zulu',
			'sv-mike',
			'sv-delta'
		]);
		// Ordering first: the projection faithfully keeps ITS input order,
		// which is what makes it a single reusable pipeline step.
		expect(ids(project(orderLibraryItems(LOADED, 'title'), selected))).toEqual([
			'sv-delta',
			'sv-mike',
			'sv-zulu'
		]);
		// Original order remains the loaded order, untouched by either step.
		expect(ids(orderLibraryItems(project(LOADED, selected), 'original'))).toEqual([
			'sv-zulu',
			'sv-mike',
			'sv-delta'
		]);
		expect(ids(LOADED)).toEqual(['sv-zulu', 'sv-alpha', 'sv-mike', 'sv-bravo', 'sv-delta']);
	});
});
