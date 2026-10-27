import { describe, expect, it } from 'vitest';
import { describeJump, jumpMedia } from './relativeJump.js';

function media(currentTime: number, duration: number, refuse = false) {
	let time = currentTime;
	return {
		get currentTime() {
			return time;
		},
		set currentTime(next: number) {
			if (refuse) throw new DOMException('refused', 'InvalidStateError');
			time = next;
		},
		duration
	};
}

describe('jumpMedia', () => {
	it('moves exactly ten seconds and keeps fractions', () => {
		const a = media(30, 120);
		expect(jumpMedia(a, 'back')).toEqual({ kind: 'jumped', target: 20, clamped: false });
		expect(a.currentTime).toBe(20);
		const b = media(30.5, 120);
		expect(jumpMedia(b, 'forward')).toEqual({ kind: 'jumped', target: 40.5, clamped: false });
	});

	it('clamps to zero and duration', () => {
		expect(jumpMedia(media(3, 120), 'back')).toEqual({ kind: 'jumped', target: 0, clamped: true });
		expect(jumpMedia(media(115, 120), 'forward')).toEqual({
			kind: 'jumped',
			target: 120,
			clamped: true
		});
	});

	it('reports bounds as no-ops without writing', () => {
		const start = media(0, 120);
		expect(jumpMedia(start, 'back')).toEqual({ kind: 'boundary', edge: 'start' });
		const end = media(120, 120);
		expect(jumpMedia(end, 'forward')).toEqual({ kind: 'boundary', edge: 'end' });
		expect(start.currentTime).toBe(0);
		expect(end.currentTime).toBe(120);
	});

	it.each([
		['NaN duration', 30, Number.NaN],
		['infinite duration', 30, Number.POSITIVE_INFINITY],
		['zero duration', 30, 0],
		['negative duration', 30, -1],
		['NaN time', Number.NaN, 120],
		['infinite time', Number.POSITIVE_INFINITY, 120],
		['negative time', -5, 120],
		['time past duration', 999, 120]
	])('refuses %s without writing', (_name, time, duration) => {
		const m = media(time, duration);
		expect(jumpMedia(m, 'forward')).toEqual({ kind: 'unavailable' });
		expect(jumpMedia(m, 'back')).toEqual({ kind: 'unavailable' });
		expect(Object.is(m.currentTime, time)).toBe(true);
	});

	it('catches a refused assignment once and keeps the position', () => {
		const m = media(30, 120, true);
		expect(jumpMedia(m, 'back')).toEqual({ kind: 'refused' });
		expect(m.currentTime).toBe(30);
	});

	it('treats unreadable media as unavailable', () => {
		const m = {
			get currentTime(): number {
				throw new Error('detached');
			},
			set currentTime(_v: number) {},
			duration: 120
		};
		expect(jumpMedia(m, 'back')).toEqual({ kind: 'unavailable' });
	});
});

describe('describeJump', () => {
	it('names a real jump and never claims one for no-ops', () => {
		expect(describeJump('back', { kind: 'jumped', target: 20, clamped: false })).toBe(
			'Back 10 seconds, now at 0:20.'
		);
		expect(describeJump('back', { kind: 'jumped', target: 0, clamped: true })).toBe(
			'Back to the start, 0:00.'
		);
		expect(describeJump('forward', { kind: 'jumped', target: 120, clamped: true })).toBe(
			'Forward to the end, 2:00.'
		);
		for (const outcome of [
			{ kind: 'boundary', edge: 'start' },
			{ kind: 'boundary', edge: 'end' },
			{ kind: 'unavailable' },
			{ kind: 'refused' }
		] as const) {
			expect(describeJump('forward', outcome)).not.toMatch(/^(jumped|seeked|moved to)/i);
			expect(describeJump('forward', outcome)).not.toMatch(/10 second/i);
		}
	});
});
