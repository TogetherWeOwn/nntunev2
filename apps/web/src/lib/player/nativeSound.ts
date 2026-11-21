import { writable } from 'svelte/store';
import { browserStorage } from '../library/cache.js';
import {
	applySoundPreference, loadSoundPreference, saveSoundPreference, type SoundPreference
} from './soundPreference.js';

type SoundElement = EventTarget & Pick<HTMLMediaElement, 'volume' | 'muted'>;

export function createNativeSound(
	getStorage: () => Pick<Storage, 'getItem' | 'setItem'> | null = () => browserStorage('local')
) {
	let preference: SoundPreference | undefined;
	let actual = { volume: 1, muted: false, restricted: false };
	let active: { audio: SoundElement; owns: () => boolean } | undefined;
	const state = writable({ ...actual, ready: false });

	function publish() {
		state.set({ ...actual, ready: !!active });
	}

	function update(request?: SoundPreference): void {
		const target = active;
		if (!target?.owns() || !preference) return;
		const { audio } = target;
		const observed = request ? applySoundPreference(audio, request) : {
			volume: audio.volume, muted: audio.muted, restricted: false
		};
		if (!target.owns()) return;
		const volumeChanged = observed.volume !== actual.volume;
		const muteChanged = observed.muted !== actual.muted;
		// Publish clamped readback, but don't save it as an accepted request.
		// Native changes can still persist independently of the sticky fallback UI.
		const saveVolume = volumeChanged && !observed.restricted;
		if (saveVolume || muteChanged) {
			preference = {
				volume: saveVolume ? observed.volume : preference.volume,
				muted: muteChanged ? observed.muted : preference.muted
			};
			saveSoundPreference(getStorage(), preference);
		}
		observed.restricted ||= actual.restricted;
		if (volumeChanged || muteChanged || observed.restricted !== actual.restricted) {
			actual = observed;
			publish();
		}
	}

	return {
		state: { subscribe: state.subscribe },
		observe: () => update(),
		connect(audio: SoundElement, matches: () => boolean, signal: AbortSignal): void {
			const owns = () => !signal.aborted && matches();
			if (!owns()) return;
			preference ??= loadSoundPreference(getStorage());
			const observed = applySoundPreference(audio, preference);
			if (!owns()) return;
			observed.restricted ||= actual.restricted;
			actual = observed;
			const target = active = { audio, owns };
			signal.addEventListener('abort', () => {
				if (active !== target) return;
				active = undefined;
				publish();
			}, { once: true });
			audio.addEventListener('volumechange', () => {
				if (active === target) update();
			}, { signal });
			publish();
		},
		setVolume(volume: number): number {
			if (active?.owns() && !actual.restricted && Number.isFinite(volume) && volume >= 0 && volume <= 1) {
				update({ volume, muted: active.audio.muted });
			}
			return actual.volume;
		},
		toggleMute(): void {
			if (active?.owns()) update({ volume: active.audio.volume, muted: !active.audio.muted });
		}
	};
}
