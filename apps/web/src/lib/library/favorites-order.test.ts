import { describe, expect, it } from 'vitest';
import { moveFavoriteId, restoreFavoriteId, type FavoriteRestoreAnchor } from './favorites-order.js';

/**
 * Builder-owned direct pure-move guards for the seam the browser
 * acceptance spec exercises through the UI. The protected
 * favorites-arrangement specs own the UI/storage contract; these pin the
 * movement algorithm itself: slot swaps, no-op identity, immutability
 * and membership preservation.
 */
describe('moveFavoriteId', () => {
	it('returns the same order for empty and singleton lists', () => {
		const empty: readonly string[] = [];
		expect(moveFavoriteId(empty, [], 'missing', 'up')).toBe(empty);
		const singleton = ['arr-m'];
		expect(moveFavoriteId(singleton, ['arr-m'], 'arr-m', 'up')).toBe(singleton);
		expect(moveFavoriteId(singleton, ['arr-m'], 'arr-m', 'down')).toBe(singleton);
	});

	it('returns the same order for boundary activations', () => {
		const stored = ['arr-z', 'arr-a', 'arr-m'];
		expect(moveFavoriteId(stored, stored, 'arr-z', 'up')).toBe(stored);
		expect(moveFavoriteId(stored, stored, 'arr-m', 'down')).toBe(stored);
	});

	it('returns the same order for stale or unknown ids', () => {
		const stored = ['arr-z', 'hidden', 'arr-a', 'arr-m'];
		const visible = ['arr-z', 'arr-a', 'arr-m'];
		expect(moveFavoriteId(stored, visible, 'hidden', 'up')).toBe(stored);
		expect(moveFavoriteId(stored, visible, 'hidden', 'down')).toBe(stored);
		expect(moveFavoriteId(stored, visible, 'gone', 'up')).toBe(stored);
		expect(moveFavoriteId(stored, visible, 'arr-a', 'up')).not.toBe(stored);
	});

	it('swaps visible neighbors in their complete-array slots without displacing hidden ids', () => {
		const stored = ['gone-first', 'arr-z', 'gone-between', 'arr-a', 'arr-m', 'gone-last'];
		const visible = ['arr-z', 'arr-a', 'arr-m'];
		expect(moveFavoriteId(stored, visible, 'arr-a', 'up')).toEqual([
			'gone-first',
			'arr-a',
			'gone-between',
			'arr-z',
			'arr-m',
			'gone-last'
		]);
		expect(moveFavoriteId(stored, visible, 'arr-m', 'up')).toEqual([
			'gone-first',
			'arr-z',
			'gone-between',
			'arr-m',
			'arr-a',
			'gone-last'
		]);
		expect(moveFavoriteId(stored, visible, 'arr-z', 'down')).toEqual([
			'gone-first',
			'arr-a',
			'gone-between',
			'arr-z',
			'arr-m',
			'gone-last'
		]);
	});

	it('swaps surviving neighbors across refreshed-away ids without moving them', () => {
		const stored = ['arr-z', 'hidden', 'arr-a', 'arr-m'];
		expect(moveFavoriteId(stored, ['arr-z', 'arr-m'], 'arr-m', 'up')).toEqual([
			'arr-m',
			'hidden',
			'arr-a',
			'arr-z'
		]);
	});

describe('restoreFavoriteId', () => {
	// Two adjacent removals undone in either order must restore the exact
	// original order, so queuing favorites keeps star order: anchors prefer
	// the surviving successor (the stable right edge after removals shift
	// later elements left), then the predecessor, then the clamped index.
	const anchor = (list: readonly string[], id: string): FavoriteRestoreAnchor => {
		const previousIndex = list.indexOf(id);
		return {
			previousIndex,
			predecessor: previousIndex > 0 ? list[previousIndex - 1] : null,
			successor: previousIndex >= 0 && previousIndex < list.length - 1 ? list[previousIndex + 1] : null
		};
	};

	it('restores two adjacent removals undone in removal order', () => {
		const start = ['A', 'B', 'C', 'D'];
		const undoB = anchor(start, 'B');
		const afterB = start.filter((id) => id !== 'B');
		const undoC = anchor(afterB, 'C');
		const afterBoth = afterB.filter((id) => id !== 'C');
		expect(afterBoth).toEqual(['A', 'D']);
		const restoredB = restoreFavoriteId(afterBoth, 'B', undoB);
		expect(restoredB).toEqual(['A', 'B', 'D']);
		expect(restoreFavoriteId(restoredB, 'C', undoC)).toEqual(['A', 'B', 'C', 'D']);
	});

	it('restores two adjacent removals undone in reverse order', () => {
		const start = ['A', 'B', 'C', 'D'];
		const undoB = anchor(start, 'B');
		const afterB = start.filter((id) => id !== 'B');
		const undoC = anchor(afterB, 'C');
		const afterBoth = afterB.filter((id) => id !== 'C');
		const restoredC = restoreFavoriteId(afterBoth, 'C', undoC);
		expect(restoredC).toEqual(['A', 'C', 'D']);
		expect(restoreFavoriteId(restoredC, 'B', undoB)).toEqual(['A', 'B', 'C', 'D']);
	});

	it('is idempotent when the id is already present and never mutates inputs', () => {
		const stored = Object.freeze(['A', 'B', 'C']);
		const next = restoreFavoriteId(stored, 'B', {
			previousIndex: 1,
			predecessor: 'A',
			successor: 'C'
		});
		expect(next).toBe(stored);
		expect(stored).toEqual(['A', 'B', 'C']);
	});

	it('falls back to the clamped index when both neighbors are gone', () => {
		expect(
			restoreFavoriteId(['X'], 'B', { previousIndex: 1, predecessor: null, successor: null })
		).toEqual(['X', 'B']);
		expect(
			restoreFavoriteId([], 'A', { previousIndex: 0, predecessor: null, successor: null })
		).toEqual(['A']);
	});
});

describe('moveFavoriteId edge cases', () => {
	it('never mutates frozen inputs and preserves exact membership', () => {
		const stored = Object.freeze(['gone-first', 'arr-z', 'gone-between', 'arr-a', 'arr-m']);
		const visible = Object.freeze(['arr-z', 'arr-a', 'arr-m']);
		const next = moveFavoriteId(stored, visible, 'arr-a', 'up');
		expect(next).toEqual(['gone-first', 'arr-a', 'gone-between', 'arr-z', 'arr-m']);
		expect(stored).toEqual(['gone-first', 'arr-z', 'gone-between', 'arr-a', 'arr-m']);
		expect(visible).toEqual(['arr-z', 'arr-a', 'arr-m']);
		expect([...next].sort()).toEqual([...stored].sort());
	});
});
});
