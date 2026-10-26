import { describe, expect, it } from 'vitest';
import * as queue from './queue.js';
import type { QueueState, Track } from './queue.js';

/**
 * QUEUEMULTI — independent, protected acceptance. No implementation accompanies
 * this test bank. Carry this file and
 * tests/library-queue-bulk-remove.spec.ts unchanged into the combined
 * implementation PR. Return incorrect tests to their author, never silently
 * weaken them.
 *
 * Proposed public seam: removeTracks(state, ids).
 * Immutable QueueState -> QueueState transition, not transport. It removes the
 * activation snapshot (the selected IDs at activation time) atomically in one
 * transition: survivors keep arranged order and object identity, the current
 * track object is preserved when it survives, otherwise the first surviving
 * successor in the original order becomes current, otherwise the last
 * surviving predecessor, otherwise the queue empties (currentIndex -1).
 * Namespace lookup lets the real, existing module import and execute normally
 * before a precise missing-capability ASSERTION; it supplies no fallback/stub.
 * Once that assertion passes every case exercises the production transition.
 *
 * Breaks caught: per-row loop removal reordering survivors, cloning the
 * current track, stale index after removal, sorting by selection order instead
 * of retaining arranged order, duplicate double-remove effects, inventing a
 * selection on no-op, mutating frozen input, and non-idempotent edits.
 * Expectations are hand-derived, never computed with the transition under test.
 *
 * Run from repo root:
 *   pnpm --dir apps/web test src/lib/player/queue.bulkRemove.acceptance.test.ts
 *   pnpm --dir apps/web test
 *   pnpm --dir apps/web check
 * Browser commands and the checkbox/count/focus contract live in the companion
 * spec. No existing test or production file is changed.
 */

type Transition = (state: QueueState, ids: readonly string[]) => QueueState;

function transition(): Transition {
	const candidate: unknown = Reflect.get(queue, 'removeTracks');
	expect(candidate, 'QUEUEMULTI requires production queue.removeTracks(state, ids)').toBeTypeOf(
		'function'
	);
	return candidate as Transition;
}

