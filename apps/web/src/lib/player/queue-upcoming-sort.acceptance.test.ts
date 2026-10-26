import { describe, expect, it, vi } from 'vitest';
import * as queueModule from './queue.js';
import type { QueueState, Track } from './queue.js';

// Protected test-first handoff: implement the real transition without editing
// this bank. Literal expectations are independent of the library sort helper.

type SortMode = 'title' | 'artist';
type SortUpcoming = (state: QueueState, mode: SortMode) => QueueState;

// A namespace lookup keeps the pre-feature bank type-checkable. Missing behavior
// must fail an assertion, never test discovery or an unresolved named import.
function sortUpcoming(state: QueueState, mode: SortMode): QueueState {
	const transition = Reflect.get(queueModule, 'sortUpcoming');
	expect(transition, 'the queue must expose the one-shot sortUpcoming transition').toBeTypeOf(
		'function'
	);
	return (transition as SortUpcoming)(state, mode);
}

function track(id: string, title = id, artist = 'Fixture artist'): Track {
	return { id, title, artist, streamUrl: `/api/demo/audio?track=${id}`, artworkUrl: null };
}

function frozenState(tracks: Track[], currentIndex: number): QueueState {
	for (const entry of tracks) Object.freeze(entry);
	Object.freeze(tracks);
	return Object.freeze({ tracks, currentIndex });
}

function ids(state: QueueState): string[] {
	return state.tracks.map((entry) => entry.id);
}

