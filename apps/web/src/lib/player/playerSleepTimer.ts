import { writable } from 'svelte/store';
import { createSleepTimer, type SleepTimerMode, type SleepTimerProgress } from './sleepTimer.js';

function formatSleepRemaining(progress: SleepTimerProgress | null): string {
	if (!progress) return '';
	if (progress.kind === 'end-of-track') return 'End of track';
	if (progress.kind === 'stop-after-tracks') {
		const n = progress.remainingTracks;
		return `${n} track${n === 1 ? '' : 's'} remaining`;
	}
	const total = Math.ceil(progress.remainingMs / 1000);
	const hours = Math.floor(total / 3600);
	const minutes = Math.floor(total / 60) % 60;
	const seconds = String(total % 60).padStart(2, '0');
	return hours ? `${hours}:${String(minutes).padStart(2, '0')}:${seconds}` : `${minutes}:${seconds}`;
}

const off = { armed: false, announcement: 'Sleep timer off', remaining: '' };

export function createPlayerSleepTimer(pause: () => void, canStart: () => boolean) {
	let generation = 0;
	let interval: ReturnType<typeof setInterval> | undefined;
	let announcement = off.announcement;
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
	function publish(): void {
		state.set({ armed: true, announcement, remaining: formatSleepRemaining(timer.progress) });
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
			announcement = message;
			if (mode.kind === 'duration') interval = setInterval(() => { if (receipt === generation) tick(); }, 500);
			publish();
		},
		cancel(): void {
			stopTicking();
			timer.cancel();
			state.set(off);
		},
		tick,
		observeEnd(): boolean {
			if (timer.mode?.kind === 'duration') return tick();
			if (timer.trackEnded()) return true;
			const progress = timer.progress;
			if (progress?.kind === 'stop-after-tracks') {
				const n = progress.remainingTracks;
				announcement = `Sleep after ${n} track${n === 1 ? '' : 's'}`;
				publish();
			}
			return false;
		}
	};
}
