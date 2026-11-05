import { describe, expect, it } from 'vitest';
import {
	createQueue,
	currentTrack,
	insertTracks,
	insertTracksNext,
	type QueueState,
	type Track
} from './queue.js';

function track(id: string, extra: Partial<Track> = {}): Track {
	return { id, title: `Title ${id}`, artist: `Artist ${id}`, streamUrl: `/audio/${id}`, ...extra };
}

function queue(ids: string[], current = 0): QueueState {
	return { tracks: ids.map((id) => track(id)), currentIndex: current };
}

const ids = (state: QueueState) => state.tracks.map((t) => t.id);

describe('insertTracks', () => {
	it('does not mutate the state or the incoming array', () => {
		const state = queue(['a', 'b', 'c'], 1);
		const incoming = [track('c'), track('x')];
		const stateSnapshot = structuredClone(state);
		const tracksRef = state.tracks;
		const incomingSnapshot = structuredClone(incoming);
		const result = insertTracks(state, incoming, 'a');
		expect(result).not.toBe(state);
		expect(result.tracks).not.toBe(tracksRef);
		expect(state).toEqual(stateSnapshot);
		expect(state.tracks).toBe(tracksRef);
		expect(incoming).toEqual(incomingSnapshot);
	});

	it('inserts new tracks before the destination and keeps the current identity', () => {
		const state = queue(['a', 'b', 'c'], 1);
		const result = insertTracks(state, [track('x'), track('y')], 'b');
		expect(ids(result)).toEqual(['a', 'x', 'y', 'b', 'c']);
		expect(currentTrack(result)?.id).toBe('b');
		expect(result.currentIndex).toBe(3);
	});

	it('keeps the current index when insertion is after the current track', () => {
		const result = insertTracks(queue(['a', 'b', 'c'], 0), [track('x')], 'c');
		expect(ids(result)).toEqual(['a', 'b', 'x', 'c']);
		expect(result.currentIndex).toBe(0);
	});

	it('keeps existing slot metadata instead of the incoming copy', () => {
		const state: QueueState = {
			tracks: [track('a'), track('b', { title: 'Original', artworkUrl: '/art/b' }), track('c')],
			currentIndex: 0
		};
		const result = insertTracks(state, [track('b', { title: 'Replaced', artworkUrl: null })], null);
		expect(ids(result)).toEqual(['a', 'c', 'b']);
		expect(result.tracks[2]).toBe(state.tracks[1]);
		expect(result.tracks[2]?.title).toBe('Original');
	});

	it('uses incoming metadata for tracks not yet in the queue', () => {
		const added = track('x', { title: 'Fresh', album: 'Alb' });
		const result = insertTracks(queue(['a']), [added], null);
		expect(result.tracks[1]).toBe(added);
	});

	it('moves several existing tracks together in input order', () => {
		const state = queue(['a', 'b', 'c', 'd', 'e'], 2);
		const result = insertTracks(state, [track('e'), track('a'), track('d')], 'c');
		expect(ids(result)).toEqual(['b', 'e', 'a', 'd', 'c']);
		expect(currentTrack(result)?.id).toBe('c');
	});

	it('appends moving tracks at the end when beforeId is null (explicit and default)', () => {
		const state = queue(['a', 'b', 'c'], 0);
		expect(ids(insertTracks(state, [track('a'), track('x')], null))).toEqual(['b', 'c', 'a', 'x']);
		expect(ids(insertTracks(state, [track('x')]))).toEqual(['a', 'b', 'c', 'x']);
	});

	it('moves once for duplicate inputs, using the first occurrence order', () => {
		const state = queue(['a', 'b', 'c'], 0);
		const result = insertTracks(state, [track('x'), track('c'), track('x'), track('c'), track('y')], 'b');
		expect(ids(result)).toEqual(['a', 'x', 'c', 'y', 'b']);
	});

	it('preserves current identity when the current track is among those moved', () => {
		const state = queue(['a', 'b', 'c', 'd'], 1);
		const result = insertTracks(state, [track('b'), track('a')], null);
		expect(ids(result)).toEqual(['c', 'd', 'b', 'a']);
		expect(currentTrack(result)?.id).toBe('b');
		expect(result.currentIndex).toBe(2);
	});

	it('fills an empty queue and selects the first track', () => {
		const result = insertTracks(createQueue([]), [track('x'), track('y')], null);
		expect(ids(result)).toEqual(['x', 'y']);
		expect(result.currentIndex).toBe(0);
	});

	it('returns the same state for empty input, and for an empty queue with no tracks', () => {
		const empty = createQueue([]);
		expect(insertTracks(empty, [], null)).toBe(empty);
		const state = queue(['a', 'b']);
		expect(insertTracks(state, [], 'b')).toBe(state);
	});

	it('ignores tracks with an empty id', () => {
		const state = queue(['a']);
		expect(insertTracks(state, [track('')], null)).toBe(state);
		expect(ids(insertTracks(state, [track(''), track('x')], null))).toEqual(['a', 'x']);
	});

	it('is a no-op (same object) for a stale destination, even with new tracks', () => {
		const state = queue(['a', 'b']);
		expect(insertTracks(state, [track('x')], 'gone')).toBe(state);
		expect(insertTracks(createQueue([]), [track('x')], 'gone')).toEqual(createQueue([]));
	});

	it('is a no-op when the destination is inside the moving set', () => {
		const state = queue(['a', 'b', 'c'], 0);
		expect(insertTracks(state, [track('b'), track('c')], 'c')).toBe(state);
		expect(insertTracks(state, [track('x'), track('b')], 'b')).toBe(state);
	});

	it('returns the identical state when the order would not change', () => {
		const state = queue(['a', 'b', 'c', 'd'], 2);
		expect(insertTracks(state, [track('b'), track('c')], 'd')).toBe(state);
		expect(insertTracks(state, [track('d')], null)).toBe(state);
		expect(insertTracks(state, [track('a')], 'b')).toBe(state);
	});
});

