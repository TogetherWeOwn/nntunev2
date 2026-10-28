import { describe, expect, it } from 'vitest';
import * as queue from './queue.js';
import type { QueueState, Track } from './queue.js';

/**
 * TRIM — independent, protected acceptance. No implementation accompanies this
 * test bank. Carry this file and tests/library-queue-trim.spec.ts plus its
 * fixtures/queue-trim/* unchanged into the combined implementation PR. Return
 * incorrect tests to their author, never silently weaken them.
 *
 * Proposed public seam: keepCurrentAndUpcoming(state), clearUpcoming(state).
 * Both are immutable QueueState -> QueueState transitions, not transport.
 * Namespace lookup lets the real, existing module import and execute normally
 * before a precise missing-capability ASSERTION; it supplies no fallback/stub.
 * Once that assertion passes every case exercises the production transition.
 *
 * Breaks caught: inclusive/exclusive off-by-one trims, sorting instead of
 * retaining arranged order, retaining a stale index, cloning the current
 * track, mutating input, clamping invalid selections, and non-idempotent edits.
 * Expectations are hand-derived, never computed with the transition under test.
 *
 * Run from repo root:
 *   pnpm --dir apps/web test src/lib/player/queue.trim.acceptance.test.ts
 *   pnpm --dir apps/web test
 *   pnpm --dir apps/web check
 * Browser commands and the callback/focus contract live in the companion spec.
 */

type TrimName = 'keepCurrentAndUpcoming' | 'clearUpcoming';
type Transition = (state: QueueState) => QueueState;

function transition(name: TrimName): Transition {
	const candidate: unknown = Reflect.get(queue, name);
	expect(candidate, `TRIM requires production queue.${name}(state)`).toBeTypeOf('function');
	return candidate as Transition;
}

const TRACKS: Track[] = [
	{ id: 'trim-zulu', title: 'Zulu', artist: 'Trim Band', streamUrl: '/test-audio/zulu', artworkUrl: null },
	{ id: 'trim-apple', title: 'Apple', artist: 'Trim Band', streamUrl: '/test-audio/apple', artworkUrl: null },
	{ id: 'trim-mango', title: 'Mango', artist: 'Trim Band', streamUrl: '/test-audio/mango', artworkUrl: null },
	{ id: 'trim-kiwi', title: 'Kiwi', artist: 'Trim Band', streamUrl: '/test-audio/kiwi', artworkUrl: null }
];

function frozen(tracks: Track[], currentIndex: number): QueueState {
	tracks.forEach(Object.freeze);
	const value = { tracks: [...tracks], currentIndex };
	Object.freeze(value.tracks);
	return Object.freeze(value);
}

function ids(value: QueueState): string[] {
	return value.tracks.map((track) => track.id);
}

const EXAMPLES: {
	id: string; name: TrimName; index: number; want: string[]; wantIndex: number;
}[] = [
	{ id: 'TRIM-U01', name: 'keepCurrentAndUpcoming', index: 0, want: ['trim-zulu', 'trim-apple', 'trim-mango', 'trim-kiwi'], wantIndex: 0 },
	{ id: 'TRIM-U02', name: 'keepCurrentAndUpcoming', index: 2, want: ['trim-mango', 'trim-kiwi'], wantIndex: 0 },
	{ id: 'TRIM-U03', name: 'keepCurrentAndUpcoming', index: 3, want: ['trim-kiwi'], wantIndex: 0 },
	{ id: 'TRIM-U04', name: 'clearUpcoming', index: 0, want: ['trim-zulu'], wantIndex: 0 },
	{ id: 'TRIM-U05', name: 'clearUpcoming', index: 1, want: ['trim-zulu', 'trim-apple'], wantIndex: 1 },
	{ id: 'TRIM-U06', name: 'clearUpcoming', index: 3, want: ['trim-zulu', 'trim-apple', 'trim-mango', 'trim-kiwi'], wantIndex: 3 }
];

