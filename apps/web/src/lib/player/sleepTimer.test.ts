import { expect, it, vi } from 'vitest';
import { createSleepTimer } from './sleepTimer.js';

it('keeps the accepted deadline across mutation, invalid clocks and rejected rearming', () => {
	let now = 100;
	const onFire = vi.fn();
	const timer = createSleepTimer({ now: () => now, onFire });
	const mode = { kind: 'duration' as const, ms: 20 };
	timer.start(mode);
	mode.ms = 1;
	Object.assign(timer.mode!, { ms: 1 });
	now = Infinity;
	expect(() => timer.start({ kind: 'duration', ms: 99 })).toThrow(RangeError);
	expect(timer.tick()).toBe(false);
	expect(timer.progress).toEqual({ kind: 'duration', remainingMs: 20 });
	expect(timer.tick(101)).toBe(false);
	expect(timer.tick(120)).toBe(true);
	expect(onFire).toHaveBeenCalledOnce();
});
