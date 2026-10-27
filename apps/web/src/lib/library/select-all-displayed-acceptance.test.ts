import { describe, expect, it } from 'vitest';

import type { LibraryItem } from './cache.js';
import { filterLibraryItems } from './filter.js';
import { orderLibraryItems } from './order.js';
import {
	clearSelection,
	hiddenSelectedCount,
	pruneSelection,
	selectionCountLabel,
	toggleSelected
} from './selection.js';
import * as selectionModule from './selection.js';

/**
 * Acceptance: select all displayed tracks in one action — pure helper half.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build makes these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead. The mocked-browser half is
 * `apps/web/tests/library-select-all-displayed.spec.ts`.
 *
 * Contract the build must provide — ONE pure helper added to the existing
 * selection family in `apps/web/src/lib/library/selection.ts` (the module
 * that already owns `toggleSelected` / `clearSelection` / `pruneSelection` /
 * `hiddenSelectedCount`), consumed by the library page's Select all action:
 * - `selectAllDisplayed(selection, displayedIds)`, selection first and an
 *   ID list second, matching `pruneSelection(selection, loadedIds)` and
 *   `hiddenSelectedCount(selection, visibleIds)`. The page passes the IDs
 *   of the currently displayed rows in displayed order — the same ID list
 *   it already hands the hidden-selected count.
 * - The result keeps every valid pre-existing selected ID in its original
 *   activation order (first occurrence wins when a defensive input repeats
 *   one), then appends each displayed ID not already present, in displayed
 *   order, first occurrence only. Already-selected IDs are never moved,
 *   reordered or duplicated; re-applying the helper is a content no-op.
 * - Additive only: a valid selected ID absent from `displayedIds` (hidden
 *   by the find or the view) survives untouched, and an ID absent from
 *   `displayedIds` is never added — so an ID pruned from the loaded
 *   snapshot can only return if the page wrongly displays it again.
 * - Membership is exact and case-sensitive. Invalid entries — non-string
 *   or empty-string, in EITHER input — are ignored without throwing and
 *   are never carried into the result.
 * - Pure and nonmutating: frozen inputs are safe, no storage, network,
 *   clock, randomness or `window`.
 *
 * RED shape: every SA-U case loads the helper through `requireSelectAll`,
 * which turns a missing export into an assertion failure naming the locked
 * home — RED means the behavior is missing, never that the harness cannot
 * load. The no-new-export self-checks below pass on the bare baseline.
 *
 * Case IDs (stable): SA-U01 all displayed from empty, SA-U02 activation-
 * order append, SA-U03 additive over a displayed subset with truthful
 * counts, SA-U04 no duplicates and idempotent re-apply, SA-U05 exact
 * case-sensitive membership, SA-U06 invalid entries ignored, SA-U07
 * frozen-input purity, SA-U08 empty inputs, SA-U09 prune composition
 * never resurrects, SA-U10 find/ordering composition follows the newly
 * displayed set, SA-U11 no globals. Browser cases are SA-B-* in the
 * sibling Playwright spec.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/library/select-all-displayed-acceptance.test.ts
 */

type SelectAllFn = (
	selection: readonly string[],
	displayedIds: readonly string[]
) => string[];

function loadSelectAll(): SelectAllFn | undefined {
	const fn = (selectionModule as unknown as Record<string, unknown>)[
		'selectAllDisplayed'
	];
	return typeof fn === 'function' ? (fn as SelectAllFn) : undefined;
}

const MISSING =
	'missing behavior: implement selectAllDisplayed(selection, displayedIds) in ' +
	'apps/web/src/lib/library/selection.ts — contract header of ' +
	'apps/web/src/lib/library/select-all-displayed-acceptance.test.ts';

async function requireSelectAll(caseId: string): Promise<SelectAllFn> {
	const fn = loadSelectAll();
	expect(fn, `${caseId}: ${MISSING}`).toBeDefined();
	return fn as SelectAllFn;
}

const ids = (items: readonly LibraryItem[]) => items.map((item) => item.id);

