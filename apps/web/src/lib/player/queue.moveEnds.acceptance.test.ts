import { describe, expect, it } from 'vitest';
import * as queueModule from './queue.js';
import {
	createQueue,
	currentTrack,
	selectTrack,
	type QueueState,
	type Track
} from './queue.js';

/**
 * QTOP acceptance-owned contract; Builder must not edit this file.
 * Test bank ref: test/queue-move-ends-acceptance.
 *
 * Add `moveToTop(state, index)` / `moveToBottom(state, index)` to queue.ts,
 * pure helpers over moveTrack following the playNext pattern: the track at
 * `index` lands at 0 / at the last index, the current *track identity* is
 * preserved (never the slot), and no-ops return the identical state object.
 * No-ops: top-most to top, bottom-most to bottom, out-of-range / non-integer
 * indexes, and any move on an empty queue.
 *
 * A missing export fails here as an assertion (`toBeTypeOf`), never as an
 * import error: this file imports only names that already exist on main.
 * Host wiring (buttons, announcements, end-button state) is pinned in
 * playerQueue.moveEnds.acceptance.test.ts (QTOP-P-*) and
 * tests/queue-move-ends.spec.ts (QTOP-B-*).
 *
 * Reproduce from apps/web:
 *   pnpm install --frozen-lockfile
 *   pnpm test src/lib/player/queue.moveEnds.acceptance.test.ts
 * Expected pre-feature RED: assertion failures for the missing helpers,
 * not import/compile errors. The Builder consumes these tests in one
 * combined ready PR; never merge the RED test bank by itself.
 */

type MoveEnd = (state: QueueState, index: number) => QueueState;

function moveToTop(state: QueueState, index: number): QueueState {
	const fn = (queueModule as unknown as { moveToTop?: MoveEnd }).moveToTop;
	expect(fn, 'queue.ts must export moveToTop(state, index) (pure, over moveTrack)').toBeTypeOf(
		'function'
	);
	return fn!(state, index);
}

function moveToBottom(state: QueueState, index: number): QueueState {
	const fn = (queueModule as unknown as { moveToBottom?: MoveEnd }).moveToBottom;
	expect(
		fn,
		'queue.ts must export moveToBottom(state, index) (pure, over moveTrack)'
	).toBeTypeOf('function');
	return fn!(state, index);
}

const TRACKS: Track[] = [
	{ id: 'qtop-a', title: 'Queue A', artist: 'Fixture', streamUrl: '/a' },
	{ id: 'qtop-b', title: 'Queue B', artist: 'Fixture', streamUrl: '/b' },
	{ id: 'qtop-c', title: 'Queue C', artist: 'Fixture', streamUrl: '/c' },
	{ id: 'qtop-d', title: 'Queue D', artist: 'Fixture', streamUrl: '/d' }
];

function ids(state: QueueState): string[] {
	return state.tracks.map((track) => track.id);
}

/** Frozen input: a mutating helper throws instead of silently passing. */
function frozenQueue(currentIndex = 0): QueueState {
	const queue = selectTrack(createQueue(TRACKS), currentIndex);
	queue.tracks.forEach(Object.freeze);
	Object.freeze(queue.tracks);
	return Object.freeze(queue);
}

