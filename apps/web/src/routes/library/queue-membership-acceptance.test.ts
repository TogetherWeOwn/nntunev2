import { expect, test } from 'vitest';
import ts from 'typescript';
import type { LibraryItem } from '#lib/library/cache.js';
import { selectFavoriteItems } from '#lib/library/favorites.js';
import { filterLibraryItems } from '#lib/library/filter.js';
import { orderLibraryItems, type LibraryOrder } from '#lib/library/order.js';
import { hiddenSelectedCount } from '#lib/library/selection.js';

// Acceptance-owned. Execute the actual route projection, following the existing
// find-composition-builder harness, not a second implementation of membership.
// The additional input contract is queueView: all | queued | not-queued, plus
// queuedIds (stable IDs, including non-current entries). Missing behavior must
// fail on projected values, never on an absent module or import.
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const source = sources['./+page.svelte'];
const start = source.indexOf('>', source.indexOf('<script')) + 1;
const script = source.slice(start, source.indexOf('</script', start));
const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
const names = ['orderedItems', 'displayedItems', 'visibleItems', 'hiddenSelectionCount'];
const declarations = parsed.statements.filter((statement) =>
	ts.isVariableStatement(statement) && statement.declarationList.declarations.some(
		(declaration) => ts.isIdentifier(declaration.name) && names.includes(declaration.name.text)
	)
);
const compiled = ts.transpileModule(
	declarations.map((statement) => statement.getText(parsed)).join('\n'),
	{ compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }
).outputText;
const evaluate = new Function(
	'items', 'order', 'favorites', 'view', 'findQuery', 'selectedIds', 'queueView', 'queuedIds',
	'orderLibraryItems', 'selectFavoriteItems', 'filterLibraryItems', 'hiddenSelectedCount', '$derived',
	`${compiled}\nreturn { visibleItems, hiddenSelectionCount };`
);

const ITEMS = [
	{ id: 'member-zulu', title: 'Shared Tune', artist: 'North Harbor' },
	{ id: 'member-alpha', title: 'Alpha Song', artist: 'North Harbor' },
	{ id: 'member-twin', title: 'Shared Tune', artist: 'South Bay' },
	{ id: 'member-literal', title: 'What.*Ever', artist: 'C++ Players' },
	{ id: 'member-fallback' }
];
const FAVORITES = ['member-zulu', 'member-alpha', 'member-twin'];
const SELECTED = ['member-alpha', 'member-twin'];
type Mode = 'all' | 'queued' | 'not-queued';

function project(queueView: Mode, queued: string[], options: {
	view?: 'all' | 'favorites'; query?: string; order?: LibraryOrder; items?: LibraryItem[];
} = {}) {
	return evaluate(
		options.items ?? ITEMS, options.order ?? 'original', FAVORITES, options.view ?? 'all',
		options.query ?? '', SELECTED, queueView, new Set(queued),
		orderLibraryItems, selectFavoriteItems, filterLibraryItems, hiddenSelectedCount,
		<T>(value: T) => value
	) as { visibleItems: LibraryItem[]; hiddenSelectionCount: number };
}

// A title-based filter, a current-track-only filter, or an inverted predicate
// must fail these literal, independently enumerated projections.
test.each([
	['MEM-U-01', 'all', ['member-zulu', 'member-alpha', 'member-twin', 'member-literal', 'member-fallback'], 0],
	['MEM-U-02', 'queued', ['member-zulu', 'member-alpha'], 1],
	['MEM-U-03', 'not-queued', ['member-twin', 'member-literal', 'member-fallback'], 1]
] as const)('%s: %s uses stable membership, not duplicate titles or current index', (_id, mode, ids, hidden) => {
	const result = project(mode, ['member-zulu', 'member-alpha', 'outside-loaded-snapshot']);
	expect(result.visibleItems.map((item) => item.id)).toEqual(ids);
	expect(result.hiddenSelectionCount).toBe(hidden);
});

test('MEM-U-04: membership intersects Favorites, literal query and descending order', () => {
	const result = project('not-queued', ['member-zulu'], {
		view: 'favorites', query: ' NORTH HARBOR ', order: 'title-desc'
	});
	expect(result.visibleItems.map((item) => item.id)).toEqual(['member-alpha']);
	expect(result.hiddenSelectionCount).toBe(1);
});

test('MEM-U-05: empty queue has no queued rows and every unqueued row', () => {
	expect(project('queued', []).visibleItems).toEqual([]);
	expect(project('queued', []).hiddenSelectionCount).toBe(2);
	expect(project('not-queued', []).visibleItems.map((item) => item.id)).toEqual([
		'member-zulu', 'member-alpha', 'member-twin', 'member-literal', 'member-fallback'
	]);
});

test('MEM-U-06: membership changes recompute hidden choices without clearing them', () => {
	const before = structuredClone(SELECTED);
	expect(project('queued', ['member-alpha']).hiddenSelectionCount).toBe(1);
	expect(project('queued', ['member-alpha', 'member-twin']).hiddenSelectionCount).toBe(0);
	expect(project('queued', ['member-twin']).visibleItems.map((item) => item.id)).toEqual(['member-twin']);
	expect(project('queued', []).hiddenSelectionCount).toBe(2);
	expect(SELECTED).toEqual(before);
});

test('MEM-U-07: reset membership alone retains Favorites and find', () => {
	const options = { view: 'favorites' as const, query: 'shared', order: 'title' as const };
	expect(project('queued', ['member-zulu'], options).visibleItems.map((item) => item.id))
		.toEqual(['member-zulu']);
	expect(project('all', ['member-zulu'], options).visibleItems.map((item) => item.id))
		.toEqual(['member-zulu', 'member-twin']);
});

test('MEM-U-08: projection preserves original objects and never mutates source sets', () => {
	const before = structuredClone(ITEMS);
	const favorites = [...FAVORITES];
	const selected = [...SELECTED];
	const result = project('not-queued', ['member-zulu'], { order: 'title' });
	expect(result.visibleItems.map((item) => item.id)).toEqual([
		'member-alpha', 'member-fallback', 'member-twin', 'member-literal'
	]);
	expect(result.visibleItems[0]).toBe(ITEMS[1]);
	expect(result.visibleItems[1]).toBe(ITEMS[4]);
	expect(ITEMS).toEqual(before);
	expect(FAVORITES).toEqual(favorites);
	expect(SELECTED).toEqual(selected);
});

test('MEM-U-09: punctuation is literal and an id-only row remains discoverable', () => {
	expect(project('not-queued', ['member-zulu'], { query: '.*' }).visibleItems.map((item) => item.id))
		.toEqual(['member-literal']);
	expect(project('queued', ['member-fallback'], { query: 'MEMBER-FALLBACK' }).visibleItems.map((item) => item.id))
		.toEqual(['member-fallback']);
});

test('MEM-U-10: refresh projects current loaded metadata and cannot resurrect missing rows', () => {
	const items = [
		{ id: 'member-alpha', title: 'Updated Song', artist: 'New Band' },
		{ id: 'member-fresh', title: 'Fresh Tune' }
	];
	expect(project('queued', ['member-alpha', 'member-zulu'], { items }).visibleItems)
		.toEqual([{ id: 'member-alpha', title: 'Updated Song', artist: 'New Band' }]);
	expect(project('not-queued', ['member-alpha', 'member-zulu'], { items }).visibleItems)
		.toEqual([{ id: 'member-fresh', title: 'Fresh Tune' }]);
});