// Loaded order is deliberately neither title order nor any activation order
// used below. "Delta Harbor Song" puts the find term in a title while the
// two Harbor Lights rows carry it in the artist, so find scopes reshape
// the displayed set three different ways.
const LOADED: readonly LibraryItem[] = [
	{ id: 'sa-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'sa-alpha', title: 'Alpha Song', artist: 'Solo Act' },
	{ id: 'sa-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'sa-bravo', title: 'Bravo Song', artist: 'Solo Act' },
	{ id: 'sa-delta', title: 'Delta Harbor Song', artist: 'Echo Band' }
];
const LOADED_IDS = ids(LOADED);

describe('select-all fixtures (harness self-checks, no new export required)', () => {
	it('fixture IDs are unique and the shipped selection family already works', () => {
		expect(new Set(LOADED_IDS).size).toBe(LOADED_IDS.length);
		let selection = toggleSelected([], 'sa-mike');
		selection = toggleSelected(selection, 'sa-zulu');
		expect(selection).toEqual(['sa-mike', 'sa-zulu']);
		expect(clearSelection(selection)).toEqual([]);
		expect(pruneSelection(selection, ['sa-zulu'])).toEqual(['sa-zulu']);
		expect(hiddenSelectedCount(selection, ['sa-zulu'])).toBe(1);
		expect(selectionCountLabel(selection.length)).toBe('2 selected');
	});

	it('the shipped find and ordering steps derive the displayed sets used below', () => {
		expect(ids(orderLibraryItems(LOADED, 'original'))).toEqual(LOADED_IDS);
		expect(ids(orderLibraryItems(LOADED, 'title'))).toEqual([
			'sa-alpha',
			'sa-bravo',
			'sa-delta',
			'sa-mike',
			'sa-zulu'
		]);
		expect(ids(filterLibraryItems(LOADED, 'harbor'))).toEqual([
			'sa-zulu',
			'sa-mike',
			'sa-delta'
		]);
		expect(ids(filterLibraryItems(LOADED, 'harbor', 'title'))).toEqual(['sa-delta']);
		expect(ids(filterLibraryItems(LOADED, 'harbor', 'artist'))).toEqual([
			'sa-zulu',
			'sa-mike'
		]);
	});
});

describe('select all displayed', () => {
	it('[SA-U01] from an empty selection it selects every displayed ID in displayed order', async () => {
		const selectAll = await requireSelectAll('SA-U01');
		expect(selectAll([], LOADED_IDS)).toEqual(LOADED_IDS);
		expect(selectAll([], ['sa-mike'])).toEqual(['sa-mike']);
	});

	it('[SA-U02] newly added IDs append in displayed order after the existing activation order', async () => {
		const selectAll = await requireSelectAll('SA-U02');
		// Mike was activated first but sits third in displayed order: it keeps
		// its position, is not duplicated, and the rest append around it in
		// displayed order.
		expect(selectAll(['sa-mike'], LOADED_IDS)).toEqual([
			'sa-mike',
			'sa-zulu',
			'sa-alpha',
			'sa-bravo',
			'sa-delta'
		]);
		// Existing activation order is never reshaped into displayed order.
		expect(selectAll(['sa-delta', 'sa-alpha'], LOADED_IDS)).toEqual([
			'sa-delta',
			'sa-alpha',
			'sa-zulu',
			'sa-mike',
			'sa-bravo'
		]);
	});

	it('[SA-U03] it adds only the displayed subset and preserves hidden selections with truthful counts', async () => {
		const selectAll = await requireSelectAll('SA-U03');
		// Bravo is selected but hidden by a find showing only zulu and mike.
		const displayed = ['sa-zulu', 'sa-mike'];
		const result = selectAll(['sa-bravo'], displayed);
		expect(result).toEqual(['sa-bravo', 'sa-zulu', 'sa-mike']);
		// Loaded-but-undisplayed rows are never swept in.
		expect(result).not.toContain('sa-alpha');
		expect(result).not.toContain('sa-delta');
		// The shipped count helpers stay truthful over the result: bravo is
		// the one hidden selection, the newly added IDs are all visible.
		expect(hiddenSelectedCount(result, displayed)).toBe(1);
		expect(selectionCountLabel(result.length)).toBe('3 selected');
		expect(hiddenSelectedCount(result, LOADED_IDS)).toBe(0);
	});

	it('[SA-U04] it never duplicates an ID and re-applying it is a content no-op', async () => {
		const selectAll = await requireSelectAll('SA-U04');
		expect(selectAll([], ['sa-zulu', 'sa-zulu', 'sa-alpha', 'sa-zulu'])).toEqual([
			'sa-zulu',
			'sa-alpha'
		]);
		// A defensive duplicate in the selection input collapses to its first
		// occurrence instead of surviving twice.
		expect(selectAll(['sa-mike', 'sa-mike', 'sa-alpha'], ['sa-alpha', 'sa-zulu'])).toEqual([
			'sa-mike',
			'sa-alpha',
			'sa-zulu'
		]);
		const once = selectAll(['sa-mike'], LOADED_IDS);
		expect(selectAll(once, LOADED_IDS)).toEqual(once);
		expect(new Set(once).size).toBe(once.length);
	});

	it('[SA-U05] membership is exact and case-sensitive', async () => {
		const selectAll = await requireSelectAll('SA-U05');
		// Different-case spellings are different IDs: they append instead of
		// matching the selected lowercase entry.
		expect(selectAll(['sa-zulu'], ['SA-ZULU', 'Sa-Zulu', 'sa-zulu'])).toEqual([
			'sa-zulu',
			'SA-ZULU',
			'Sa-Zulu'
		]);
		expect(selectAll(['SA-ZULU'], ['sa-zulu'])).toEqual(['SA-ZULU', 'sa-zulu']);
	});

	it('[SA-U06] invalid entries in either input are ignored without throwing', async () => {
		const selectAll = await requireSelectAll('SA-U06');
		const messyDisplayed = [
			null,
			undefined,
			42,
			true,
			{},
			['sa-zulu'],
			'',
			'sa-alpha'
		] as unknown as string[];
		let result: string[] | undefined;
		expect(() => {
			result = selectAll(['sa-mike'], messyDisplayed);
		}, 'invalid displayed entries must not throw').not.toThrow();
		expect(result).toEqual(['sa-mike', 'sa-alpha']);

		const messySelection = [
			'sa-mike',
			'',
			null,
			42,
			['sa-zulu']
		] as unknown as string[];
		expect(() => {
			result = selectAll(messySelection, ['sa-alpha', 'sa-zulu']);
		}, 'invalid selected entries must not throw').not.toThrow();
		// Invalid selected entries are not carried into the result, and the
		// array-wrapped 'sa-zulu' never counted as already selected.
		expect(result).toEqual(['sa-mike', 'sa-alpha', 'sa-zulu']);
		// An entirely invalid displayed list adds nothing.
		expect(selectAll(['sa-mike'], messyDisplayed.slice(0, 7))).toEqual(['sa-mike']);
	});

	it('[SA-U07] it never mutates frozen inputs', async () => {
		const selectAll = await requireSelectAll('SA-U07');
		const selection = Object.freeze(['sa-mike', 'sa-zulu']);
		const displayed = Object.freeze([...LOADED_IDS]);
		const beforeSelection = JSON.stringify(selection);
		const beforeDisplayed = JSON.stringify(displayed);
		const result = selectAll(selection, displayed);
		expect(result).toEqual(['sa-mike', 'sa-zulu', 'sa-alpha', 'sa-bravo', 'sa-delta']);
		expect(result).not.toBe(selection);
		expect(JSON.stringify(selection)).toBe(beforeSelection);
		expect(JSON.stringify(displayed)).toBe(beforeDisplayed);
		// Repeating the call is stable: no hidden state carries over.
		expect(selectAll(selection, displayed)).toEqual(result);
	});

	it('[SA-U08] empty inputs select nothing and preserve the valid selection', async () => {
		const selectAll = await requireSelectAll('SA-U08');
		expect(selectAll([], [])).toEqual([]);
		expect(selectAll(['sa-mike', 'sa-alpha'], [])).toEqual(['sa-mike', 'sa-alpha']);
	});

	it('[SA-U09] composed after prune it never resurrects a pruned ID', async () => {
		const selectAll = await requireSelectAll('SA-U09');
		// A successful refresh drops zulu from the loaded snapshot; the page
		// prunes first, then displays only the survivors.
		const survivors = LOADED.filter((item) => item.id !== 'sa-zulu');
		const pruned = pruneSelection(
			['sa-mike', 'sa-zulu', 'sa-alpha'],
			ids(survivors)
		);
		expect(pruned).toEqual(['sa-mike', 'sa-alpha']);
		const result = selectAll(pruned, ids(survivors));
		expect(result).toEqual(['sa-mike', 'sa-alpha', 'sa-bravo', 'sa-delta']);
		expect(result).not.toContain('sa-zulu');
		expect(selectionCountLabel(result.length)).toBe('4 selected');
	});

	it('[SA-U10] composed with find scope and ordering it follows the newly displayed set and order', async () => {
		const selectAll = await requireSelectAll('SA-U10');
		// Title order reshapes the displayed list; new IDs append in that
		// displayed order behind the one ID activated earlier.
		const byTitle = ids(orderLibraryItems(LOADED, 'title'));
		expect(selectAll(['sa-mike'], byTitle)).toEqual([
			'sa-mike',
			'sa-alpha',
			'sa-bravo',
			'sa-delta',
			'sa-zulu'
		]);
		// The same query under each scope is a different displayed set, and
		// select-all follows whichever set is displayed when it runs.
		const byTitleScope = ids(filterLibraryItems(LOADED, 'harbor', 'title'));
		const afterTitleScope = selectAll([], byTitleScope);
		expect(afterTitleScope).toEqual(['sa-delta']);
		const byArtistScope = ids(filterLibraryItems(LOADED, 'harbor', 'artist'));
		expect(selectAll(afterTitleScope, byArtistScope)).toEqual([
			'sa-delta',
			'sa-zulu',
			'sa-mike'
		]);
		// Once everything is selected, a later ordering change reorders
		// nothing: the append half only ever touches absent IDs.
		const all = selectAll([], LOADED_IDS);
		expect(selectAll(all, byTitle)).toEqual(all);
	});

	it('[SA-U11] it runs with no window, storage or network globals', async () => {
		const selectAll = await requireSelectAll('SA-U11');
		expect(() => selectAll(['sa-mike'], LOADED_IDS)).not.toThrow();
		expect(() => selectAll([], [])).not.toThrow();
	});
});
