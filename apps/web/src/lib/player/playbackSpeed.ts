/**
 * Persisted playback speed (0.5–2x). The store is fail-soft: storage errors
 * degrade to the default and never throw, so private-mode playback still works.
 */

export const MIN_PLAYBACK_SPEED = 0.5;
export const MAX_PLAYBACK_SPEED = 2;
export const DEFAULT_PLAYBACK_SPEED = 1;
export const PLAYBACK_SPEED_STORAGE_KEY = 'nntune:playback-speed:v1';

export function clampPlaybackSpeed(rate: number): number {
	if (!Number.isFinite(rate)) return DEFAULT_PLAYBACK_SPEED;
	return Math.min(MAX_PLAYBACK_SPEED, Math.max(MIN_PLAYBACK_SPEED, rate));
}

export function parsePlaybackSpeed(raw: string | null): number {
	if (raw === null) return DEFAULT_PLAYBACK_SPEED;
	// Blank is garbage, not 0: `Number('')` is 0, so reject blanks first.
	if (raw.trim() === '') return DEFAULT_PLAYBACK_SPEED;
	const parsed = Number(raw.trim());
	if (!Number.isFinite(parsed)) return DEFAULT_PLAYBACK_SPEED;
	return clampPlaybackSpeed(parsed);
}

export interface PlaybackSpeedDeps {
	storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
	/** Current audio element; null/undefined when none is mounted. */
	audio: () => { playbackRate: number } | null | undefined;
	storageKey?: string;
}

export interface PlaybackSpeed {
	/** The effective rate (persisted-or-default, then sets). */
	current(): number;
	/** Clamp, apply to the current element, persist; returns the effective rate. */
	set(rate: number): number;
	/** Re-apply the current rate (for element swaps); returns the effective rate. */
	apply(): number;
}

export function createPlaybackSpeed({
	storage,
	audio,
	storageKey = PLAYBACK_SPEED_STORAGE_KEY
}: PlaybackSpeedDeps): PlaybackSpeed {
	let current: number;
	try {
		current = parsePlaybackSpeed(storage.getItem(storageKey));
	} catch {
		current = DEFAULT_PLAYBACK_SPEED;
	}

	function persist(rate: number): void {
		try {
			storage.setItem(storageKey, String(rate));
		} catch {
			// Fail-soft: the rate still applies to audio below.
		}
	}

	return {
		current() {
			return current;
		},
		set(rate: number) {
			current = clampPlaybackSpeed(rate);
			const element = audio();
			if (element) element.playbackRate = current;
			persist(current);
			return current;
		},
		apply() {
			const element = audio();
			if (element) element.playbackRate = current;
			return current;
		}
	};
}
