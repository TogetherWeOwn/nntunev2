import { get, writable } from 'svelte/store';
import { clampPlaybackSpeed, createPlaybackSpeed, type PlaybackSpeedDeps } from './playbackSpeed.js';

type RatePort = { playbackRate: number };
type RateTarget = { audio: RatePort; matches: () => boolean };
const unavailable: { rate: number | null; feedback: string } = { rate: null, feedback: '' };

export function createLivePlaybackSpeed(target: () => RateTarget | undefined) {
	const state = writable(unavailable);
	let preference: ReturnType<typeof createPlaybackSpeed> | undefined;
	let writePort: (RatePort & { passive: boolean }) | undefined;

	function read({ audio, matches }: RateTarget): number | null {
		try {
			if (!matches()) return null;
			const rate = audio.playbackRate;
			return Number.isFinite(rate) && matches() ? rate : null;
		} catch {
			return null;
		}
	}

	function write(request?: number): void {
		const current = target();
		if (!preference || !current?.matches()) return;
		const intended = request === undefined ? preference.current() : clampPlaybackSpeed(request);
		const previousPort = writePort;
		let observed: number | null | undefined;
		writePort = {
			passive: request === undefined,
			get playbackRate() {
				observed = read(current);
				return observed ?? NaN;
			},
			set playbackRate(rate) {
				if (!current.matches()) throw new Error('Audio attachment retired');
				current.audio.playbackRate = rate;
			}
		};
		try {
			if (request === undefined) preference.apply();
			else preference.set(request);
		} finally {
			writePort = previousPort;
		}
		// A throwing setter never persists, but its readable actual rate still belongs in the UI.
		const rate = observed === undefined ? read(current) : observed;
		if (!current.matches()) return;
		const feedback = rate === intended ? '' : rate === null
			? `Could not verify speed ${intended}x.`
			: `Speed ${intended}x not applied — still ${rate}x`;
		state.set({ rate, feedback });
	}

	return {
		state: { subscribe: state.subscribe },
		get managed() { return preference !== undefined; },
		restore(storage: PlaybackSpeedDeps['storage'] | null): void {
			let saved: string | null | undefined;
			preference = createPlaybackSpeed({
				audio: () => writePort,
				storage: {
					getItem: (key) => (saved = storage?.getItem(key) ?? null),
					setItem(key, rate) {
						// Passive restores must not overwrite a value saved by another tab.
						if (!storage || (writePort?.passive && storage.getItem(key) !== saved)) return;
						storage.setItem(key, rate);
						saved = rate;
					},
					removeItem() {}
				}
			});
			write();
		},
		attach(): void {
			state.set(unavailable);
			write();
		},
		retire: () => state.set(unavailable),
		set(rate: number): number | null {
			if (!writePort) write(rate);
			return get(state).rate;
		},
		observe(current: RateTarget): void {
			if (!preference || writePort) return;
			const rate = read(current);
			// Same-rate events include the queued event from our own write. Keep its feedback.
			if (rate === null || rate === get(state).rate) return;
			preference.adoptNativeRate(rate);
			if (current.matches()) state.set({ rate, feedback: '' });
		}
	};
}
