import { describe, expect, it, vi } from 'vitest';
import { createSleepTimer, type SleepTimerMode } from './sleepTimer.js';

/**
 * Test-Author owned RED acceptance for the player sleep timer. The Builder
 * implements `sleepTimer.ts` to satisfy this file and must not edit it.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/sleepTimer.ts`):
 * - `SleepTimerMode = { kind: 'duration'; ms: number }`
 *   `| { kind: 'end-of-track' } | { kind: 'stop-after-tracks'; count: number }`
 * - `createSleepTimer({ now, onFire }): SleepTimer` where `now: () => number`
 *   is an injectable wall clock (ms) and `onFire` pauses playback.
 * - `timer.mode: SleepTimerMode | null` — the armed mode, null when disarmed.
 * - `timer.tracksElapsed: number` — track ends observed since arming.
 * - `timer.start(mode)` arms (replaces any armed timer, resetting progress).
 *   Throws `RangeError` for non-positive/non-finite duration `ms` or for
 *   `count` that is not an integer >= 1; a rejected start leaves the previous
 *   arming (if any) untouched.
 * - `timer.cancel()` disarms; later ticks/ends are inert.
 * - `timer.tick(nowMs?): boolean` advances wall-clock state (`now()` when the
 *   argument is omitted). A duration timer fires when now >= armedAt + ms.
 *   `end-of-track` / `stop-after-tracks` timers never fire from `tick`.
 * - `timer.trackEnded(): boolean` observes one track end. `end-of-track`
 *   fires on the first call; `stop-after-tracks` fires when `tracksElapsed`
 *   reaches `count`; duration timers ignore it (`tracksElapsed` stays 0).
 * - Firing calls `onFire` exactly once, disarms (`mode` becomes null), and
 *   returns true from the call that fired; every later `tick`/`trackEnded`
 *   returns false without calling `onFire` again.
 * - Queue preservation is asserted at the page level (the paired Playwright
 *   spec): firing pauses; it never clears or reorders the queue.
 */

function harness() {
	const state = { now: 1_000_000 };
	const onFire = vi.fn();
	const timer = createSleepTimer({ now: () => state.now, onFire });
	return { state, onFire, timer };
}

function duration(ms: number): SleepTimerMode {
	return { kind: 'duration', ms };
}

describe('sleep timer duration mode', () => {
	it('does not fire before the deadline', () => {
		const { state, onFire, timer } = harness();
		timer.start(duration(15 * 60_000));

		expect(timer.tick(state.now + 14 * 60_000)).toBe(false);
		expect(onFire).not.toHaveBeenCalled();
		expect(timer.mode).toEqual(duration(15 * 60_000));
	});

	it('fires exactly once at the deadline via the injected clock', () => {
		const { state, onFire, timer } = harness();
		timer.start(duration(60_000));

		state.now += 60_000;
		expect(timer.tick()).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
		expect(timer.mode).toBeNull();

		state.now += 60_000;
		expect(timer.tick()).toBe(false);
		expect(timer.trackEnded()).toBe(false);
		expect(onFire).toHaveBeenCalledTimes(1);
	});

	it('fires when the tick argument reaches the deadline', () => {
		const { state, onFire, timer } = harness();
		timer.start(duration(5_000));

		expect(timer.tick(state.now + 5_000)).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
	});

	it('ignores track ends without counting them', () => {
		const { state, onFire, timer } = harness();
		timer.start(duration(60_000));

		expect(timer.trackEnded()).toBe(false);
		expect(timer.trackEnded()).toBe(false);
		expect(timer.tracksElapsed).toBe(0);
		expect(onFire).not.toHaveBeenCalled();

		state.now += 60_000;
		expect(timer.tick()).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
	});
});

