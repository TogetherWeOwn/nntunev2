import { describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import * as queue from './queue.js';
import type { QueueState, Track } from './queue.js';

/**
 * Acceptance-owned: shuffle only the upcoming suffix of the explicit queue.
 * Builder continues this test-only branch; do not edit these assertions to get GREEN.
 *
 * Run from repo root:
 * pnpm --dir apps/web test src/lib/player/queue.shuffle-upcoming.test.ts --reporter=json
 * Browser companion: apps/web/tests/shuffle-upcoming.spec.ts (execution/handoff note).
 *
 * Seam: export shuffleUpcoming(state: QueueState, random?: () => number): QueueState
 * from the existing queue module. Namespace lookup keeps the pre-feature module
 * loadable: baseline RED is an assertion, never an absent named-import/compile error.
 * Shared-panel seam: PlayerQueue accepts onshuffle?: () => void and
 * radioActive?: boolean (default false), following its existing onnext callback
 * convention. U07 gives it THREE upcoming entries, so radio disablement cannot
 * accidentally pass just because the explicit queue is already at its end.
 * No fallback shuffle, production stub, backend fixture, or source-text assertion.
 *
 * Breaks caught: shuffling the prefix/current; mutating tracks; losing/duplicating
 * entries; biased bucket selection; rejecting identity permutations; changing
 * serialization, manual edits or clamped navigation. Five synthetic stable IDs
 * cover a played prefix AND at least two upcoming entries without changing DEMO_TRACKS.
 */

type ShuffleUpcoming = (state: QueueState, random?: () => number) => QueueState;

function shuffle(state: QueueState, random: () => number): QueueState {
	const operation: unknown = Reflect.get(queue, 'shuffleUpcoming');
	expect(operation, 'UP-SHUF: queue exposes the one-shot shuffleUpcoming operation').toBeTypeOf('function');
	return (operation as ShuffleUpcoming)(state, random);
}

const TRACKS: Track[] = ['mango', 'apple', 'zebra', 'birch', 'cobalt'].map((id) => ({
	id,
	title: `Synthetic ${id}`,
	artist: 'Shuffle Acceptance Band',
	streamUrl: `/api/demo/audio?track=${id}`,
	artworkUrl: null
}));

function frozenQueue(size: number, index = 0): QueueState {
	const state = queue.selectTrack(queue.createQueue(TRACKS.slice(0, size)), index);
	for (const track of state.tracks) Object.freeze(track);
	Object.freeze(state.tracks);
	return Object.freeze(state);
}

function ids(state: QueueState): string[] {
	return state.tracks.map((track) => track.id);
}

describe('shuffle upcoming acceptance', () => {
	for (const { size, index } of [
		{ size: 0, index: 0 },
		{ size: 1, index: 0 },
		{ size: 2, index: 0 },
		{ size: 2, index: 1 },
		{ size: 5, index: 3 },
		{ size: 5, index: 4 }
	]) {
		it(`UP-SHUF-U01 size=${size} current=${index}: fewer than two upcoming is a no-op`, () => {
			const state = frozenQueue(size, index);
			const random = vi.fn(() => 0.25);
			expect(shuffle(state, random)).toEqual(state);
			expect(random).not.toHaveBeenCalled();
		});
	}

	for (const index of [0, 1, 2]) {
		for (const value of [0, 0.2, 0.5, 1 - Number.EPSILON]) {
			it(`UP-SHUF-U02 current=${index} rng=${value}: fixed prefix/current, exact suffix permutation, immutable input`, () => {
				const state = frozenQueue(5, index);
				const before = JSON.stringify(state);
				const result = shuffle(state, () => value);
				expect(result.currentIndex).toBe(index);
				expect(queue.currentTrack(result)).toBe(state.tracks[index]);
				expect(result.tracks.slice(0, index + 1)).toEqual(state.tracks.slice(0, index + 1));
				expect(ids(result).slice(index + 1).sort()).toEqual(ids(state).slice(index + 1).sort());
				expect(result.tracks).toHaveLength(5);
				for (let prefix = 0; prefix <= index; prefix += 1) {
					expect(result.tracks[prefix]).toBe(state.tracks[prefix]);
				}
				for (const track of result.tracks) expect(state.tracks).toContain(track);
				expect(JSON.stringify(state)).toBe(before);
			});
		}
	}

	it('UP-SHUF-U03: all six three-upcoming permutations have equal bucket weight, including unchanged order', () => {
		const state = frozenQueue(4);
		const counts = new Map<string, number>();
		// Six equal bins divide the 3-choice and 2-choice draws exactly. Allow
		// a draw for the final singleton too; neither traversal direction nor
		// exact output for one RNG sequence is pinned. This is an exhaustive
		// 216-vector grid, not a flaky random-frequency experiment.
		for (let first = 0; first < 6; first += 1) {
			for (let second = 0; second < 6; second += 1) {
				for (let third = 0; third < 6; third += 1) {
					const draws = [(first + 0.5) / 6, (second + 0.5) / 6, (third + 0.5) / 6];
					const random = vi.fn(() => {
						const draw = draws.shift();
						expect(draw, 'bounded three-item permutation exhausted its random vector').toBeDefined();
						return draw!;
					});
					const result = shuffle(state, random);
					expect(result.currentIndex).toBe(0);
					expect(result.tracks[0]).toBe(state.tracks[0]);
					const key = ids(result).slice(1).join(',');
					counts.set(key, (counts.get(key) ?? 0) + 1);
				}
			}
		}
		expect(Object.fromEntries(counts)).toEqual({
			'apple,zebra,birch': 36,
			'apple,birch,zebra': 36,
			'zebra,apple,birch': 36,
			'zebra,birch,apple': 36,
			'birch,apple,zebra': 36,
			'birch,zebra,apple': 36
		});
	});

	it('UP-SHUF-U04: repeated one-shot operations preserve identity and the suffix multiset', () => {
		let state = frozenQueue(5, 1);
		for (const value of [0, 0.8, 0.5, 0.01, 0.99]) {
			state = shuffle(state, () => value);
			expect(state.currentIndex).toBe(1);
			expect(ids(state).slice(0, 2)).toEqual(['mango', 'apple']);
			expect(ids(state).slice(2).sort()).toEqual(['birch', 'cobalt', 'zebra']);
		}
	});

	it('UP-SHUF-U05: existing serialization stores only order/current ID and restores that exact result', () => {
		const state = shuffle(frozenQueue(5, 1), () => 0);
		const raw = queue.serializeQueue(state);
		expect(JSON.parse(raw)).toEqual({ orderIds: ids(state), currentId: 'apple' });
		const restored = queue.restoreQueue(raw, TRACKS);
		expect(ids(restored)).toEqual(ids(state));
		expect(restored.currentIndex).toBe(1);
		expect(queue.currentTrack(restored)?.id).toBe('apple');
	});

	for (const radioActive of [false, true]) {
		it(`UP-SHUF-U07 radio=${radioActive}: shared panel disables shuffle for radio even with three upcoming entries`, () => {
			const props = { state: frozenQueue(5, 1), onshuffle: () => {}, radioActive };
			const html = render(PlayerQueue, { props }).body;
			// Inspect actual rendered native buttons, never component source text.
			const buttons = [...html.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].filter(
				(match) => {
					const label = [...match[2]!.matchAll(/<!--[\s\S]*?-->|<[^>]*>|([^<]+)/g)]
						.map((token) => token[1] ?? '')
						.join('');
					return /aria-label=["']Shuffle upcoming["']/i.test(match[1]!) ||
						/Shuffle\s+upcoming/i.test(label);
				}
			);
			expect(buttons, 'shared player exposes the labeled one-shot shuffle button').toHaveLength(1);
			expect(/\bdisabled(?:\s|=|$)/.test(buttons[0]![1]!)).toBe(radioActive);
		});
	}

	it('UP-SHUF-U06: manual moves still follow current identity, and Next/Previous still clamp', () => {
		const shuffled = shuffle(frozenQueue(5, 1), () => 0);
		const moved = queue.moveTrack(shuffled, 0, 4);
		expect(queue.currentTrack(moved)).toBe(TRACKS[1]);
		expect(moved.currentIndex).toBe(0);
		expect(ids(moved)).toEqual([...ids(shuffled).slice(1), 'mango']);
		expect(queue.previousTrack(moved)).toEqual(moved);
		let last = moved;
		for (let step = 0; step < 4; step += 1) last = queue.nextTrack(last);
		expect(last.currentIndex).toBe(4);
		expect(queue.nextTrack(last)).toEqual(last);
		expect(queue.currentTrack(last)?.id).toBe('mango');
	});
});
