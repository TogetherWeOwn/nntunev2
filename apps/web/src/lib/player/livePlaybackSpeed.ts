import { get, writable } from 'svelte/store';
import { clampPlaybackSpeed, createPlaybackSpeed, type PlaybackSpeedDeps } from './playbackSpeed.js';

type RateTarget = { audio: { playbackRate: number }; matches: () => boolean };
const unavailable: { rate: number | null; feedback: string } = { rate: null, feedback: '' };

export function createLivePlaybackSpeed(target: () => RateTarget | undefined) {
	const state = writable(unavailable);
	let preference: ReturnType<typeof createPlaybackSpeed> | undefined;
	let writePort: RateTarget['audio'] | undefined;
	let replacementRate: (() => number | null) | undefined;

	function read({ audio, matches }: RateTarget): number | null {
		try {
			if (!matches()) return null;
			const rate = audio.playbackRate;
			return Number.isFinite(rate) && matches() ? rate : null;
		} catch { return null; }
	}

	function write(request?: number, current = target(), inherited?: number | null): void {
		const intended = inherited ?? (request === undefined ? preference?.current() : clampPlaybackSpeed(request));
		if (intended === undefined || !current?.matches()) return;
		const previousPort = writePort;
		let observed: number | null | undefined;
		writePort = {
			get playbackRate() { return (observed = read(current)) ?? NaN; },
			set playbackRate(rate) {
				if (!current.matches()) throw new Error('Audio attachment retired');
				current.audio.playbackRate = rate;
			}
		};
		try {
			if (inherited != null) { // Live handoff, not a preference write.
				if (read(current) !== inherited) writePort.playbackRate = inherited;
			} else if (request === undefined) preference?.apply();
			else preference?.set(request);
		} catch { /* Still show native readback if the setter throws. */ } finally {
			writePort = previousPort;
		}
		const rate = observed === undefined ? read(current) : observed;
		if (!current.matches()) return;
		const feedback = rate === intended ? '' : rate === null
			? `Could not verify speed ${intended}x.` : `Speed ${intended}x not applied — still ${rate}x`;
		state.set({ rate, feedback });
	}

	return {
		state: { subscribe: state.subscribe },
		restore(storage: PlaybackSpeedDeps['storage'] | null): void {
			preference = createPlaybackSpeed({
				audio: () => writePort,
				// The shared preference retains its live rate when browser storage is blocked.
				storage: storage ?? { getItem: () => null, setItem() {}, removeItem() {} }
			});
			write();
		},
		handoff(inherit: () => number | null): void { replacementRate = inherit; },
		attach(): void {
			const inherit = replacementRate;
			replacementRate = undefined;
			const current = target();
			state.set(unavailable);
			if (current) write(undefined, current, inherit?.());
		},
		reapply: (current: RateTarget, inherited?: number) => write(undefined, current, inherited),
		retire: () => { replacementRate = undefined; state.set(unavailable); },
		set(rate: number): number | null {
			if (preference && !writePort) write(rate);
			return get(state).rate;
		},
		observe(current = target()): number | null {
			if (!current || writePort) return null;
			const rate = read(current);
			// Same-rate events include the queued event from our own write. Keep its feedback.
			if (!preference || rate === null || rate === get(state).rate) return rate;
			preference.adoptNativeRate(rate);
			if (current.matches()) state.set({ rate, feedback: '' });
			return rate;
		}
	};
}