describe('sleep timer end-of-track mode', () => {
	it('fires on the first track end', () => {
		const { onFire, timer } = harness();
		timer.start({ kind: 'end-of-track' });

		expect(timer.trackEnded()).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
		expect(timer.mode).toBeNull();
	});

	it('never fires from wall-clock ticks alone', () => {
		const { state, onFire, timer } = harness();
		timer.start({ kind: 'end-of-track' });

		expect(timer.tick(state.now + 24 * 3_600_000)).toBe(false);
		expect(onFire).not.toHaveBeenCalled();
		expect(timer.mode).toEqual({ kind: 'end-of-track' });
	});

	it('fires once even across repeated ends', () => {
		const { onFire, timer } = harness();
		timer.start({ kind: 'end-of-track' });

		expect(timer.trackEnded()).toBe(true);
		expect(timer.trackEnded()).toBe(false);
		expect(onFire).toHaveBeenCalledTimes(1);
	});
});

describe('sleep timer stop-after-N mode', () => {
	it('fires on the Nth end and counts the earlier ones', () => {
		const { onFire, timer } = harness();
		timer.start({ kind: 'stop-after-tracks', count: 3 });

		expect(timer.trackEnded()).toBe(false);
		expect(timer.tracksElapsed).toBe(1);
		expect(timer.trackEnded()).toBe(false);
		expect(timer.tracksElapsed).toBe(2);
		expect(onFire).not.toHaveBeenCalled();

		expect(timer.trackEnded()).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
		expect(timer.mode).toBeNull();
	});

	it('fires on the first end when N is 1', () => {
		const { onFire, timer } = harness();
		timer.start({ kind: 'stop-after-tracks', count: 1 });

		expect(timer.trackEnded()).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
	});

	it('never fires from wall-clock ticks alone', () => {
		const { state, onFire, timer } = harness();
		timer.start({ kind: 'stop-after-tracks', count: 2 });

		expect(timer.tick(state.now + 24 * 3_600_000)).toBe(false);
		expect(onFire).not.toHaveBeenCalled();
		expect(timer.tracksElapsed).toBe(0);
	});
});

describe('sleep timer cancel and re-arm', () => {
	it('cancel disarms a duration timer so the deadline passes silently', () => {
		const { state, onFire, timer } = harness();
		timer.start(duration(1_000));
		timer.cancel();

		expect(timer.mode).toBeNull();
		state.now += 60_000;
		expect(timer.tick()).toBe(false);
		expect(onFire).not.toHaveBeenCalled();
	});

	it('cancel disarms a track-counting timer so later ends are inert', () => {
		const { onFire, timer } = harness();
		timer.start({ kind: 'stop-after-tracks', count: 1 });
		timer.cancel();

		expect(timer.trackEnded()).toBe(false);
		expect(onFire).not.toHaveBeenCalled();
	});

	it('cancel on a disarmed timer is a no-op', () => {
		const { timer } = harness();

		expect(() => timer.cancel()).not.toThrow();
		expect(timer.mode).toBeNull();
	});

	it('start replaces an armed timer and resets its progress', () => {
		const { state, onFire, timer } = harness();
		timer.start({ kind: 'stop-after-tracks', count: 5 });
		timer.trackEnded();
		timer.trackEnded();
		expect(timer.tracksElapsed).toBe(2);

		timer.start(duration(30_000));
		expect(timer.mode).toEqual(duration(30_000));
		expect(timer.tracksElapsed).toBe(0);

		state.now += 30_000;
		expect(timer.tick()).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
	});
});

describe('sleep timer validation', () => {
	it.each([[0], [-1], [Number.NaN], [Number.POSITIVE_INFINITY]])(
		'rejects a duration of %s ms with RangeError',
		(ms) => {
			const { timer } = harness();

			expect(() => timer.start(duration(ms))).toThrow(RangeError);
			expect(timer.mode).toBeNull();
		}
	);

	it.each([[0], [-2], [1.5], [Number.NaN]])('rejects stop-after-N count %s with RangeError', (count) => {
		const { timer } = harness();

		expect(() => timer.start({ kind: 'stop-after-tracks', count })).toThrow(RangeError);
		expect(timer.mode).toBeNull();
	});

	it('a rejected start leaves the previous arming untouched', () => {
		const { onFire, timer } = harness();
		timer.start({ kind: 'end-of-track' });

		expect(() => timer.start(duration(-5))).toThrow(RangeError);
		expect(timer.mode).toEqual({ kind: 'end-of-track' });

		expect(timer.trackEnded()).toBe(true);
		expect(onFire).toHaveBeenCalledTimes(1);
	});
});
