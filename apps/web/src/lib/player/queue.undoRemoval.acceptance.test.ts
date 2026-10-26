import { describe, expect, it } from 'vitest';
import * as queue from './queue.js';
import type { QueueState, Track } from './queue.js';

/**
 * UNDO — independent, protected acceptance (pure queue layer). No
 * implementation accompanies this test bank. Carry this file and
 * tests/library-queue-undo-removal.spec.ts unchanged into the combined
 * implementation PR. Return incorrect tests to their author, never silently
 * weaken them. No standalone ready PR of this intentionally RED bank.
 * Bank ref: test/queue-undo-removal-acceptance. Baseline:
 * a242bbc721f17e736757dadc4a6f85ba396e2d3f.
 *
 * Proposed public seam in queue.ts (immutable, transport-free):
 *   removeTrackWithUndo(state, index) -> { state, undo }
 *   undoRemoval(state, undo)         -> QueueState
 * `undo` is plain in-memory data carrying at least `track` (the exact removed
 * track object; hosts read its title for the affordance/announcement) and
 * `index` (the original position). Extra fingerprint fields are the
 * implementer's choice. A valid removal's `state` is exactly what today's
 * removeTrack returns; undoRemoval against that untouched state restores the
 * pre-removal arrangement AND selection with the same track objects. Against
 * any other state (appended, reordered, cleared, re-selected, trimmed, a
 * second removal, already undone, an unrelated queue) undoRemoval returns the
 * identical state object: no insert, no duplicate, no clamp. Invalid indexes
 * return the identical state with `undo: null`; a null/undefined undo and a
 * tampered index or survivor track fail closed the same way.
 * Namespace lookup lets the real, existing module import and execute normally
 * before a precise missing-capability ASSERTION; it supplies no fallback/stub.
 *
 * The token alone cannot distinguish "mutated then coincidentally restored"
 * from "never mutated"; retiring the live affordance on every other queue
 * mutation, single-slot replacement in the UI and session-only lifetime are
 * pinned at the host level in the companion browser spec. Undo for clear-all,
 * append/reorder undo and cross-reload history are out of scope.
 *
 * Breaks caught: restore at the end instead of the original index, restoring
 * a copy instead of the exact track, restarting selection at the slot instead
 * of the pre-removal track, undo surviving an intervening mutation, duplicate
 * insert after a re-add, second-removal undo restoring the first track,
 * double-undo, input mutation, and undo data leaking into serializeQueue.
 * Expectations are hand-derived, never computed with the seam under test
 * (post-removal states are cross-checked against the shipped removeTrack).
 *
 * Run from repo root:
 *   pnpm --dir apps/web test src/lib/player/queue.undoRemoval.acceptance.test.ts
 *   pnpm --dir apps/web test
 *   pnpm --dir apps/web check
 * Browser commands and the focus/announcement contract live in the companion
 * spec. Expected on the baseline: assertion RED from the capability checks
 * (the seam is undefined), with the UNDO-U00/U20 real-module controls passing.
 */

type RemovalUndo = { track: Track; index: number } & Record<string, unknown>;
type RemoveResult = { state: QueueState; undo: RemovalUndo | null };
type RemoveWithUndo = (state: QueueState, index: number) => RemoveResult;
type UndoRemoval = (state: QueueState, undo: RemovalUndo | null | undefined) => QueueState;

function removeWithUndo(state: QueueState, index: number): RemoveResult {
	const candidate: unknown = Reflect.get(queue, 'removeTrackWithUndo');
	expect(candidate, 'UNDO requires production queue.removeTrackWithUndo(state, index)').toBeTypeOf(
		'function'
	);
	return (candidate as RemoveWithUndo)(state, index);
}

function undoRemoval(state: QueueState, undo: RemovalUndo | null | undefined): QueueState {
	const candidate: unknown = Reflect.get(queue, 'undoRemoval');
	expect(candidate, 'UNDO requires production queue.undoRemoval(state, undo)').toBeTypeOf(
		'function'
	);
	return (candidate as UndoRemoval)(state, undo);
}