const TRACKS: Track[] = [
	{
		id: 'qm-zulu',
		title: 'Zulu Song',
		artist: 'Bulk Band',
		streamUrl: '/test-audio/zulu',
		artworkUrl: null
	},
	{
		id: 'qm-alpha',
		title: 'Alpha Song',
		artist: 'Bulk Band',
		streamUrl: '/test-audio/alpha',
		artworkUrl: null
	},
	{
		id: 'qm-mike',
		title: 'Mike Song',
		artist: 'Bulk Band',
		streamUrl: '/test-audio/mike',
		artworkUrl: null
	},
	{
		id: 'qm-bravo',
		title: 'Bravo Song',
		artist: 'Bulk Band',
		streamUrl: '/test-audio/bravo',
		artworkUrl: null
	},
	{
		id: 'qm-kilo',
		title: 'Kilo Song',
		artist: 'Bulk Band',
		streamUrl: '/test-audio/kilo',
		artworkUrl: null
	}
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

function expectIdentities(result: QueueState, before: QueueState): void {
	for (const track of result.tracks) {
		expect(track).toBe(before.tracks.find((original) => original.id === track.id));
	}
}

describe('QUEUEMULTI immutable bulk-remove transition', () => {
	it('QUEUEMULTI-U00 control: real queue exports create and single-remove without replacing track objects', () => {
		const value = queue.selectTrack(queue.createQueue(TRACKS), 2);
		expect(ids(value)).toEqual(['qm-zulu', 'qm-alpha', 'qm-mike', 'qm-bravo', 'qm-kilo']);
		expect(queue.currentTrack(value)).toBe(TRACKS[2]);
		const removed = queue.removeTrack(value, 0);
		expect(ids(removed)).toEqual(['qm-alpha', 'qm-mike', 'qm-bravo', 'qm-kilo']);
		expect(queue.currentTrack(removed)).toBe(TRACKS[2]);
	});

	it('QUEUEMULTI-U01 remove non-current subset preserves exact current object and survivor order', () => {
		const apply = transition();
		const value = frozen(TRACKS, 0);
		const before = structuredClone(value);
		const selected = queue.currentTrack(value);
		const result = apply(value, ['qm-alpha', 'qm-bravo']);

		expect(ids(result)).toEqual(['qm-zulu', 'qm-mike', 'qm-kilo']);
		expect(result.currentIndex).toBe(0);
		expect(queue.currentTrack(result)).toBe(selected);
		expect(queue.currentTrack(result)).toBe(TRACKS[0]);
		expectIdentities(result, value);
		expect(value).toEqual(before);
		expect(result).not.toBe(value);
		expect(result.tracks).not.toBe(value.tracks);
	});

	it('QUEUEMULTI-U02 remove current selects the first surviving successor in original order', () => {
		const apply = transition();
		// Current is alpha (index 1); remove alpha + mike. Survivors are
		// zulu, bravo, kilo; the first survivor after the removed current is
		// bravo (original index 3), not the predecessor zulu.
		const value = frozen(TRACKS, 1);
		const before = structuredClone(value);
		const result = apply(value, ['qm-alpha', 'qm-mike']);

		expect(ids(result)).toEqual(['qm-zulu', 'qm-bravo', 'qm-kilo']);
		expect(result.currentIndex).toBe(1);
		expect(queue.currentTrack(result)).toBe(TRACKS[3]);
		expect(queue.currentTrack(result)?.streamUrl).toBe('/test-audio/bravo');
		expectIdentities(result, value);
		expect(value).toEqual(before);
	});

	it('QUEUEMULTI-U03 remove current and every successor selects the last surviving predecessor', () => {
		const apply = transition();
		// Current is mike (index 2); remove mike + bravo + kilo. Survivors
		// are zulu, alpha; no successor survives, so the last predecessor
		// (alpha, original index 1) becomes current.
		const value = frozen(TRACKS, 2);
		const before = structuredClone(value);
		const result = apply(value, ['qm-mike', 'qm-bravo', 'qm-kilo']);

		expect(ids(result)).toEqual(['qm-zulu', 'qm-alpha']);
		expect(result.currentIndex).toBe(1);
		expect(queue.currentTrack(result)).toBe(TRACKS[1]);
		expectIdentities(result, value);
		expect(value).toEqual(before);
	});

	it('QUEUEMULTI-U04 remove current tail selects the last surviving predecessor', () => {
		const apply = transition();
		// Current is the tail (kilo, index 4); remove only the tail. The new
		// current is bravo (original index 3), the last surviving predecessor.
		const value = frozen(TRACKS, 4);
		const result = apply(value, ['qm-kilo']);

		expect(ids(result)).toEqual(['qm-zulu', 'qm-alpha', 'qm-mike', 'qm-bravo']);
		expect(result.currentIndex).toBe(3);
		expect(queue.currentTrack(result)).toBe(TRACKS[3]);
		expectIdentities(result, value);
	});

	it('QUEUEMULTI-U05 remove every track empties the queue and clears current', () => {
		const apply = transition();
		const value = frozen(TRACKS, 2);
		const before = structuredClone(value);
		const result = apply(value, ['qm-zulu', 'qm-alpha', 'qm-mike', 'qm-bravo', 'qm-kilo']);

		expect(result.tracks).toEqual([]);
		expect(result.currentIndex).toBe(-1);
		expect(queue.currentTrack(result)).toBeNull();
		expect(result).not.toBe(value);
		expect(value).toEqual(before);
	});

	it('QUEUEMULTI-U06 empty selection is a no-op returning the same state', () => {
		const apply = transition();
		const value = frozen(TRACKS, 1);
		expect(apply(value, [])).toBe(value);
	});

	it('QUEUEMULTI-U07 unknown IDs are ignored and duplicates remove once', () => {
		const apply = transition();
		const value = frozen(TRACKS, 0);
		const before = structuredClone(value);
		const result = apply(value, ['qm-alpha', 'qm-alpha', 'missing-id', 'qm-alpha']);

		expect(ids(result)).toEqual(['qm-zulu', 'qm-mike', 'qm-bravo', 'qm-kilo']);
		expect(result.currentIndex).toBe(0);
		expect(queue.currentTrack(result)).toBe(TRACKS[0]);
		expectIdentities(result, value);
		expect(value).toEqual(before);
	});

	it('QUEUEMULTI-U08 all-unknown selection is a no-op returning the same state', () => {
		const apply = transition();
		const value = frozen(TRACKS, 2);
		expect(apply(value, ['ghost-a', 'ghost-b'])).toBe(value);
	});

	it('QUEUEMULTI-U09 activation order never reorders survivors', () => {
		const apply = transition();
		// Selection order runs against queue order (kilo first, then zulu);
		// survivors must still read in arranged order, not activation order.
		const value = frozen(TRACKS, 2);
		const result = apply(value, ['qm-kilo', 'qm-zulu']);

		expect(ids(result)).toEqual(['qm-alpha', 'qm-mike', 'qm-bravo']);
		expect(result.currentIndex).toBe(1);
		expect(queue.currentTrack(result)).toBe(TRACKS[2]);
		expectIdentities(result, value);
	});

	it('QUEUEMULTI-U10 non-contiguous subset keeps the middle survivor in place', () => {
		const apply = transition();
		// Remove first + last; the middle (including current) keeps exact
		// slots relative to each other. This is the pure analogue of hidden
		// selected rows: visibility never changes the transition.
		const value = frozen(TRACKS, 2);
		const result = apply(value, ['qm-zulu', 'qm-kilo']);

		expect(ids(result)).toEqual(['qm-alpha', 'qm-mike', 'qm-bravo']);
		expect(result.currentIndex).toBe(1);
		expect(queue.currentTrack(result)).toBe(TRACKS[2]);
		expectIdentities(result, value);
	});

	it('QUEUEMULTI-U11 singleton removal empties; singleton survivor is a no-op', () => {
		const apply = transition();
		const single = frozen([TRACKS[2]!], 0);
		const emptied = apply(single, ['qm-mike']);
		expect(emptied.tracks).toEqual([]);
		expect(emptied.currentIndex).toBe(-1);
		expect(apply(single, ['ghost'])).toBe(single);
		expect(apply(single, [])).toBe(single);
	});

	it('QUEUEMULTI-U12 empty queue is a no-op for any selection', () => {
		const apply = transition();
		const value = frozen([], -1);
		expect(apply(value, ['qm-zulu'])).toBe(value);
		expect(apply(value, [])).toBe(value);
	});

	it('QUEUEMULTI-U13 repeated removal with the same snapshot is a no-op', () => {
		const apply = transition();
		const value = frozen(TRACKS, 0);
		const once = apply(value, ['qm-alpha', 'qm-bravo']);
		expect(ids(once)).toEqual(['qm-zulu', 'qm-mike', 'qm-kilo']);
		// The pruned snapshot (IDs already gone) changes nothing on re-apply.
		expect(apply(once, ['qm-alpha', 'qm-bravo'])).toBe(once);
	});

	it('QUEUEMULTI-U14 invalid snapshots fail closed without inventing a selection', () => {
		const apply = transition();
		const value = frozen(TRACKS, 1);
		const invalid: unknown[] = [null, undefined, 'qm-alpha', 42, { id: 'qm-alpha' }];
		for (const snapshot of invalid) {
			expect(apply(value, snapshot as unknown as readonly string[])).toBe(value);
		}
		expect(value.currentIndex).toBe(1);
		expect(queue.currentTrack(value)).toBe(TRACKS[1]);
	});
});
