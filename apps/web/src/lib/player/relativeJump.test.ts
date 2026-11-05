import { describe, expect, it } from 'vitest';

import { RELATIVE_JUMP_SECONDS, describeJump, jumpMedia } from './relativeJump.js';

function ranges(...spans: [number, number][]): TimeRanges {
	return { length: spans.length, start: (i: number) => spans[i][0], end: (i: number) => spans[i][1] };
}

function media(currentTime: number, duration: number, seekable = ranges([0, duration])) {
	return { currentTime, duration, seekable };
}

describe('jumpMedia', () => {
	it('moves forward and back by the fixed step', () => {
		const el = media(30, 120);
		expect(jumpMedia(el, 'forward')).toEqual({ kind: 'jumped', target: 40, clamped: false });
		expect(el.currentTime).toBe(40);
		expect(jumpMedia(el, 'back')).toEqual({ kind: 'jumped', target: 30, clamped: false });
		expect(el.currentTime).toBe(30);
		expect(RELATIVE_JUMP_SECONDS).toBe(10);
	});

	it('clamps to the start and to the end', () => {
		const start = media(4, 120);
		expect(jumpMedia(start, 'back')).toEqual({ kind: 'jumped', target: 0, clamped: true });
		expect(start.currentTime).toBe(0);
		const end = media(115, 120);
		expect(jumpMedia(end, 'forward')).toEqual({ kind: 'jumped', target: 120, clamped: true });
		expect(end.currentTime).toBe(120);
	});

	it('reports a boundary without writing when already at an edge', () => {
		expect(jumpMedia(media(0, 120), 'back')).toEqual({ kind: 'boundary', edge: 'start' });
		expect(jumpMedia(media(120, 120), 'forward')).toEqual({ kind: 'boundary', edge: 'end' });
		// Float readback just short of or past the end still counts as the end.
		expect(jumpMedia(media(119.9999, 120), 'forward')).toEqual({ kind: 'boundary', edge: 'end' });
		expect(jumpMedia(media(120.0001, 120), 'forward')).toEqual({ kind: 'boundary', edge: 'end' });
		// The opposite direction still moves away from an edge.
		expect(jumpMedia(media(0, 120), 'forward')).toMatchObject({ kind: 'jumped', target: 10 });
		const past = media(120.0001, 120);
		expect(jumpMedia(past, 'back')).toMatchObject({ kind: 'jumped' });
		expect(past.currentTime).toBeCloseTo(110, 3);
	});

	it('seeks into a later seekable range', () => {
		const el = media(30, 120, ranges([0, 5], [35, 120]));
		expect(jumpMedia(el, 'forward')).toMatchObject({ kind: 'jumped', target: 40 });
	});

	it.each([
		['no range support', ranges([0, 0])],
		['no seekable ranges yet', ranges()],
		['target outside every range', ranges([0, 35], [50, 120])]
	])('leaves unseekable media untouched: %s', (_label, seekable) => {
		const el = media(30, 120, seekable);
		expect(jumpMedia(el, 'forward')).toEqual({ kind: 'unavailable' });
		expect(el.currentTime).toBe(30);
	});

	it.each([
		['unknown duration', 0, Number.NaN],
		['live stream', 5, Number.POSITIVE_INFINITY],
		['zero duration', 0, 0],
	])('leaves media untouched for %s', (_label, currentTime, duration) => {
		const el = media(currentTime, duration);
		expect(jumpMedia(el, 'forward')).toEqual({ kind: 'unavailable' });
		expect(el.currentTime).toBe(currentTime);
	});

	it('reports a setter that silently ignores the write as refused', () => {
		const el = {
			duration: 120,
			seekable: ranges([0, 120]),
			get currentTime() {
				return 30;
			},
			set currentTime(_value: number) {}
		};
		expect(jumpMedia(el, 'forward')).toEqual({ kind: 'refused' });
	});

	it('reports a refused assignment', () => {
		const el = {
			duration: 120,
			seekable: ranges([0, 120]),
			get currentTime() {
				return 30;
			},
			set currentTime(_value: number) {
				throw new DOMException('nope', 'InvalidStateError');
			}
		};
		expect(jumpMedia(el, 'forward')).toEqual({ kind: 'refused' });
	});
});

describe('describeJump', () => {
	it('describes each outcome', () => {
		expect(describeJump('forward', { kind: 'jumped', target: 40, clamped: false })).toBe(
			'Forward 10 seconds, now at 0:40.'
		);
		expect(describeJump('back', { kind: 'jumped', target: 0, clamped: true })).toBe(
			'Back to the start, 0:00.'
		);
		expect(describeJump('forward', { kind: 'jumped', target: 120, clamped: true })).toBe(
			'Forward to the end, 2:00.'
		);
		expect(describeJump('back', { kind: 'boundary', edge: 'start' })).toBe('Already at the start.');
		expect(describeJump('forward', { kind: 'unavailable' })).toBe(
			'Cannot jump: this track cannot seek yet.'
		);
		expect(describeJump('back', { kind: 'refused' })).toBe('The player did not accept that jump.');
	});
});
