import { writable } from 'svelte/store';
import { formatDuration } from '../format.js';
import { createSleepTimer, type SleepTimerMode } from './sleepTimer.js';

const off = { armed: false, announcement: 'Sleep timer off', remaining: '' };

export function createPlayerSleepTimer(pause: () => void, canStart: () => boolean) {
	let generation = 0;
	let interval: ReturnType<typeof setInterval> | undefined;
	const state = writable(off);
	const timer = createSleepTimer({ now: Date.now, onFire() {
		const receipt = stopTicking();
		pause();
		// A synchronous transport observer may have reset or rearmed us.
		if (receipt !== generation) return;
		state.set({ ...off, announcement: 'Sleep timer fired — playback paused' });
	} });

	function stopTicking(): number {
		clearInterval(interval);
		interval = undefined;
		return ++generation;
	}
	function publish(message?: string): void {
		const progress = timer.progress;
		if (!progress) return;
		const count = progress.kind === 'stop-after-tracks' ? progress.remainingTracks : 0;
		const tracks = `${count} track${count === 1 ? '' : 's'}`;
		const remaining = progress.kind === 'duration' ? formatDuration(Math.ceil(progress.remainingMs / 1000))
			: progress.kind === 'end-of-track' ? 'End of track' : `${tracks} remaining`;
		state.update((prior) => ({ armed: true, remaining,
			announcement: message ?? (count ? `Sleep after ${tracks}` : prior.announcement) }));
	}
	function tick(): boolean {
		if (timer.mode?.kind !== 'duration') return false;
		if (timer.tick()) return true;
		publish();
		return false;
	}

	return {
		subscribe: state.subscribe,
		start(mode: SleepTimerMode, message: string): void {
			if (!canStart()) return;
			timer.start(mode);
			const receipt = stopTicking();
			if (mode.kind === 'duration') interval = setInterval(() => { if (receipt === generation) tick(); }, 500);
			publish(message);
		},
		cancel(): void {
			stopTicking();
			timer.cancel();
			state.set(off);
		},
		tick,
		observeEnd(natural: boolean): boolean {
			if (timer.mode?.kind === 'duration') return tick();
			if (!natural) return false;
			if (timer.trackEnded()) return true;
			publish();
			return false;
		}
	};
}
