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

type RateTarget = { playbackRate: number };

export interface PlaybackSpeedDeps {
	storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
	/** Current audio element; null/undefined when none is mounted. */
	audio: () => RateTarget | null | undefined;
	storageKey?: string;
}

export interface PlaybackSpeed {
	/** The bounded preference for a future decoder. */
	current(): number;
	/** Clamp, apply to the current element, persist; returns the live rate. */
	set(rate: number): number;
	/** Restore the cached preference without overwriting a newer one; returns the live rate. */
	apply(): number;
	/** Accept a new raw rate from the current decoder, or return undefined for feedback. */
	observeNativeRate(element: RateTarget): number | undefined;
	/**
	 * Adopt a verified native rate without touching media (for `ratechange`
	 * reconciliation); persists best-effort and returns the bounded preference.
	 * Non-finite input keeps the previous rate and leaves storage untouched.
	 */
	adoptNativeRate(rate: number): number;
}

export function createPlaybackSpeed({
	storage,
	audio,
	storageKey = PLAYBACK_SPEED_STORAGE_KEY
}: PlaybackSpeedDeps): PlaybackSpeed {
	let lastSaved: string | null | undefined;
	let liveRate = DEFAULT_PLAYBACK_SPEED;
	let lastVerified: { element: RateTarget; rate: number } | undefined;
	try {
		lastSaved = storage.getItem(storageKey);
		liveRate = parsePlaybackSpeed(lastSaved);
	} catch {
		// No known storage baseline; keep the default in memory.
	}

	function persist(rate: number, restoring = false): void {
		try {
			// Best-effort compare, not an atomic cross-tab write. A restore may
			// reconcile storage only if the value we based it on is still visible.
			if (restoring && (lastSaved === undefined || storage.getItem(storageKey) !== lastSaved)) {
				return;
			}
			const saved = String(clampPlaybackSpeed(rate));
			storage.setItem(storageKey, saved);
			lastSaved = saved;
		} catch {
			// Storage is optional; the live rate still works.
		}
	}

	function readCurrentRate(element: RateTarget): number | undefined {
		if (audio() !== element) return;
		const rate = element.playbackRate;
		if (Number.isFinite(rate) && audio() === element) return rate;
	}

	/** Keep the raw readback for display, but persist only a bounded rate. */
	function writeAndVerify(request: number, restoring = false): number {
		try {
			const element = audio();
			if (!element) {
				lastVerified = undefined;
				if (!restoring) {
					liveRate = clampPlaybackSpeed(request);
					persist(liveRate);
				}
				return liveRate;
			}
			element.playbackRate = clampPlaybackSpeed(request);
			const rate = readCurrentRate(element);
			if (rate !== undefined) {
				lastVerified = { element, rate };
				liveRate = rate;
				persist(rate, restoring);
			}
		} catch {
			// An unverifiable write leaves the last rate and storage untouched.
		}
		return liveRate;
	}

	function adoptNativeRate(rate: number): number {
		if (Number.isFinite(rate)) {
			liveRate = rate;
			persist(rate);
		}
		return clampPlaybackSpeed(liveRate);
	}

	return {
		current() {
			return clampPlaybackSpeed(liveRate);
		},
		set(rate: number) {
			return writeAndVerify(rate);
		},
		apply() {
			return writeAndVerify(liveRate, true);
		},
		observeNativeRate(element: RateTarget) {
			try {
				const rate = readCurrentRate(element);
				if (rate === undefined) return;
				// A queued event from a verified write reports the same raw rate.
				// Compare raw values so 0.5 -> 0.25 is not lost to clamping.
				if (lastVerified?.element === element && lastVerified.rate === rate) return;
				lastVerified = { element, rate };
				adoptNativeRate(rate);
				return rate;
			} catch {
				// The current native rate could not be verified.
			}
		},
		adoptNativeRate
	};
}
