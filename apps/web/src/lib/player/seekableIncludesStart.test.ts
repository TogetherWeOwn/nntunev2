import { describe, expect, it } from 'vitest';
import { seekableIncludesStart } from './seekableIncludesStart.js';

function audioWithRanges(bounds: [number, number][]): HTMLAudioElement {
	return {
		seekable: {
			length: bounds.length,
			start: (index: number) => bounds[index][0],
			end: (index: number) => bounds[index][1]
		}
	} as HTMLAudioElement;
}

describe('seekableIncludesStart', () => {
	it('accepts finite ranges containing zero, including either boundary', () => {
		for (const bounds of [[-1, 1], [0, 5], [-5, 0], [0, 0]] as [number, number][]) {
			expect(seekableIncludesStart(audioWithRanges([bounds]))).toBe(true);
		}
		expect(seekableIncludesStart(audioWithRanges([[1, 2], [0, 3]]))).toBe(true);
	});

	it('rejects empty, non-containing and non-finite ranges', () => {
		for (const bounds of [[], [[1, 2]], [[-2, -1]], [[NaN, 1]], [[-1, Infinity]], [[-Infinity, 1]]] as [number, number][][]) {
			expect(seekableIncludesStart(audioWithRanges(bounds))).toBe(false);
		}
	});

	it('fails closed when the range set or either bound cannot be read', () => {
		const fail = () => { throw new Error('unreadable'); };
		for (const element of [
			{ get seekable() { return fail(); } },
			{ seekable: { get length() { return fail(); } } },
			{ seekable: { length: 1, start: fail, end: () => 1 } },
			{ seekable: { length: 1, start: () => 0, end: fail } }
		]) {
			expect(seekableIncludesStart(element as unknown as HTMLAudioElement)).toBe(false);
		}
	});
});