describe('sort only upcoming tracks — independent acceptance', () => {
	// Catches sorting the full queue, moving current, cloning metadata, or mutating input.
	it.each([
		['title', ['played', 'current', 'a', 'b', 'c']],
		['artist', ['played', 'current', 'c', 'b', 'a']]
	] as const)('[US-P01-%s] preserves exact prefix/current and all track objects', (mode, expected) => {
		const input = frozenState(
			[
				track('played', 'Zulu played', 'Zulu played'),
				track('current', 'Middle current', 'Middle current'),
				track('c', 'Charlie', 'Alpha'),
				track('a', 'Alpha', 'Charlie'),
				track('b', 'Bravo', 'Bravo')
			],
			1
		);
		const before = JSON.stringify(input);
		const result = sortUpcoming(input, mode);
		expect(ids(result)).toEqual(expected);
		expect(result).not.toBe(input);
		expect(result.tracks).not.toBe(input.tracks);
		expect(result.currentIndex).toBe(1);
		expect(result.tracks[0]).toBe(input.tracks[0]);
		expect(result.tracks[1]).toBe(input.tracks[1]);
		for (const entry of result.tracks) {
			expect(entry).toBe(input.tracks.find((original) => original.id === entry.id));
		}
		expect(JSON.stringify(input)).toBe(before);
	});

	// Equal labels retain suffix order: neither id nor the other field breaks ties.
	it('[US-P02] title ties are stable and blank titles fall back to stable ids', () => {
		const input = frozenState(
			[
				track('current'),
				track('z-tie', 'Echo', 'Zulu'),
				track('b-fallback', '   '),
				track('a-tie', 'eCHO', 'Alpha'),
				track('a-fallback', ''),
				track('tail', 'Zulu')
			],
			0
		);
		expect(ids(sortUpcoming(input, 'title'))).toEqual([
			'current', 'a-fallback', 'b-fallback', 'z-tie', 'a-tie', 'tail'
		]);
	});

	it('[US-P03] artist ties are stable and missing artists follow named artists', () => {
		const input = frozenState(
			[
				track('current'),
				track('missing-z', 'Zulu', ''),
				track('z-tie', 'Zulu', ' Echo '),
				track('late', 'Any', 'Zulu'),
				track('missing-a', 'Alpha', ' \t '),
				track('early', 'Any', 'Alpha'),
				track('a-tie', 'Alpha', 'eCHO')
			],
			0
		);
		expect(ids(sortUpcoming(input, 'artist'))).toEqual([
			'current', 'early', 'z-tie', 'a-tie', 'late', 'missing-z', 'missing-a'
		]);
	});

	it.each(['title', 'artist'] as const)(
		'[US-P04-%s] uses deterministic code-unit keys, never locale or randomness',
		(mode) => {
			const input = frozenState(
				[
					track('current'),
					track('umlaut', 'Äther', 'Äther'),
					track('two', '2', '2'),
					track('z', 'zebra', 'zebra'),
					track('ten', '10', '10'),
					track('a', 'Alpha', 'Alpha')
				],
				0
			);
			const locale = vi.spyOn(String.prototype, 'localeCompare').mockImplementation(() => {
				throw new Error('locale-dependent queue ordering is forbidden');
			});
			const random = vi.spyOn(Math, 'random').mockImplementation(() => {
				throw new Error('a deterministic sort must not draw randomness');
			});
			try {
				expect(ids(sortUpcoming(input, mode))).toEqual([
					'current', 'ten', 'two', 'a', 'z', 'umlaut'
				]);
			} finally {
				locale.mockRestore();
				random.mockRestore();
			}
		}
	);

	it.each([-2, -1, 0.5, 3, 20, Number.NaN, Number.POSITIVE_INFINITY])(
		'[US-P05-%s] an invalid current selection is an identity no-op',
		(currentIndex) => {
			const input = frozenState([track('c'), track('b'), track('a')], currentIndex);
			for (const mode of ['title', 'artist'] as const) {
				expect(sortUpcoming(input, mode)).toBe(input);
			}
		}
	);

	it.each([
		{ tracks: [], currentIndex: -1 },
		{ tracks: [track('current')], currentIndex: 0 },
		{ tracks: [track('current'), track('only-upcoming')], currentIndex: 0 },
		{ tracks: [track('played'), track('current')], currentIndex: 1 }
	])('[US-P06-$currentIndex-$tracks.length] fewer than two upcoming is an identity no-op', (state) => {
		const input = frozenState(state.tracks, state.currentIndex);
		for (const mode of ['title', 'artist'] as const) {
			expect(sortUpcoming(input, mode)).toBe(input);
		}
	});

	it.each(['title', 'artist'] as const)(
		'[US-P07-%s] an already-correct suffix is an identity no-op, including ties',
		(mode) => {
			const input = frozenState(
				[
					track('current', 'Zulu', 'Zulu'),
					track('z-tie', 'Alpha', 'Alpha'),
					track('a-tie', 'ALPHA', 'ALPHA'),
					track('last', 'Zulu', 'Zulu')
				],
				0
			);
			expect(sortUpcoming(input, mode)).toBe(input);
		}
	);

	it('[US-P09] missing metadata uses id titles and stable unnamed artists without rewriting records', () => {
		// Metadata can be absent at the catalogue boundary even though normalized
		// Track consumers usually receive strings. Keep the raw records intact.
		const missing = { id: 'b-missing', streamUrl: '/api/demo/audio?track=b-missing' } as Track;
		const blank = track('a-blank', '', '');
		const input = frozenState([track('current'), missing, track('named', ' Zebra', 'Alpha'), blank], 0);
		expect(ids(sortUpcoming(input, 'title'))).toEqual(['current', 'named', 'a-blank', 'b-missing']);
		expect(ids(sortUpcoming(input, 'artist'))).toEqual(['current', 'named', 'b-missing', 'a-blank']);
		expect(missing).not.toHaveProperty('title');
		expect(missing).not.toHaveProperty('artist');
	});

	it('[US-P08] sorting is one-shot, not a mode that reorders future appends', () => {
		const input = frozenState([track('current'), track('c'), track('b')], 0);
		const arranged = sortUpcoming(input, 'title');
		const appended = queueModule.addTrack(arranged, track('a'));
		expect(ids(appended)).toEqual(['current', 'b', 'c', 'a']);
		expect(ids(arranged)).toEqual(['current', 'b', 'c']);
		expect(appended.currentIndex).toBe(0);
	});
});