describe('TRIM immutable queue transitions', () => {
	it('TRIM-U00 control: real queue exports create and select without replacing track objects', () => {
		const value = queue.selectTrack(queue.createQueue(TRACKS), 2);
		expect(ids(value)).toEqual(['trim-zulu', 'trim-apple', 'trim-mango', 'trim-kiwi']);
		expect(queue.currentTrack(value)).toBe(TRACKS[2]);
	});

	for (const example of EXAMPLES) {
		it(`${example.id} ${example.name} at index ${example.index} preserves current object and survivor order`, () => {
			const apply = transition(example.name);
			const value = frozen(TRACKS, example.index);
			const before = structuredClone(value);
			const selected = queue.currentTrack(value);
			const result = apply(value);

			expect(ids(result)).toEqual(example.want);
			expect(result.currentIndex).toBe(example.wantIndex);
			expect(queue.currentTrack(result)).toBe(selected);
			for (const track of result.tracks) {
				expect(track).toBe(value.tracks.find((original) => original.id === track.id));
			}
			expect(value).toEqual(before);
			if (example.want.length < value.tracks.length) {
				expect(result).not.toBe(value);
				expect(result.tracks).not.toBe(value.tracks);
			}
		});
	}

	for (const [operationIndex, name] of (['keepCurrentAndUpcoming', 'clearUpcoming'] as const).entries()) {
		it(`TRIM-U07-${operationIndex} ${name} preserves a singleton`, () => {
			const apply = transition(name);
			const value = frozen([TRACKS[2]!], 0);
			expect(apply(value)).toEqual(value);
			expect(queue.currentTrack(apply(value))).toBe(TRACKS[2]);
		});

		const invalid = [
			{ label: 'empty', tracks: [], index: -1 },
			{ label: 'empty-with-zero', tracks: [], index: 0 },
			{ label: 'negative', tracks: TRACKS, index: -2 },
			{ label: 'unselected', tracks: TRACKS, index: -1 },
			{ label: 'past-end', tracks: TRACKS, index: 4 },
			{ label: 'fraction', tracks: TRACKS, index: 1.5 },
			{ label: 'nan', tracks: TRACKS, index: Number.NaN },
			{ label: 'positive-infinity', tracks: TRACKS, index: Number.POSITIVE_INFINITY },
			{ label: 'negative-infinity', tracks: TRACKS, index: Number.NEGATIVE_INFINITY }
		];
		for (const example of invalid) {
			it(`TRIM-U08-${operationIndex}-${example.label} ${name} fails closed without inventing a selection`, () => {
				const apply = transition(name);
				const value = frozen(example.tracks, example.index);
				const result = apply(value);
				expect(result).toEqual(value);
				expect(result.tracks).toEqual(value.tracks);
				expect(result.currentIndex).toBe(example.index);
			});
		}

		it(`TRIM-U09-${operationIndex} repeated ${name} is a no-op after removing its eligible rows`, () => {
			const apply = transition(name);
			const value = frozen(TRACKS, 1);
			const once = apply(value);
			const expected = name === 'keepCurrentAndUpcoming'
				? ['trim-apple', 'trim-mango', 'trim-kiwi']
				: ['trim-zulu', 'trim-apple'];
			expect(ids(once)).toEqual(expected);
			const twice = apply(frozen(once.tracks, once.currentIndex));
			const thrice = apply(frozen(twice.tracks, twice.currentIndex));
			expect(twice).toEqual(once);
			expect(thrice).toEqual(once);
			expect(queue.currentTrack(thrice)).toBe(TRACKS[1]);
		});
	}

	it('TRIM-U10 sequential prefix and suffix trims leave only the same current object', () => {
		const keep = transition('keepCurrentAndUpcoming');
		const clear = transition('clearUpcoming');
		const value = frozen(TRACKS, 2);
		for (const result of [clear(keep(value)), keep(clear(value))]) {
			expect(ids(result)).toEqual(['trim-mango']);
			expect(result.currentIndex).toBe(0);
			expect(queue.currentTrack(result)).toBe(TRACKS[2]);
		}
	});

	it('TRIM-U11 trims use arranged order after move/play-next, not catalogue order', () => {
		const keep = transition('keepCurrentAndUpcoming');
		const clear = transition('clearUpcoming');
		const selected = queue.selectTrack(queue.createQueue(TRACKS), 2);
		const arranged = queue.playNext(queue.moveTrack(selected, 3, 0), 1);
		expect(ids(arranged)).toEqual(['trim-kiwi', 'trim-apple', 'trim-mango', 'trim-zulu']);
		expect(ids(keep(arranged))).toEqual(['trim-mango', 'trim-zulu']);
		expect(ids(clear(arranged))).toEqual(['trim-kiwi', 'trim-apple', 'trim-mango']);
		expect(queue.currentTrack(keep(arranged))).toBe(TRACKS[2]);
		expect(queue.currentTrack(clear(arranged))).toBe(TRACKS[2]);
	});
});
