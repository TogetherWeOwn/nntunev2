import { describe, expect, it } from 'vitest';
import {
	addTrack,
	clearQueue,
	createQueue,
	currentTrack,
	DEMO_TRACKS,
	moveTrack,
	nextTrack,
	previousTrack,
	restoreQueue,
	selectTrack,
	serializeQueue
} from './queue.js';

describe('demo fixture', () => {
	it('ships exactly three tracks, the first on the plain demo stream', () => {
		expect(DEMO_TRACKS).toHaveLength(3);
		expect(new Set(DEMO_TRACKS.map((t) => t.id)).size).toBe(3);
		// player-seek.spec.ts asserts the demo audio src verbatim.
		expect(DEMO_TRACKS[0]?.streamUrl).toBe('/api/demo/audio');
	});
});

describe('queue navigation', () => {
	it('plays through next/prev and clamps at both ends (no wrap)', () => {
		let q = createQueue(DEMO_TRACKS);
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[0]?.id);

		q = nextTrack(q);
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[1]?.id);
		q = nextTrack(q);
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[2]?.id);
		// At the end: stays put, playback ends instead of wrapping.
		expect(nextTrack(q)).toEqual(q);

		q = previousTrack(q);
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[1]?.id);
		q = previousTrack(previousTrack(q));
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[0]?.id);
		expect(previousTrack(q)).toEqual(q);
	});

	it('selects by index and ignores out-of-range indexes', () => {
		const q = createQueue(DEMO_TRACKS);
		expect(currentTrack(selectTrack(q, 2))?.id).toBe(DEMO_TRACKS[2]?.id);
		expect(selectTrack(q, 99)).toEqual(q);
		expect(selectTrack(q, -1)).toEqual(q);
	});

	it('is a no-op on an empty queue', () => {
		const q = createQueue([]);
		expect(currentTrack(q)).toBeNull();
		expect(nextTrack(q)).toEqual(q);
		expect(previousTrack(q)).toEqual(q);
	});
});

describe('queue editing', () => {
	it('adds tracks, ignores duplicates, and starts playback at the first add', () => {
		let q = clearQueue(createQueue(DEMO_TRACKS));
		expect(q.tracks).toEqual([]);
		expect(currentTrack(q)).toBeNull();

		q = addTrack(q, DEMO_TRACKS[1]!);
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[1]?.id);
		q = addTrack(q, DEMO_TRACKS[1]!);
		expect(q.tracks).toHaveLength(1);
		q = addTrack(q, DEMO_TRACKS[0]!);
		expect(q.tracks.map((t) => t.id)).toEqual([DEMO_TRACKS[1]?.id, DEMO_TRACKS[0]?.id]);
		// Adding never steals the current track.
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[1]?.id);
	});

	it('reorders while the current track identity follows the move', () => {
		let q = selectTrack(createQueue(DEMO_TRACKS), 0);
		q = moveTrack(q, 0, 2);
		expect(q.tracks.map((t) => t.id)).toEqual([
			DEMO_TRACKS[1]?.id,
			DEMO_TRACKS[2]?.id,
			DEMO_TRACKS[0]?.id
		]);
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[0]?.id);
		expect(q.currentIndex).toBe(2);

		// Moving another track across the current one keeps the same track current.
		q = moveTrack(q, 0, 2);
		expect(currentTrack(q)?.id).toBe(DEMO_TRACKS[0]?.id);
		expect(q.currentIndex).toBe(1);
	});

	it('ignores out-of-range moves', () => {
		const q = createQueue(DEMO_TRACKS);
		expect(moveTrack(q, -1, 1)).toEqual(q);
		expect(moveTrack(q, 0, 3)).toEqual(q);
		expect(moveTrack(q, 1, 1)).toEqual(q);
	});
});

describe('in-session persistence', () => {
	it('round-trips order and current track through serialize/restore', () => {
		const q = nextTrack(moveTrack(createQueue(DEMO_TRACKS), 2, 0));
		const restored = restoreQueue(serializeQueue(q), DEMO_TRACKS);
		expect(restored).toEqual(q);
	});

	it('falls back to the fixture queue on junk or unknown ids', () => {
		const fallback = createQueue(DEMO_TRACKS);
		expect(restoreQueue(null, DEMO_TRACKS)).toEqual(fallback);
		expect(restoreQueue('not json', DEMO_TRACKS)).toEqual(fallback);
		expect(restoreQueue('{"orderIds":["nope"],"currentId":"nope"}', DEMO_TRACKS)).toEqual(
			fallback
		);
	});

	it('restores a cleared queue instead of resurrecting the fixture', () => {
		const cleared = clearQueue(createQueue(DEMO_TRACKS));
		expect(restoreQueue(serializeQueue(cleared), DEMO_TRACKS)).toEqual(cleared);
	});
});
