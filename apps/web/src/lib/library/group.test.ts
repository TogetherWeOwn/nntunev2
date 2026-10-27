import { describe, expect, test } from 'vitest';
import type { LibraryItem } from './cache.js';
import { groupLibraryItems } from './group.js';
import { filterLibraryItems } from './filter.js';
import { orderLibraryItems } from './order.js';
import { selectFavoriteItems } from './favorites.js';

const items: LibraryItem[] = [
	{ id: 'z1', title: 'Zebra', artist: ' Zulu ' },
	{ id: 'unknown', title: 'Morning' },
	{ id: 'e1', title: 'Evening', artist: 'Echo' },
	{ id: 'accent', title: 'Song', artist: 'Écho' },
	{ id: 'z2', title: 'Aardvark', artist: 'zulu' },
	{ id: 'a', title: 'Alpha', artist: 'Alpha' },
	{ id: 'blank', title: 'Night', artist: ' \t ' },
	{ id: 'e2', title: 'Dawn', artist: ' echo ' }
];
const partition = (input: readonly LibraryItem[]) =>
	groupLibraryItems(input).map(({ key, items }) => [key, items.map(({ id }) => id)]);

describe('artist grouping presentation', () => {
	test('case-folds and trims artist keys, sorts by UTF-16, and keeps row order', () => {
		expect(partition(items)).toEqual([
			['alpha', ['a']], ['echo', ['e1', 'e2']], ['zulu', ['z1', 'z2']],
			['écho', ['accent']], [null, ['unknown', 'blank']]
		]);
		expect(groupLibraryItems(items).map(({ label }) => label)).toEqual([
			'Alpha', 'Echo', 'Zulu', 'Écho', 'Unknown artist'
		]);
	});

	test('preserves immutable inputs and original track objects without adding metadata', () => {
		const frozen = Object.freeze(items.map((track) => Object.freeze({ ...track })));
		const before = JSON.stringify(frozen);
		const result = groupLibraryItems(frozen);
		expect(JSON.stringify(frozen)).toBe(before);
		expect(result.at(-1)?.items[0]).toBe(frozen[1]);
		expect(result.at(-1)?.items[0].artist).toBeUndefined();
		result[0].items.pop();
		expect(frozen).toHaveLength(8);
	});

	test('keeps a genuinely named Unknown artist separate from missing and malformed artists', () => {
		const malformed = { id: 'bad', artist: 42 } as unknown as LibraryItem;
		expect(partition([{ id: 'named', artist: 'Unknown artist' }, malformed])).toEqual([
			['unknown artist', ['named']], [null, ['bad']]
		]);
		expect(groupLibraryItems([])).toEqual([]);
	});

	test('composes after descending ordering and find without reordering within sections', () => {
		const ordered = orderLibraryItems(items, 'title-desc');
		expect(partition(ordered)).toEqual([
			['alpha', ['a']], ['echo', ['e1', 'e2']], ['zulu', ['z1', 'z2']],
			['écho', ['accent']], [null, ['blank', 'unknown']]
		]);
		expect(partition(filterLibraryItems(ordered, 'zulu', 'artist'))).toEqual([
			['zulu', ['z1', 'z2']]
		]);
		expect(partition(filterLibraryItems(items, 'no matches'))).toEqual([]);
	});

	test('groups only the favorites or selected subset supplied by the view', () => {
		expect(partition(selectFavoriteItems(items, ['z2', 'a', 'blank']))).toEqual([
			['alpha', ['a']], ['zulu', ['z2']], [null, ['blank']]
		]);
		const selected = new Set(['e2', 'unknown']);
		expect(partition(items.filter(({ id }) => selected.has(id)))).toEqual([
			['echo', ['e2']], [null, ['unknown']]
		]);
	});
});
