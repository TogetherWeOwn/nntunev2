import { expect, test } from 'vitest';
import { projectInteractionIds, survivingInteractionIds } from './interactionSelection.js';

const rows = (ids: string[]) => ids.map((id) => ({ id }));

test('selected interactions follow rendered order, omit hidden ids and deduplicate occurrences', () => {
	expect(projectInteractionIds('a', ['hidden', 'a', 'b'], rows(['b', 'a', 'b', 'other'])))
		.toEqual(['b', 'a']);
	expect(projectInteractionIds('other', ['a', 'b'], rows(['b', 'a', 'other'])))
		.toEqual(['other']);
	expect(projectInteractionIds('hidden', ['hidden'], rows(['a']))).toEqual([]);
});

test('deferred interactions retain requested order and only surviving unique ids', () => {
	expect(survivingInteractionIds(['b', 'missing', 'a', 'b'], rows(['a', 'b', 'b']), false))
		.toEqual(['b', 'a']);
	expect(survivingInteractionIds(['missing'], rows(['a']), false)).toEqual([]);
	expect(survivingInteractionIds([], rows(['a']), false)).toEqual([]);
});

test('loading or unavailable library refuses actions even for present ids', () => {
	expect(survivingInteractionIds(['a'], rows(['a']), true)).toEqual([]);
});
