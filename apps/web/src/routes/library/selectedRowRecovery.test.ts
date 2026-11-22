import { expect, test } from 'vitest';
import { selectedRowRecoveryIds } from './selectedRowRecovery.js';

test('finds nearest surviving rows in display order, not selection order', () => {
	expect(selectedRowRecoveryIds(['a', 'skip', 'removed', 'skip2', 'b', 'c'], 'removed', ['c', 'a', 'b', 'hidden']))
		.toEqual({ nextId: 'b', previousId: 'a' });
});

test('handles missing, first, last and only removed rows', () => {
	expect(selectedRowRecoveryIds(['a', 'b'], 'missing', ['a', 'b']))
		.toEqual({ nextId: null, previousId: null });
	expect(selectedRowRecoveryIds(['a', 'b'], 'a', ['b']))
		.toEqual({ nextId: 'b', previousId: null });
	expect(selectedRowRecoveryIds(['a', 'b'], 'b', ['a']))
		.toEqual({ nextId: null, previousId: 'a' });
	expect(selectedRowRecoveryIds(['a'], 'a', []))
		.toEqual({ nextId: null, previousId: null });
});

test('returns no neighbors when the display is empty or survivors are hidden', () => {
	expect(selectedRowRecoveryIds([], 'removed', ['hidden']))
		.toEqual({ nextId: null, previousId: null });
	expect(selectedRowRecoveryIds(['a', 'removed', 'b'], 'removed', ['hidden']))
		.toEqual({ nextId: null, previousId: null });
});

test('finds the nearest neighbors of the first exact removed occurrence', () => {
	const displayed = ['earlier', 'Removed', 'skip', 'removed', 'next', 'removed', 'later'];
	const selected = ['later', 'earlier', 'next', 'Removed'];
	expect(selectedRowRecoveryIds(displayed, 'removed', selected))
		.toEqual({ nextId: 'next', previousId: 'Removed' });
});

test('uses the first removed occurrence and exact survivor membership without mutating inputs', () => {
	const displayed = ['A', 'removed', 'a', 'removed', 'A'];
	const selected = ['A', 'A', 'hidden'];
	expect(selectedRowRecoveryIds(displayed, 'removed', selected))
		.toEqual({ nextId: 'A', previousId: 'A' });
	expect(displayed).toEqual(['A', 'removed', 'a', 'removed', 'A']);
	expect(selected).toEqual(['A', 'A', 'hidden']);
});