const TRACKS: Track[] = [
	{ id: 'undo-zulu', title: 'Zulu', artist: 'Undo Band', streamUrl: '/test-audio/undo-zulu', artworkUrl: null },
	{ id: 'undo-apple', title: 'Apple', artist: 'Undo Band', streamUrl: '/test-audio/undo-apple', artworkUrl: null },
	{ id: 'undo-mango', title: 'Mango', artist: 'Undo Band', streamUrl: '/test-audio/undo-mango', artworkUrl: null },
	{ id: 'undo-kiwi', title: 'Kiwi', artist: 'Undo Band', streamUrl: '/test-audio/undo-kiwi', artworkUrl: null }
];
const OUTSIDER: Track = {
	id: 'undo-outsider',
	title: 'Outsider',
	artist: 'Undo Band',
	streamUrl: '/test-audio/undo-outsider',
	artworkUrl: null
};
const ALL_IDS = ['undo-zulu', 'undo-apple', 'undo-mango', 'undo-kiwi'];

function frozen(tracks: Track[], currentIndex: number): QueueState {
	tracks.forEach(Object.freeze);
	const value = { tracks: [...tracks], currentIndex };
	Object.freeze(value.tracks);
	return Object.freeze(value);
}

function ids(value: QueueState): string[] {
	return value.tracks.map((track) => track.id);
}

/** Remove index 2 (Mango, upcoming) from a 4-track queue, current = Apple. */
function mangoRemoval(): { before: QueueState; after: QueueState; undo: RemovalUndo } {
	const before = frozen(TRACKS, 1);
	const result = removeWithUndo(before, 2);
	expect(result.undo, 'a valid removal must yield an undo token').not.toBeNull();
	return { before, after: result.state, undo: result.undo! };
}

