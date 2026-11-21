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
		} catch { return null; }
	}

	function write(request?: number, current = target(), inherited?: number | null): void {
		const intended = inherited ?? (request === undefined ? preference?.current() : clampPlaybackSpeed(request));
		if (intended === undefined || !current?.matches()) return;
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
			if (inherited != null) writePort.playbackRate = inherited; // Live handoff, not a preference write.
			else if (request === undefined) preference?.apply();
			else preference?.set(request);
		} catch { /* A refused handoff still allows play at the observed rate. */ } finally {
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
		attach(inherit?: () => number | null): void {
			const current = target();
			state.set(unavailable);
			if (current) write(undefined, current, inherit?.());
		},
		reapply: (current: RateTarget) => write(undefined, current),
		retire: () => state.set(unavailable),
		set(rate: number): number | null {
			if (preference && !writePort) write(rate);
			return get(state).rate;
		},
		observe(current: RateTarget): number | null {
			if (writePort) return null;
			const rate = read(current);
			// Same-rate events include the queued event from our own write. Keep its feedback.
			if (!preference || rate === null || rate === get(state).rate) return rate;
			preference.adoptNativeRate(rate);
			if (current.matches()) state.set({ rate, feedback: '' });
			return rate;
		}
	};
}
