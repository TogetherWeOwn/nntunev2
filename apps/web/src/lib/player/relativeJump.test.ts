import { afterEach, describe, expect, it, vi } from 'vitest';
import { describeJump, jumpMedia, watchJump } from './relativeJump.js';

function media(currentTime: number, duration: number) {
	let time = currentTime;
	return {
		attempts: [] as number[],
		refuse: false,
		ignore: false,
		readbackOffset: 0,
		seeking: false,
		seekable: undefined as TimeRanges | undefined,
		get currentTime() {
			return time;
		},
		set currentTime(next: number) {
			this.attempts.push(next);
			if (this.refuse) throw new DOMException('refused', 'InvalidStateError');
			if (!this.ignore) time = next + this.readbackOffset;
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

	it.each([
		{ position: 0, direction: 'forward' as const, target: 10 },
		{ position: 120, direction: 'back' as const, target: 110 }
	])('moves $direction away from the boundary at $position', ({ position, direction, target }) => {
		const m = media(position, 120);
		expect(jumpMedia(m, direction)).toEqual({ kind: 'jumped', target, clamped: false });
		expect(m.currentTime).toBe(target);
	});

	it('reports bounds as no-ops without writing', () => {
		const start = media(0, 120);
		expect(jumpMedia(start, 'back')).toEqual({ kind: 'boundary', edge: 'start' });
		const end = media(120, 120);
		expect(jumpMedia(end, 'forward')).toEqual({ kind: 'boundary', edge: 'end' });
		expect(start.currentTime).toBe(0);
		expect(end.currentTime).toBe(120);
		expect(start.attempts).toEqual([]);
		expect(end.attempts).toEqual([]);
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
		expect(m.attempts).toEqual([]);
	});

	it.each(['back', 'forward'] as const)('catches a refused %s assignment once and keeps the position', (direction) => {
		const m = media(30, 120);
		m.refuse = true;
		expect(jumpMedia(m, direction)).toEqual({ kind: 'refused' });
		expect(m.attempts).toEqual([direction === 'back' ? 20 : 40]);
		expect(m.currentTime).toBe(30);
	});

	it.each(['back', 'forward'] as const)('reports a silently ignored %s assignment as refused', (direction) => {
		const m = media(30, 120);
		m.ignore = true;
		expect(jumpMedia(m, direction)).toEqual({ kind: 'refused' });
		expect(m.attempts).toEqual([direction === 'back' ? 20 : 40]);
		expect(m.currentTime).toBe(30);
	});

	it.each(['back', 'forward'] as const)('does not write when seekable excludes the %s target', (direction) => {
		const m = media(30, 120);
		m.seekable = { length: 1, start: () => 25, end: () => 35 };
		expect(jumpMedia(m, direction)).toEqual({ kind: 'unavailable' });
		expect(m.attempts).toEqual([]);
		expect(m.currentTime).toBe(30);
	});

	it.each(['back', 'forward'] as const)('writes the %s target covered only by the second seekable interval', (direction) => {
		const m = media(30, 120);
		const ranges = [[0, 10], [20, 120]];
		m.seekable = {
			length: ranges.length,
			start: (index) => ranges[index][0],
			end: (index) => ranges[index][1]
		};
		const target = direction === 'back' ? 20 : 40;
		expect(jumpMedia(m, direction)).toEqual({ kind: 'jumped', target, clamped: false });
		expect(m.attempts).toEqual([target]);
		expect(m.currentTime).toBe(target);
	});

	it.each([-0.1, -0.051, -0.05, -0.03, 0.03, 0.05, 0.051, 0.1])(
		'immediate readback enforces the 50ms tolerance for offset %s', (offset) => {
			const m = media(30, 120);
			m.readbackOffset = offset;
			expect(jumpMedia(m, 'forward')).toEqual(Math.abs(offset) <= 0.05
				? { kind: 'jumped', target: 40, clamped: false }
				: { kind: 'refused' });
			expect(m.attempts).toEqual([40]);
			expect(m.currentTime).toBe(40 + offset);
		}
	);

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

describe('watchJump', () => {
	class NativeMedia extends EventTarget {
		duration = 120;
		seeking = false;
		position = 30;
		paused = true;
		pause = vi.fn(() => { this.paused = true; });
		play = vi.fn(() => { this.paused = false; return Promise.resolve(); });
		attempts: number[] = [];
		get currentTime() { return this.position; }
		set currentTime(value: number) {
			this.attempts.push(value);
			this.seeking = true;
		}
		settle(position: number) {
			this.position = position;
			this.seeking = false;
			this.dispatchEvent(new Event('seeked'));
		}
	}

	afterEach(() => {
		vi.useRealTimers();
		vi.restoreAllMocks();
	});

	function begin(direction: 'back' | 'forward' = 'forward', paused = true) {
		vi.useFakeTimers();
		const m = new NativeMedia();
		m.paused = paused;
		const outcome = jumpMedia(m, direction);
		expect(outcome).toEqual({ kind: 'pending', target: direction === 'back' ? 20 : 40, clamped: false });
		if (outcome.kind !== 'pending') throw new Error('expected pending seek');
		const publish = vi.fn();
		const removed = vi.spyOn(m, 'removeEventListener');
		const cancel = watchJump(m as unknown as HTMLMediaElement, outcome, publish);
		expect(publish).not.toHaveBeenCalled();
		return { m, publish, removed, cancel };
	}

	it('publishes success only when seeked reaches the target', () => {
		const { m, publish, cancel } = begin();
		vi.advanceTimersByTime(1999);
		expect(publish).not.toHaveBeenCalled();
		m.settle(40);
		expect(publish).toHaveBeenCalledExactlyOnceWith({ kind: 'jumped', target: 40, clamped: false });
		vi.advanceTimersByTime(2000);
		expect(publish).toHaveBeenCalledTimes(1);
		expect(m.attempts).toEqual([40]);
		cancel();
	});

	it.each([
		{ direction: 'back' as const, target: 20, paused: false },
		{ direction: 'forward' as const, target: 40, paused: false },
		{ direction: 'back' as const, target: 20, paused: true },
		{ direction: 'forward' as const, target: 40, paused: true }
	])('$direction settlement preserves paused=$paused without pausing or playing', ({ direction, target, paused }) => {
		const { m, publish, cancel } = begin(direction, paused);
		expect(m.paused).toBe(paused);
		expect(m.pause).not.toHaveBeenCalled();
		expect(m.play).not.toHaveBeenCalled();
		m.settle(target);
		expect(publish).toHaveBeenCalledExactlyOnceWith({ kind: 'jumped', target, clamped: false });
		expect(m.paused).toBe(paused);
		expect(m.pause).not.toHaveBeenCalled();
		expect(m.play).not.toHaveBeenCalled();
		expect(m.currentTime).toBe(target);
		expect(m.attempts).toEqual([target]);
		vi.advanceTimersByTime(2000);
		expect(publish).toHaveBeenCalledTimes(1);
		expect(m.paused).toBe(paused);
		expect(m.pause).not.toHaveBeenCalled();
		expect(m.play).not.toHaveBeenCalled();
		cancel();
	});

	it.each([-0.1, -0.051, -0.05, -0.03, 0.03, 0.05, 0.051, 0.1])(
		'pending readback enforces the 50ms tolerance for offset %s', (offset) => {
			const { m, publish, cancel } = begin();
			m.settle(40 + offset);
			expect(publish).toHaveBeenCalledExactlyOnceWith(Math.abs(offset) <= 0.05
				? { kind: 'jumped', target: 40, clamped: false }
				: { kind: 'refused' });
			expect(m.attempts).toEqual([40]);
			vi.advanceTimersByTime(2000);
			expect(publish).toHaveBeenCalledTimes(1);
			cancel();
		}
	);

	it('reports a settled position mismatch as refused', () => {
		const { m, publish, cancel } = begin();
		m.settle(35);
		expect(publish).toHaveBeenCalledExactlyOnceWith({ kind: 'refused' });
		expect(m.attempts).toEqual([40]);
		cancel();
	});

	it('reports a native error as refused', () => {
		const { m, publish, cancel } = begin();
		m.dispatchEvent(new Event('error'));
		expect(publish).toHaveBeenCalledExactlyOnceWith({ kind: 'refused' });
		expect(m.attempts).toEqual([40]);
		cancel();
	});

	it('times out once at 2000ms and retires both listeners', () => {
		const { m, publish, removed, cancel } = begin();
		vi.advanceTimersByTime(1999);
		expect(publish).not.toHaveBeenCalled();
		vi.advanceTimersByTime(1);
		expect(publish).toHaveBeenCalledExactlyOnceWith({ kind: 'refused' });
		expect(removed.mock.calls.map(([type]) => type)).toEqual(expect.arrayContaining(['seeked', 'error']));
		m.settle(40);
		m.dispatchEvent(new Event('error'));
		vi.advanceTimersByTime(2000);
		expect(publish).toHaveBeenCalledTimes(1);
		expect(m.attempts).toEqual([40]);
		cancel();
	});

	it('cancellation suppresses later events and timeout', () => {
		const { m, publish, removed, cancel } = begin();
		cancel();
		expect(removed.mock.calls.map(([type]) => type)).toEqual(expect.arrayContaining(['seeked', 'error']));
		m.settle(40);
		m.dispatchEvent(new Event('error'));
		vi.advanceTimersByTime(2000);
		expect(publish).not.toHaveBeenCalled();
		expect(m.attempts).toEqual([40]);
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
			{ kind: 'pending', target: 40, clamped: false },
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