describe('UNDO pure queue transitions', () => {
	it('UNDO-U00 control: the shipped removeTrack works on these fixtures', () => {
		const value = queue.removeTrack(frozen(TRACKS, 1), 2);
		expect(ids(value)).toEqual(['undo-zulu', 'undo-apple', 'undo-kiwi']);
		expect(queue.currentTrack(value)).toBe(TRACKS[1]);
	});

	const EXAMPLES = [
		{ id: 'UNDO-U01', label: 'first track, current', current: 0, remove: 0 },
		{ id: 'UNDO-U02', label: 'first track, before current', current: 2, remove: 0 },
		{ id: 'UNDO-U03', label: 'middle track, current', current: 1, remove: 1 },
		{ id: 'UNDO-U04', label: 'middle track, upcoming next', current: 1, remove: 2 },
		{ id: 'UNDO-U05', label: 'middle track, before current', current: 3, remove: 1 },
		{ id: 'UNDO-U06', label: 'upcoming track from the first', current: 0, remove: 2 },
		{ id: 'UNDO-U07', label: 'last track, upcoming', current: 0, remove: 3 },
		{ id: 'UNDO-U08', label: 'last track, current tail', current: 3, remove: 3 }
	];
	for (const example of EXAMPLES) {
		it(`${example.id} removing the ${example.label} then undoing restores arrangement and selection exactly`, () => {
			const before = frozen(TRACKS, example.current);
			const snapshot = structuredClone(before);
			const { state: after, undo } = removeWithUndo(before, example.remove);

			// The removal itself must match the shipped removal semantics.
			expect(after).toEqual(queue.removeTrack(before, example.remove));
			expect(ids(after)).toEqual(ALL_IDS.filter((_, i) => i !== example.remove));
			expect(undo, 'a valid removal must yield an undo token').not.toBeNull();
			expect(undo!.track).toBe(TRACKS[example.remove]);
			expect(undo!.index).toBe(example.remove);

			const restored = undoRemoval(after, undo);
			expect(ids(restored)).toEqual(ALL_IDS);
			expect(restored.currentIndex).toBe(example.current);
			expect(queue.currentTrack(restored)).toBe(TRACKS[example.current]);
			restored.tracks.forEach((track, i) => expect(track).toBe(TRACKS[i]));
			expect(restored).not.toBe(after);
			expect(restored.tracks).not.toBe(after.tracks);
			// Frozen inputs come through untouched.
			expect(before).toEqual(snapshot);
		});
	}

	it('UNDO-U09 removing the sole track then undoing restores it as current', () => {
		const before = frozen([TRACKS[1]!], 0);
		const { state: after, undo } = removeWithUndo(before, 0);
		expect(after.tracks).toEqual([]);
		expect(after.currentIndex).toBe(-1);

		const restored = undoRemoval(after, undo);
		expect(ids(restored)).toEqual(['undo-apple']);
		expect(restored.currentIndex).toBe(0);
		expect(queue.currentTrack(restored)).toBe(TRACKS[1]);
	});

	it('UNDO-U10 a two-track queue restores order and the surviving selection', () => {
		const before = frozen([TRACKS[0]!, TRACKS[1]!], 1);
		const { state: after, undo } = removeWithUndo(before, 0);
		expect(ids(after)).toEqual(['undo-apple']);
		expect(after.currentIndex).toBe(0);

		const restored = undoRemoval(after, undo);
		expect(ids(restored)).toEqual(['undo-zulu', 'undo-apple']);
		expect(restored.currentIndex).toBe(1);
		expect(queue.currentTrack(restored)).toBe(TRACKS[1]);
	});

	for (const [label, index] of [
		['negative', -1],
		['past-end', 4],
		['fraction', 1.5],
		['nan', Number.NaN],
		['positive-infinity', Number.POSITIVE_INFINITY],
		['negative-infinity', Number.NEGATIVE_INFINITY]
	] as const) {
		it(`UNDO-U11-${label} an invalid removal index is an identical-state no-op with no token`, () => {
			const before = frozen(TRACKS, 1);
			const result = removeWithUndo(before, index);
			expect(result.state).toBe(before);
			expect(result.undo).toBeNull();
		});
	}

	it('UNDO-U12 removing from an empty queue yields no token and no invented state', () => {
		const empty = frozen([], -1);
		const result = removeWithUndo(empty, 0);
		expect(result.state).toBe(empty);
		expect(result.undo).toBeNull();
	});

	it('UNDO-U13 a null or undefined undo is an identical-state no-op', () => {
		const { after } = mangoRemoval();
		expect(undoRemoval(after, null)).toBe(after);
		expect(undoRemoval(after, undefined)).toBe(after);
		expect(ids(after)).toEqual(['undo-zulu', 'undo-apple', 'undo-kiwi']);
	});

	const MUTATIONS: { id: string; label: string; mutate: (after: QueueState) => QueueState }[] = [
		{ id: 'UNDO-U14-append', label: 'appending a new track', mutate: (s) => queue.addTrack(s, OUTSIDER) },
		{ id: 'UNDO-U14-readd', label: 're-adding the removed track', mutate: (s) => queue.addTrack(s, TRACKS[2]!) },
		{ id: 'UNDO-U14-reorder', label: 'reordering survivors', mutate: (s) => queue.moveTrack(s, 2, 0) },
		{ id: 'UNDO-U14-playnext', label: 'play-next reordering', mutate: (s) => queue.playNext(s, 0) },
		{ id: 'UNDO-U14-clear', label: 'clearing the queue', mutate: (s) => queue.clearQueue(s) },
		{ id: 'UNDO-U14-select', label: 'selecting another track', mutate: (s) => queue.selectTrack(s, 2) },
		{ id: 'UNDO-U14-next', label: 'advancing to the next track', mutate: (s) => queue.nextTrack(s) },
		{ id: 'UNDO-U14-previous', label: 'returning to the previous track', mutate: (s) => queue.previousTrack(s) },
		{ id: 'UNDO-U14-trim', label: 'keeping only current and upcoming', mutate: (s) => queue.keepCurrentAndUpcoming(s) },
		{ id: 'UNDO-U14-remove', label: 'removing another track', mutate: (s) => queue.removeTrack(s, 0) }
	];
	for (const mutation of MUTATIONS) {
		it(`${mutation.id} undo after ${mutation.label} is an identical-state no-op`, () => {
			const { after, undo } = mangoRemoval();
			const mutated = mutation.mutate(after);
			// Guard: the intervening edit really changed the queue.
			expect(mutated).not.toEqual(after);

			expect(undoRemoval(mutated, undo)).toBe(mutated);
			// No duplicate of the removed track is ever inserted.
			expect(ids(mutated).filter((id) => id === 'undo-mango').length).toBeLessThanOrEqual(1);
		});
	}

	it('UNDO-U15 a second removal replaces the slot: only its track can come back', () => {
		const before = frozen(TRACKS, 0);
		const first = removeWithUndo(before, 1); // Apple out
		expect(ids(first.state)).toEqual(['undo-zulu', 'undo-mango', 'undo-kiwi']);
		const second = removeWithUndo(first.state, 1); // Mango out
		expect(ids(second.state)).toEqual(['undo-zulu', 'undo-kiwi']);

		// The first token is dead against the twice-removed state.
		expect(undoRemoval(second.state, first.undo)).toBe(second.state);

		const restored = undoRemoval(second.state, second.undo);
		expect(ids(restored)).toEqual(['undo-zulu', 'undo-mango', 'undo-kiwi']);
		expect(restored.currentIndex).toBe(0);
		expect(queue.currentTrack(restored)).toBe(TRACKS[0]);
	});

	it('UNDO-U16 undo is single-use: replaying a spent token changes nothing', () => {
		const { after, undo } = mangoRemoval();
		const restored = undoRemoval(after, undo);
		expect(ids(restored)).toEqual(ALL_IDS);
		expect(undoRemoval(restored, undo)).toBe(restored);
		expect(ids(restored)).toEqual(ALL_IDS);
	});

	it('UNDO-U17 a token applies to no queue but its own post-removal state', () => {
		const { before, undo } = mangoRemoval();
		// The pre-removal queue itself (the track is already there).
		expect(undoRemoval(before, undo)).toBe(before);
		// A queue the token never came from.
		const unrelated = frozen([OUTSIDER, TRACKS[0]!], 0);
		expect(undoRemoval(unrelated, undo)).toBe(unrelated);
	});

	for (const [label, tamper] of [
		['past-end-index', (undo: RemovalUndo) => ({ ...undo, index: 99 })],
		['negative-index', (undo: RemovalUndo) => ({ ...undo, index: -1 })],
		['fractional-index', (undo: RemovalUndo) => ({ ...undo, index: 1.5 })],
		['survivor-track', (undo: RemovalUndo) => ({ ...undo, track: TRACKS[3]! })]
	] as const) {
		it(`UNDO-U18-${label} a tampered token fails closed without inserting`, () => {
			const { after, undo } = mangoRemoval();
			expect(undoRemoval(after, tamper(undo))).toBe(after);
			expect(ids(after)).toEqual(['undo-zulu', 'undo-apple', 'undo-kiwi']);
		});
	}

	it('UNDO-U19 undo works after the queue was arranged by move/play-next, not catalogue order', () => {
		const selected = queue.selectTrack(queue.createQueue(TRACKS), 2);
		const arranged = queue.playNext(queue.moveTrack(selected, 3, 0), 1);
		expect(ids(arranged)).toEqual(['undo-kiwi', 'undo-apple', 'undo-mango', 'undo-zulu']);
		const before = frozen(arranged.tracks, arranged.currentIndex);

		const { state: after, undo } = removeWithUndo(before, 0); // Kiwi, ahead of current Mango
		expect(ids(after)).toEqual(['undo-apple', 'undo-mango', 'undo-zulu']);

		const restored = undoRemoval(after, undo);
		expect(ids(restored)).toEqual(['undo-kiwi', 'undo-apple', 'undo-mango', 'undo-zulu']);
		expect(restored.currentIndex).toBe(before.currentIndex);
		expect(queue.currentTrack(restored)).toBe(TRACKS[2]);
	});

	it('UNDO-U20 control: serialization carries no trace of the removed track (session-only)', () => {
		const after = queue.removeTrack(frozen(TRACKS, 1), 2);
		const serialized = queue.serializeQueue(after);
		expect(serialized).not.toContain('undo-mango');
		expect(ids(queue.restoreQueue(serialized, TRACKS))).toEqual([
			'undo-zulu',
			'undo-apple',
			'undo-kiwi'
		]);
	});
});
