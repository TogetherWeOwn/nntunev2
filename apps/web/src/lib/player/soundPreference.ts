/** Device-local sound only: no track, provider or account identity. */
export interface SoundPreference {
	volume: number;
	muted: boolean;
}

export const SOUND_PREFERENCE_STORAGE_KEY = 'nntune:sound-preference:v1';
const MAX_SOUND_PREFERENCE_CHARS = 1024;
type SoundStorage = Pick<Storage, 'getItem' | 'setItem'> | null;

function defaults(): SoundPreference {
	return { volume: 1, muted: false };
}

function validSound(value: unknown): value is SoundPreference {
	if (typeof value !== 'object' || !value) return false;
	const sound = value as Partial<SoundPreference>;
	// The closed interval also rejects NaN and both infinities.
	return typeof sound.volume === 'number' && sound.volume >= 0 &&
		sound.volume <= 1 && typeof sound.muted === 'boolean';
}

export function parseSoundPreference(raw: string | null, fallback = defaults()): SoundPreference {
	if (raw === null || raw.length > MAX_SOUND_PREFERENCE_CHARS) return fallback;
	try {
		const value: unknown = JSON.parse(raw);
		if (validSound(value) && (value as { version?: unknown }).version === 1) {
			return { volume: value.volume, muted: value.muted };
		}
	} catch {
		// Ignore the entire malformed record without clearing storage.
	}
	return fallback;
}

export function serializeSoundPreference(value: SoundPreference): string {
	const sound = validSound(value) ? value : defaults();
	return JSON.stringify({ version: 1, volume: sound.volume, muted: sound.muted });
}

export function loadSoundPreference(storage: SoundStorage, fallback = defaults()): SoundPreference {
	try {
		return parseSoundPreference(storage?.getItem(SOUND_PREFERENCE_STORAGE_KEY) ?? null, fallback);
	} catch {
		return fallback;
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
): SoundPreference & { restricted: boolean } {
	if (!validSound(preference)) return { volume: element.volume, muted: element.muted, restricted: true };
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
	const volume = element.volume;
	return {
		volume,
		muted: element.muted,
		restricted: volume !== preference.volume
	};
}
