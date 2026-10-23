/** Device-local sound only: no track, provider or account identity. */
export interface SoundPreference {
	volume: number;
	muted: boolean;
}

export const SOUND_PREFERENCE_STORAGE_KEY = 'nntune:sound-preference:v1';
type SoundStorage = Pick<Storage, 'getItem' | 'setItem'> | null;

function defaults(): SoundPreference {
	return { volume: 1, muted: false };
}

function validSound(value: unknown): value is SoundPreference {
	if (typeof value !== 'object' || value === null) return false;
	const sound = value as Partial<SoundPreference>;
	return typeof sound.volume === 'number' && Number.isFinite(sound.volume) &&
		sound.volume >= 0 && sound.volume <= 1 && typeof sound.muted === 'boolean';
}

export function parseSoundPreference(raw: string | null): SoundPreference {
	try {
		const value: unknown = JSON.parse(raw ?? 'null');
		if (validSound(value) && (value as { version?: unknown }).version === 1) {
			return { volume: value.volume, muted: value.muted };
		}
	} catch {
		// A malformed record defaults atomically, without clearing storage.
	}
	return defaults();
}

export function serializeSoundPreference(value: SoundPreference): string {
	const sound = validSound(value) ? value : defaults();
	return JSON.stringify({ version: 1, volume: sound.volume, muted: sound.muted });
}

export function loadSoundPreference(storage: SoundStorage): SoundPreference {
	try {
		return parseSoundPreference(storage?.getItem(SOUND_PREFERENCE_STORAGE_KEY) ?? null);
	} catch {
		return defaults();
	}
}

export function saveSoundPreference(storage: SoundStorage, value: SoundPreference): void {
	try {
		storage?.setItem(SOUND_PREFERENCE_STORAGE_KEY, serializeSoundPreference(value));
	} catch {
		// Quota/privacy failures never prevent live controls or transport.
	}
}

/** Read back actual values: some platforms throw or silently ignore volume writes. */
export function applySoundPreference(
	element: Pick<HTMLMediaElement, 'volume' | 'muted'>,
	preference: SoundPreference
): SoundPreference & { volumeRestricted: boolean } {
	try {
		if (element.volume !== preference.volume) element.volume = preference.volume;
	} catch {
		// System-controlled volume: leave transport and the chosen mute alone.
	}
	try {
		if (element.muted !== preference.muted) element.muted = preference.muted;
	} catch {
		// Reflect actual mute even on a platform that rejects this setter too.
	}
	return {
		volume: element.volume,
		muted: element.muted,
		volumeRestricted: element.volume !== preference.volume
	};
}
