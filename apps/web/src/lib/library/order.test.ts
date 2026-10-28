import { describe, expect, it } from 'vitest';
import type { LibraryItem } from './cache.js';
import { displayArtist, displayTitle, orderLibraryItems } from './order.js';

const ids = (items: readonly LibraryItem[]) => items.map((item) => item.id);

describe('transient library ordering', () => {
	it('returns a separate array in Original mode, preserving objects and metadata', () => {
		const items: readonly LibraryItem[] = Object.freeze([
			Object.freeze({ id: 'z', title: 'Zulu', extra: { retained: true } }),
			Object.freeze({ id: 'a', title: 'Alpha' })
		]);
		const ordered = orderLibraryItems(items, 'original');
		expect(ordered).not.toBe(items);
		expect(ordered).toEqual(items);
		expect(ordered[0]).toBe(items[0]);
		ordered.reverse();
		expect(ids(items)).toEqual(['z', 'a']);
	});

	it('sorts display titles by lowercase code units, retaining API order for ties', () => {
		const items: readonly LibraryItem[] = Object.freeze([
			Object.freeze({ id: 'z', title: 'Zulu', artist: 'Alpha' }),
			Object.freeze({ id: 'tie-z', title: 'alpha', artist: 'Zulu' }),
			Object.freeze({ id: 'id-fallback' }),
			Object.freeze({ id: 'accent-z', title: 'Éclair' }),
			Object.freeze({ id: 'tie-a', title: 'ALPHA', artist: 'Alpha' }),
			Object.freeze({ id: 'accent-a', title: 'éclair' }),
			Object.freeze({ id: 'astral', title: '🎵 Music' })
		]);
		const before = JSON.stringify(items);
		expect(ids(orderLibraryItems(items, 'title'))).toEqual([
			'tie-z', 'tie-a', 'id-fallback', 'z', 'accent-z', 'accent-a', 'astral'
		]);
		expect(JSON.stringify(items)).toBe(before);
		expect(ids(orderLibraryItems(items, 'original'))).toEqual(ids(items));
	});

	it('compares trimmed artists, puts unnamed last and never adds a secondary key', () => {
		const items: readonly LibraryItem[] = Object.freeze([
			Object.freeze({ id: 'missing', title: 'A' }),
			Object.freeze({ id: 'tie-z', title: 'Zulu', artist: ' echo ' }),
			Object.freeze({ id: 'blank', artist: '\t ' }),
			Object.freeze({ id: 'accent-z', artist: 'Écho' }),
			Object.freeze({ id: 'alpha', artist: 'Alpha' }),
			Object.freeze({ id: 'tie-a', title: 'Alpha', artist: 'Echo' }),
			Object.freeze({ id: 'accent-a', artist: 'écho' })
		]);
		expect(ids(orderLibraryItems(items, 'artist'))).toEqual([
			'alpha', 'tie-z', 'tie-a', 'accent-z', 'accent-a', 'missing', 'blank'
		]);
		expect(items[1].artist).toBe(' echo ');
	});

	it('does not trim titles or normalize canonically equivalent Unicode', () => {
		const items = [
			{ id: 'composed', title: 'é' },
			{ id: 'plain', title: 'Alpha' },
			{ id: 'decomposed', title: 'é' },
			{ id: 'spaced', title: ' Zulu ' }
		];
		expect(ids(orderLibraryItems(items, 'title'))).toEqual([
			'spaced', 'plain', 'decomposed', 'composed'
		]);
		expect(items[2].title).toBe('é');
	});

	it('uses the same display fallback for blank/missing/invalid metadata', () => {
		for (const title of [undefined, '', '  ', null, 12]) {
			const item = { id: 'fallback', title } as LibraryItem;
			expect(displayTitle(item)).toBe('fallback');
			expect(ids(orderLibraryItems([item, { id: 'b', title: 'Bravo' }], 'title')))
				.toEqual(['b', 'fallback']);
		}
		for (const artist of [undefined, '', '\n\t', null, 12]) {
			const item = { id: 'unnamed', artist } as LibraryItem;
			expect(displayArtist(item)).toBeNull();
			expect(ids(orderLibraryItems([item, { id: 'a', artist: 'Zulu' }], 'artist')))
				.toEqual(['a', 'unnamed']);
		}
		expect(displayTitle({ id: 'a', title: ' Alpha ' })).toBe(' Alpha ');
		expect(displayArtist({ id: 'a', artist: ' Echo ' })).toBe(' Echo ');
	});

	it('handles empty, singleton and all-equal snapshots without mutation', () => {
		for (const mode of ['original', 'title', 'artist'] as const) {
			expect(orderLibraryItems([], mode)).toEqual([]);
			const item = { id: 'only' };
			expect(orderLibraryItems([item], mode)).toEqual([item]);
			const ties = [{ id: 'z', title: 'Same' }, { id: 'a', title: 'same' }];
			expect(ids(orderLibraryItems(ties, mode))).toEqual(['z', 'a']);
		}
	});
});