describe('move a queued track to the top (acceptance)', () => {
	it('QTOP-U-01 moves a middle track to index 0 and keeps the current track current', () => {
		const queue = frozenQueue(0);
		const moved = moveToTop(queue, 2);

		expect(ids(moved)).toEqual(['qtop-c', 'qtop-a', 'qtop-b', 'qtop-d']);
		expect(currentTrack(moved)?.id).toBe('qtop-a');
		expect(moved.currentIndex).toBe(1);
		expect(moved.tracks).not.toBe(queue.tracks);
	});

	it('QTOP-U-02 moves the last track to index 0 and keeps the current track current', () => {
		const queue = frozenQueue(0);
		const moved = moveToTop(queue, 3);

		expect(ids(moved)).toEqual(['qtop-d', 'qtop-a', 'qtop-b', 'qtop-c']);
		expect(currentTrack(moved)?.id).toBe('qtop-a');
		expect(moved.currentIndex).toBe(1);
	});

	it('QTOP-U-03 moves the current track itself to index 0 with its identity', () => {
		const queue = frozenQueue(2);
		const moved = moveToTop(queue, 2);

		expect(ids(moved)).toEqual(['qtop-c', 'qtop-a', 'qtop-b', 'qtop-d']);
		expect(currentTrack(moved)?.id).toBe('qtop-c');
		expect(moved.currentIndex).toBe(0);
	});

	it('QTOP-U-04 swaps a two-track queue when the tail moves to the top', () => {
		const queue = selectTrack(createQueue(TRACKS.slice(0, 2)), 0);
		const moved = moveToTop(queue, 1);

		expect(ids(moved)).toEqual(['qtop-b', 'qtop-a']);
		expect(currentTrack(moved)?.id).toBe('qtop-a');
		expect(moved.currentIndex).toBe(1);
	});
});

describe('move a queued track to the bottom (acceptance)', () => {
	it('QTOP-U-05 moves the first (current) track to the last index with its identity', () => {
		const queue = frozenQueue(0);
		const moved = moveToBottom(queue, 0);

		expect(ids(moved)).toEqual(['qtop-b', 'qtop-c', 'qtop-d', 'qtop-a']);
		expect(currentTrack(moved)?.id).toBe('qtop-a');
		expect(moved.currentIndex).toBe(3);
		expect(moved.tracks).not.toBe(queue.tracks);
	});

	it('QTOP-U-06 moves a middle track past the current one without stealing it', () => {
		const queue = frozenQueue(1);
		const moved = moveToBottom(queue, 2);

		expect(ids(moved)).toEqual(['qtop-a', 'qtop-b', 'qtop-d', 'qtop-c']);
		expect(currentTrack(moved)?.id).toBe('qtop-b');
		expect(moved.currentIndex).toBe(1);
	});

	it('QTOP-U-07 moves a middle track to the bottom while the tail stays current', () => {
		const queue = frozenQueue(3);
		const moved = moveToBottom(queue, 1);

		expect(ids(moved)).toEqual(['qtop-a', 'qtop-c', 'qtop-d', 'qtop-b']);
		expect(currentTrack(moved)?.id).toBe('qtop-d');
		expect(moved.currentIndex).toBe(2);
	});

	it('QTOP-U-08 swaps a two-track queue when the head moves to the bottom', () => {
		const queue = selectTrack(createQueue(TRACKS.slice(0, 2)), 1);
		const moved = moveToBottom(queue, 0);

		expect(ids(moved)).toEqual(['qtop-b', 'qtop-a']);
		expect(currentTrack(moved)?.id).toBe('qtop-b');
		expect(moved.currentIndex).toBe(0);
	});
});

describe('move-to-end no-ops (acceptance)', () => {
	it('QTOP-U-09 returns the identical state for already-at-end moves', () => {
		const queue = frozenQueue(1);
		expect(moveToTop(queue, 0)).toBe(queue);
		expect(moveToBottom(queue, TRACKS.length - 1)).toBe(queue);

		const single = createQueue(TRACKS.slice(0, 1));
		expect(moveToTop(single, 0)).toBe(single);
		expect(moveToBottom(single, 0)).toBe(single);
	});

	it('QTOP-U-10 returns the identical state for out-of-range indexes and empty queues', () => {
		const queue = frozenQueue(0);
		for (const index of [-1, TRACKS.length, Number.NaN, 1.5, Number.POSITIVE_INFINITY]) {
			expect(moveToTop(queue, index)).toBe(queue);
			expect(moveToBottom(queue, index)).toBe(queue);
		}

		const empty = createQueue([]);
		expect(moveToTop(empty, 0)).toBe(empty);
		expect(moveToBottom(empty, 0)).toBe(empty);
	});
});
