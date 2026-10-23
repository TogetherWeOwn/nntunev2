/**
 * Sleep timer: wall-clock or track-count arming that pauses playback without
 * touching the queue. Pure state machine over an injectable clock so the page
 * and the tests drive the same `tick`/`trackEnded` path.
 */

export type SleepTimerMode =
	| { kind: 'duration'; ms: number }
	| { kind: 'end-of-track' }
	| { kind: 'stop-after-tracks'; count: number };

export interface SleepTimerDeps {
	/** Wall clock in ms; the page injects `Date.now` plus the test hook offset. */
	now: () => number;
	/** Called exactly once when the timer fires (the page pauses playback). */
	onFire: () => void;
}

export interface SleepTimer {
	/** The armed mode, or null when disarmed. */
	readonly mode: SleepTimerMode | null;
	/** Track ends observed since arming (duration timers never count). */
	readonly tracksElapsed: number;
	/** Arm (replaces any armed timer, resetting progress). Validates first. */
	start(mode: SleepTimerMode): void;
	/** Disarm; later ticks/ends are inert. */
	cancel(): void;
	/** Advance wall-clock state; true only from the call that fires. */
	tick(nowMs?: number): boolean;
	/** Observe one track end; true only from the call that fires. */
	trackEnded(): boolean;
}

function validateMode(mode: SleepTimerMode): void {
	if (mode.kind === 'duration') {
		if (!Number.isFinite(mode.ms) || mode.ms <= 0) {
			throw new RangeError(`sleep duration must be a positive finite ms value, got ${mode.ms}`);
		}
	} else if (mode.kind === 'stop-after-tracks') {
		if (!Number.isInteger(mode.count) || mode.count < 1) {
			throw new RangeError(`stop-after count must be an integer >= 1, got ${mode.count}`);
		}
	}
}

export function createSleepTimer({ now, onFire }: SleepTimerDeps): SleepTimer {
	let mode: SleepTimerMode | null = null;
	let armedAt = 0;
	let tracksElapsed = 0;

	function fire(): void {
		mode = null;
		onFire();
	}

	return {
		get mode() {
			return mode;
		},
		get tracksElapsed() {
			return tracksElapsed;
		},
		start(next: SleepTimerMode) {
			// Validate before touching state: a rejected start leaves the
			// previous arming (if any) untouched.
			validateMode(next);
			mode = next;
			armedAt = now();
			tracksElapsed = 0;
		},
		cancel() {
			mode = null;
		},
		tick(nowMs: number = now()) {
			if (mode?.kind !== 'duration') return false;
			if (nowMs >= armedAt + mode.ms) {
				fire();
				return true;
			}
			return false;
		},
		trackEnded() {
			if (mode === null || mode.kind === 'duration') return false;
			tracksElapsed += 1;
			if (mode.kind === 'end-of-track' || tracksElapsed >= mode.count) {
				fire();
				return true;
			}
			return false;
		}
	};
}
