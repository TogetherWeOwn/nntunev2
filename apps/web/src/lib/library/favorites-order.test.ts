import { describe, expect, it } from 'vitest';
import { moveFavoriteId } from './favorites-order.js';

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
