import { describe, expect, it } from 'vitest';
import * as queueModule from './queue.js';
import {
	createQueue,
	moveTrack,
	nextTrack,
	previousTrack,
	selectTrack,
	type QueueState,
	type Track
} from './queue.js';

/**
 * Acceptance-owned contract; Builder must not edit this file.
 * Add `naturalEndPolicy(queue, context)` to queue.ts (or re-export it there).
 * Unlike explicit nextTrack/previousTrack, this pure policy may replay/wrap.
 * `play` means play the result from zero; `radio` delegates to existing radio;
 * `ignore` does nothing; `stop` completes without starting another track.
 * Host event validity, transport effects, timer/resume and a11y are also tested
 * in tests/repeat-modes.spec.ts. No source-string assertions or native loop.
 * A missing export is an assertion failure, NOT behavioral RED evidence; use
 * the paired browser suite for actual baseline playback mismatches.
 */
type Context = {
	repeat: 'off' | 'one' | 'queue';
	radioActive: boolean;
	continueRadio: boolean;
	/** Playback intent before completion, not the native ended element's paused flag. */
	playing: boolean;
	validEnd: boolean;
	sleepComplete: boolean;
};
type Result = { queue: QueueState; action: 'play' | 'stop' | 'radio' | 'ignore' };
const context: Context = {
	repeat: 'off', radioActive: false, continueRadio: false,
	playing: true, validEnd: true, sleepComplete: false
};
const tracks: Track[] = [
	{ id: 'repeat-a', title: 'Repeat A', artist: 'Fixture', streamUrl: '/a' },
	{ id: 'repeat-b', title: 'Repeat B', artist: 'Fixture', streamUrl: '/b' },
	{ id: 'repeat-c', title: 'Repeat C', artist: 'Fixture', streamUrl: '/c' }
];

function policy(queue: QueueState, patch: Partial<Context> = {}): Result {
	const fn = (queueModule as unknown as {
		naturalEndPolicy?: (queue: QueueState, context: Context) => Result;
	}).naturalEndPolicy;
	expect(fn, 'Implement the separate natural-end policy, not wrapping explicit Next').toBeTypeOf('function');
	return fn!(queue, { ...context, ...patch });
}

function frozenQueue(index = 0): QueueState {
	const queue = selectTrack(createQueue(tracks), index);
	queue.tracks.forEach(Object.freeze);
	Object.freeze(queue.tracks);
	return Object.freeze(queue);
}

describe('natural completion policy (acceptance)', () => {
	it.each([
		['REP-U01', 0, 'play', 1],
		['REP-U02', 2, 'stop', 2]
	] as const)('%s Off preserves advance then stop', (_id, index, action, wantIndex) => {
		const queue = frozenQueue(index);
		expect(policy(queue)).toEqual({ queue: { tracks, currentIndex: wantIndex }, action });
	});

	it.each(['one', 'queue'] as const)('REP-U03 %s beats continuation preference', (repeat) => {
		const queue = frozenQueue(2);
		const result = policy(queue, { repeat, continueRadio: true });
		expect(result.action).toBe('play');
		expect(result.queue.currentIndex).toBe(repeat === 'one' ? 2 : 0);
		expect(result.queue.tracks).toEqual(tracks);
	});

	it('REP-U04 One preserves the current identity in the middle', () => {
		const queue = frozenQueue(1);
		expect(policy(queue, { repeat: 'one' })).toEqual({ queue, action: 'play' });
	});

	it('REP-U05 Queue follows edited order and wraps its last item, without mutation', () => {
		const edited = moveTrack(createQueue(tracks), 2, 0);
		const before = JSON.stringify(edited);
		const first = policy(edited, { repeat: 'queue' });
		expect(first).toEqual({ queue: { tracks: [tracks[2], tracks[0], tracks[1]], currentIndex: 2 }, action: 'play' });
		const wrapped = policy(first.queue, { repeat: 'queue' });
		expect(wrapped).toEqual({ queue: { tracks: [tracks[2], tracks[0], tracks[1]], currentIndex: 0 }, action: 'play' });
		expect(JSON.stringify(edited)).toBe(before);
	});

	it.each(['one', 'queue'] as const)('REP-U06 %s replays a singleton', (repeat) => {
		const queue = createQueue([tracks[1]!]);
		expect(policy(queue, { repeat })).toEqual({ queue, action: 'play' });
	});

	it('REP-U07 Off honors continuation only at the tail', () => {
		expect(policy(frozenQueue(0), { continueRadio: true }).action).toBe('play');
		expect(policy(frozenQueue(2), { continueRadio: true })).toEqual({ queue: frozenQueue(2), action: 'radio' });
	});

	it.each(['off', 'one', 'queue'] as const)('REP-U08 active radio retains its pass under %s', (repeat) => {
		const queue = frozenQueue(2);
		expect(policy(queue, { repeat, radioActive: true })).toEqual({ queue, action: 'radio' });
	});

	it.each(['off', 'one', 'queue'] as const)('REP-U09 sleep completion overrides %s and radio', (repeat) => {
		const queue = frozenQueue(2);
		expect(policy(queue, { repeat, sleepComplete: true, continueRadio: true, radioActive: true }))
			.toEqual({ queue, action: 'stop' });
	});

	it.each(['off', 'one', 'queue'] as const)('REP-U10 %s ignores paused/stale/empty ends', (repeat) => {
		const queue = frozenQueue(1);
		for (const patch of [{ playing: false }, { validEnd: false }]) {
			expect(policy(queue, { repeat, ...patch })).toEqual({ queue, action: 'ignore' });
		}
		const empty = createQueue([]);
		expect(policy(empty, { repeat, continueRadio: true })).toEqual({ queue: empty, action: 'ignore' });
	});

	it('REP-U11 explicit navigation remains clamped (known-good control)', () => {
		const first = frozenQueue(0);
		const last = frozenQueue(2);
		expect(nextTrack(first).currentIndex).toBe(1);
		expect(nextTrack(last)).toEqual(last);
		expect(previousTrack(first)).toEqual(first);
		expect(previousTrack(last).currentIndex).toBe(1);
	});
});