describe('insertTracksNext', () => {
	it('does not mutate the state or the incoming array', () => {
		const state = queue(['a', 'b', 'c'], 0);
		const incoming = [track('c'), track('x')];
		const stateSnapshot = structuredClone(state);
		const incomingSnapshot = structuredClone(incoming);
		insertTracksNext(state, incoming);
		expect(state).toEqual(stateSnapshot);
		expect(incoming).toEqual(incomingSnapshot);
	});

	it('places new and existing tracks right after the current one in input order', () => {
		const state = queue(['a', 'b', 'c', 'd', 'e'], 1);
		const result = insertTracksNext(state, [track('e'), track('x'), track('c')]);
		expect(ids(result)).toEqual(['a', 'b', 'e', 'x', 'c', 'd']);
		expect(currentTrack(result)?.id).toBe('b');
		expect(result.currentIndex).toBe(1);
	});

	it('skips the moving current track', () => {
		const state = queue(['a', 'b', 'c'], 1);
		const result = insertTracksNext(state, [track('b'), track('x')]);
		expect(ids(result)).toEqual(['a', 'b', 'x', 'c']);
		expect(result.currentIndex).toBe(1);
	});

	it('returns the same state when only the current track is supplied', () => {
		const state = queue(['a', 'b', 'c'], 1);
		expect(insertTracksNext(state, [track('b')])).toBe(state);
	});

	it('moves a selection from before the current track after it, keeping current identity', () => {
		const state = queue(['a', 'b', 'c', 'd'], 2);
		const result = insertTracksNext(state, [track('a'), track('b')]);
		expect(ids(result)).toEqual(['c', 'a', 'b', 'd']);
		expect(currentTrack(result)?.id).toBe('c');
		expect(result.currentIndex).toBe(0);
	});

	it('handles a selection spanning both sides of the current track', () => {
		const state = queue(['a', 'b', 'c', 'd', 'e'], 2);
		const result = insertTracksNext(state, [track('e'), track('a'), track('c')]);
		expect(ids(result)).toEqual(['b', 'c', 'e', 'a', 'd']);
		expect(currentTrack(result)?.id).toBe('c');
	});

	it('anchors past tracks that are themselves moving', () => {
		const state = queue(['a', 'b', 'c', 'd', 'e'], 0);
		const result = insertTracksNext(state, [track('b'), track('x')]);
		expect(ids(result)).toEqual(['a', 'b', 'x', 'c', 'd', 'e']);
	});

	it('moves duplicate inputs once', () => {
		const state = queue(['a', 'b', 'c'], 0);
		const result = insertTracksNext(state, [track('c'), track('c'), track('x'), track('x')]);
		expect(ids(result)).toEqual(['a', 'c', 'x', 'b']);
	});

	it('returns the identical state when tracks are already immediately next', () => {
		const state = queue(['a', 'b', 'c', 'd'], 0);
		expect(insertTracksNext(state, [track('b'), track('c')])).toBe(state);
	});

	it('appends at the end (null destination) when the current track is last', () => {
		const state = queue(['a', 'b', 'c'], 2);
		const result = insertTracksNext(state, [track('a'), track('x')]);
		expect(ids(result)).toEqual(['b', 'c', 'a', 'x']);
		expect(currentTrack(result)?.id).toBe('c');
		expect(result.currentIndex).toBe(1);
	});

	it('appends at the end when every later track is moving', () => {
		const state = queue(['a', 'b', 'c'], 0);
		const result = insertTracksNext(state, [track('c'), track('b'), track('x')]);
		expect(ids(result)).toEqual(['a', 'c', 'b', 'x']);
	});

	it('fills an empty queue and selects the first track', () => {
		const result = insertTracksNext(createQueue([]), [track('x'), track('y')]);
		expect(ids(result)).toEqual(['x', 'y']);
		expect(result.currentIndex).toBe(0);
	});

	it('returns the same state for empty input', () => {
		const state = queue(['a', 'b']);
		expect(insertTracksNext(state, [])).toBe(state);
		const empty = createQueue([]);
		expect(insertTracksNext(empty, [])).toBe(empty);
	});

	it('keeps existing metadata for moved rows', () => {
		const state: QueueState = {
			tracks: [track('a'), track('b'), track('c', { title: 'Kept' })],
			currentIndex: 0
		};
		const result = insertTracksNext(state, [track('c', { title: 'Other' })]);
		expect(ids(result)).toEqual(['a', 'c', 'b']);
		expect(result.tracks[1]).toBe(state.tracks[2]);
	});
});
