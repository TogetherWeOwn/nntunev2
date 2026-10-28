import { describe, expect, it } from 'vitest';
import { readTimeline, seekTarget, type TimelineMedia } from './libraryTimeline.js';

function media(currentTime: number, duration: number, ranges: Array<[number, number]>): TimelineMedia {
	return {
		currentTime,
		duration,
		seekable: { length: ranges.length, start: (i) => ranges[i]![0], end: (i) => ranges[i]![1] }
	};
}

describe('readTimeline', () => {
	it('reads finite coordinates and seekable ranges from the element', () => {
		expect(readTimeline(media(65.7, 185, [[0, 185]]))).toEqual({
			currentTime: 65.7,
			duration: 185,
			ranges: [[0, 185]],
			seekable: true
		});
	});

	it.each([Number.NaN, Number.POSITIVE_INFINITY, -1])('treats duration %s as unknown', (duration) => {
		const snap = readTimeline(media(12, duration, [[0, 100]]));
		expect(snap.duration).toBeNull();
		expect(snap.ranges).toEqual([]);
		expect(snap.seekable).toBe(false);
	});

	it('is not seekable without ranges or with a zero duration', () => {
		expect(readTimeline(media(0, 120, [])).seekable).toBe(false);
		expect(readTimeline(media(0, 0, [[0, 0]])).seekable).toBe(false);
	});

	it('clips ranges to the duration and clamps the position', () => {
		const snap = readTimeline(media(500, 100, [[-5, 40], [60, 400]]));
		expect(snap.ranges).toEqual([[0, 40], [60, 100]]);
		expect(snap.currentTime).toBe(100);
	});

	it('reads a non-finite position as zero and survives throwing getters', () => {
		expect(readTimeline(media(Number.NaN, 10, [[0, 10]])).currentTime).toBe(0);
		const broken = {
			get currentTime(): number {
				throw new Error('detached');
			},
			duration: 10,
			seekable: { length: 0, start: () => 0, end: () => 0 }
		} as TimelineMedia;
		expect(readTimeline(broken).duration).toBeNull();
	});
});

describe('seekTarget', () => {
	const snap = readTimeline(media(0, 120, [[0, 30], [60, 120]]));

	it('clamps to zero and the finite duration', () => {
		const whole = readTimeline(media(0, 200, [[0, 200]]));
		expect(seekTarget(whole, -5)).toBe(0);
		expect(seekTarget(whole, 250)).toBe(200);
	});

	it('keeps a target that is inside a seekable range', () => {
		expect(seekTarget(snap, 70)).toBe(70);
	});

	it('moves a target in a gap to the nearest range edge', () => {
		expect(seekTarget(snap, 35)).toBe(30);
		expect(seekTarget(snap, 55)).toBe(60);
	});

	it('returns null when nothing is seekable or the request is not finite', () => {
		expect(seekTarget(readTimeline(media(0, Number.NaN, [])), 10)).toBeNull();
		expect(seekTarget(snap, Number.NaN)).toBeNull();
	});
});
