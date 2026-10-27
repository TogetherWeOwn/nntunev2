import { describe, expect, it } from 'vitest';
import type { LibraryItem } from './cache.js';
import { orderLibraryItems } from './order.js';

/**
 * Acceptance-owned. Builder implements the direction model without editing this
 * file or order.test.ts. These call the existing executable ordering seam; the
 * cast allows assertion-RED against the pre-direction union, not a compile RED.
 * New model values: title-desc / artist-desc. Existing values remain unchanged.
 * Breaks caught: reversed tie groups, unnamed-first descending, locale sorting,
 * metadata normalization, secondary keys, in-place mutation and lost identities.
 */
const order = (items: readonly LibraryItem[], mode: string) =>
	orderLibraryItems(items, mode as Parameters<typeof orderLibraryItems>[1]);
const ids = (items: readonly LibraryItem[]) => items.map((item) => item.id);

const ITEMS: readonly LibraryItem[] = Object.freeze([
	Object.freeze({ id: 'zulu', title: 'Zulu Song', artist: ' beta ' }),
	Object.freeze({ id: 'tie-z', title: 'alpha', artist: 'Zulu' }),
	Object.freeze({ id: 'id-fallback' }),
	Object.freeze({ id: 'accent-z', title: 'Éclair', artist: 'Écho' }),
	Object.freeze({ id: 'echo', title: 'Omega', artist: 'echo' }),
	Object.freeze({ id: 'tie-a', title: 'ALPHA', artist: 'Echo' }),
	Object.freeze({ id: 'blank', title: 'Beta Song', artist: '\t ' }),
	Object.freeze({ id: 'accent-a', title: 'éclair', artist: 'écho' }),
	Object.freeze({ id: 'astral', title: '🎵 Music', artist: 'Alpha' })
]);

// Hand-checked UTF-16 order. Tied keys deliberately oppose id/secondary order.
const MODES = [
	{ mode: 'original', want: ['zulu', 'tie-z', 'id-fallback', 'accent-z', 'echo', 'tie-a', 'blank', 'accent-a', 'astral'] },
	{ mode: 'title', want: ['tie-z', 'tie-a', 'blank', 'id-fallback', 'echo', 'zulu', 'accent-z', 'accent-a', 'astral'] },
	{ mode: 'artist', want: ['astral', 'zulu', 'echo', 'tie-a', 'tie-z', 'accent-z', 'accent-a', 'id-fallback', 'blank'] },
	{ mode: 'title-desc', want: ['astral', 'accent-z', 'accent-a', 'zulu', 'echo', 'id-fallback', 'blank', 'tie-z', 'tie-a'] },
	{ mode: 'artist-desc', want: ['accent-z', 'accent-a', 'tie-z', 'echo', 'tie-a', 'zulu', 'astral', 'id-fallback', 'blank'] }
] as const;

describe('loaded-library ordering direction acceptance', () => {
	it.each(MODES)('DORD-U01 $mode: ordered copy, untouched snapshot and original row identities', ({ mode, want }) => {
		const before = JSON.stringify(ITEMS);
		const result = order(ITEMS, mode);
		expect(ids(result)).toEqual(want);
		expect(result).not.toBe(ITEMS);
		for (const item of result) expect(item).toBe(ITEMS.find((source) => source.id === item.id));
		result.pop();
		expect(JSON.stringify(ITEMS)).toBe(before);
		expect(ids(order(ITEMS, 'original'))).toEqual([
			'zulu', 'tie-z', 'id-fallback', 'accent-z', 'echo', 'tie-a', 'blank', 'accent-a', 'astral'
		]);
	});

	it.each(['title-desc', 'artist-desc'])('DORD-U02 %s: equal keys keep API order, never reverse a tie group', (mode) => {
		const items = [
			{ id: 'first-z', title: 'same', artist: ' Echo ' },
			{ id: 'second-a', title: 'SAME', artist: 'echo' },
			{ id: 'third-m', title: 'Same', artist: 'ECHO' }
		];
		expect(ids(order(items, mode))).toEqual(['first-z', 'second-a', 'third-m']);
	});

	it('DORD-U03 artist-desc: all invalid/blank artists remain last in API order', () => {
		const items = [
			{ id: 'missing' },
			{ id: 'alpha', artist: 'Alpha' },
			{ id: 'blank', artist: ' \t\n' },
			{ id: 'null', artist: null },
			{ id: 'zulu', artist: ' Zulu ' },
			{ id: 'numeric', artist: 12 },
			{ id: 'empty', artist: '' }
		] as LibraryItem[];
		expect(ids(order(items, 'artist-desc'))).toEqual([
			'zulu', 'alpha', 'missing', 'blank', 'null', 'numeric', 'empty'
		]);
	});

	it('DORD-U04 title-desc: missing/blank/invalid titles use the existing display ID fallback', () => {
		const items = [
			{ id: 'm-missing' },
			{ id: 'z-blank', title: '   ' },
			{ id: 'b-empty', title: '' },
			{ id: 'y-null', title: null },
			{ id: 'x-number', title: 12 },
			{ id: 'named', title: 'Charlie' }
		] as LibraryItem[];
		expect(ids(order(items, 'title-desc'))).toEqual([
			'z-blank', 'y-null', 'x-number', 'm-missing', 'named', 'b-empty'
		]);
	});

	it.each(['title-desc', 'artist-desc'])('DORD-U05 %s: lowercase UTF-16, not locale or Unicode normalization', (mode) => {
		const items = [
			{ id: 'composed', title: 'é', artist: 'é' },
			{ id: 'plain', title: 'Zulu', artist: 'Zulu' },
			{ id: 'decomposed', title: 'é', artist: 'é' },
			{ id: 'astral', title: '🎵', artist: '🎵' },
			{ id: 'upper', title: 'É', artist: 'É' }
		];
		expect(ids(order(items, mode))).toEqual(['astral', 'composed', 'upper', 'plain', 'decomposed']);
		expect(items[2]).toEqual({ id: 'decomposed', title: 'é', artist: 'é' });
	});

	it('DORD-U06 title-desc: surrounding title whitespace remains part of the comparison key', () => {
		expect(ids(order([
			{ id: 'spaced', title: ' Zulu ' },
			{ id: 'alpha', title: 'Alpha' },
			{ id: 'zulu', title: 'Zulu' }
		], 'title-desc'))).toEqual(['zulu', 'alpha', 'spaced']);
	});

	it.each(MODES)('DORD-U07 $mode: empty and singleton inputs still return independent arrays', ({ mode }) => {
		const empty: readonly LibraryItem[] = Object.freeze([]);
		const single: readonly LibraryItem[] = Object.freeze([Object.freeze({ id: 'only' })]);
		const emptyResult = order(empty, mode);
		const singleResult = order(single, mode);
		expect(emptyResult).toEqual([]);
		expect(emptyResult).not.toBe(empty);
		expect(singleResult).toEqual([{ id: 'only' }]);
		expect(singleResult).not.toBe(single);
		expect(singleResult[0]).toBe(single[0]);
	});
});
