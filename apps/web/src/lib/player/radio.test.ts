import { describe, expect, it } from 'vitest';
import { createRadioOrder, DEFAULT_RADIO_SEED, parseRadioSeed } from './radio.js';
import { DEMO_TRACKS, type Track } from './queue.js';

function fixtureTracks(count: number): Track[] {
	return Array.from({ length: count }, (_, i) => ({
		id: `track-${i}`,
		title: `Track ${i}`,
		artist: 'Fixture Band',
		streamUrl: `/api/demo/audio?track=track-${i}`
	}));
}

describe('parseRadioSeed', () => {
	it('parses an explicit integer seed', () => {
		expect(parseRadioSeed('7')).toBe(7);
		expect(parseRadioSeed('0')).toBe(0);
		expect(parseRadioSeed('-3')).toBe(-3);
	});

	it('falls back to the default seed when absent or unparseable', () => {
		expect(parseRadioSeed(null)).toBe(DEFAULT_RADIO_SEED);
		expect(parseRadioSeed('')).toBe(DEFAULT_RADIO_SEED);
		expect(parseRadioSeed('seed!')).toBe(DEFAULT_RADIO_SEED);
	});
});

describe('createRadioOrder', () => {
	it('returns every track exactly once (a permutation, no repeats)', () => {
		for (const seed of [0, 1, 7, 11, 123456]) {
			const order = createRadioOrder(DEMO_TRACKS, seed);
			expect(order).toHaveLength(DEMO_TRACKS.length);
			expect([...order.map((t) => t.id)].sort()).toEqual(
				[...DEMO_TRACKS.map((t) => t.id)].sort()
			);
		}
	});

	it('is deterministic: same seed and library give the same order', () => {
		const library = fixtureTracks(12);
		const first = createRadioOrder(library, 11).map((t) => t.id);
		const second = createRadioOrder(library, 11).map((t) => t.id);
		const third = createRadioOrder([...library], 11).map((t) => t.id);
		expect(second).toEqual(first);
		expect(third).toEqual(first);
	});

	it('shuffles rather than echoing the library order', () => {
		// A fixed seed pins one known-good shuffle; if the algorithm changes,
		// this fails loudly instead of silently reordering everyone's radio.
		const library = fixtureTracks(12);
		const order = createRadioOrder(library, 11).map((t) => t.id);
		expect(order).not.toEqual(library.map((t) => t.id));
		expect(createRadioOrder(library, 11).map((t) => t.id)).toEqual(order);
	});

	it('does not mutate the input library', () => {
		const library = fixtureTracks(8);
		const snapshot = library.map((t) => t.id);
		createRadioOrder(library, 7);
		expect(library.map((t) => t.id)).toEqual(snapshot);
	});

	it('handles edge-case libraries', () => {
		expect(createRadioOrder([], 7)).toEqual([]);
		const [only] = fixtureTracks(1);
		expect(createRadioOrder([only!], 7).map((t) => t.id)).toEqual([only!.id]);
	});
});
