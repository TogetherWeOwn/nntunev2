import { expect, test } from 'vitest';
import ts from 'typescript';
import type { LibraryItem } from '#lib/library/cache.js';
import { selectFavoriteItems } from '#lib/library/favorites.js';
import { filterLibraryItems } from '#lib/library/filter.js';
import { orderLibraryItems, type LibraryOrder } from '#lib/library/order.js';
import { hiddenSelectedCount } from '#lib/library/selection.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

// Execute the route's real projection declarations with plain derived cells.
// No copied composition expression, DOM, storage, fetch or queue substitutes.
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
expect(declarations).toHaveLength(names.length);
const compiled = ts.transpileModule(
	declarations.map((statement) => statement.getText(parsed)).join('\n'),
	{ compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }
).outputText;
const evaluate = new Function(
	'items', 'order', 'favorites', 'view', 'findQuery', 'selectedIds',
	'orderLibraryItems', 'selectFavoriteItems', 'filterLibraryItems', 'hiddenSelectedCount', '$derived',
	`${compiled}\nreturn { visibleItems, hiddenSelectionCount };`
);

const ITEMS = [
	{ id: 'composition-zulu', title: 'Zulu Tune', artist: 'Common Band' },
	{ id: 'composition-beta', title: 'Beta Song', artist: 'Common Band' },
	{ id: 'composition-private', title: 'Private Previous Account Text', artist: 'Other Band' }
];
const FAVORITES = ['composition-zulu', 'composition-beta'];
const SELECTED = ['composition-beta', 'composition-private'];

function project(view: 'all' | 'favorites', query: string, order: LibraryOrder = 'original') {
	return evaluate(
		ITEMS, order, FAVORITES, view, query, SELECTED,
		orderLibraryItems, selectFavoriteItems, filterLibraryItems, hiddenSelectedCount,
		<T>(value: T) => value
	) as { visibleItems: LibraryItem[]; hiddenSelectionCount: number };
}

test('find cannot reveal a non-favorite through the Favorites-only view', () => {
	const result = project('favorites', 'private');
	expect(result.visibleItems).toEqual([]);
	expect(result.hiddenSelectionCount).toBe(2);
	expect(project('all', 'private').visibleItems.map((item) => item.id))
		.toEqual(['composition-private']);
});

test('clearing either presentation filter keeps the other and counts all hidden choices', () => {
	const combined = project('favorites', 'beta');
	expect(combined.visibleItems.map((item) => item.id)).toEqual(['composition-beta']);
	expect(combined.hiddenSelectionCount).toBe(1);
	const clearedFind = project('favorites', '');
	expect(clearedFind.visibleItems.map((item) => item.id)).toEqual(FAVORITES);
	expect(clearedFind.hiddenSelectionCount).toBe(1);
	expect(project('all', 'beta').visibleItems.map((item) => item.id)).toEqual(['composition-beta']);
	expect(project('all', '').hiddenSelectionCount).toBe(0);
	expect(SELECTED).toEqual(['composition-beta', 'composition-private']);
	expect(FAVORITES).toEqual(['composition-zulu', 'composition-beta']);
});

test('the composed projection preserves canonical order, identities and immutable input', () => {
	const before = structuredClone(ITEMS);
	const result = project('favorites', ' common band ', 'title');
	expect(result.visibleItems.map((item) => item.id))
		.toEqual(['composition-beta', 'composition-zulu']);
	expect(result.visibleItems[0]).toBe(ITEMS[1]);
	expect(result.visibleItems[1]).toBe(ITEMS[0]);
	expect(ITEMS).toEqual(before);
});
