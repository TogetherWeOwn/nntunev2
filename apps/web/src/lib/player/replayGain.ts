/**
 * Loudness leveling with ReplayGain tags: pure, offline, deterministic core.
 *
 * The player reads per-track ReplayGain tag text (e.g. "-7.00 dB"), converts
 * the tag to a linear amplitude multiplier, guards against clipping with the
 * track/album peak, and drives a `GainNode`. The leveling toggle persists in
 * `localStorage` and defaults to ON.
 *
 * Clipping guard (design note): ReplayGain tags only describe loudness, so a
 * large positive tag on a hot master can push peaks past full scale. The
 * guard scales the resolved gain back only when `peak * linear > 1`, landing
 * exactly on full scale (`1 / peak`); anything with headroom — including the
 * exact `peak * linear === 1` boundary — passes through bit-identical, and
 * invalid peaks (zero, negative, non-numeric) are ignored rather than muting
 * or boosting the track. Out of scope: loudness scanning, writing tags,
 * server-side normalization, crossfade, gapless.
 */

/** `localStorage` key for the persisted leveling toggle. */
export const LEVELING_STORAGE_KEY = 'nntune.leveling.enabled';

/** ReplayGain tag/peak inputs as read from track metadata. */
export interface LevelingTags {
	trackGainDb?: unknown;
	albumGainDb?: unknown;
	trackPeak?: unknown;
	albumPeak?: unknown;
}

/** Minimal slice of `GainNode` the leveler drives. */
export interface GainNodeLike {
	gain: { value: number };
}

/** Minimal slice of `Storage` the toggle persists through. */
export interface StorageLike {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

/** Options for {@link resolveTrackLinearGain}. */
export interface LevelingOptions {
	/** When false, every track resolves to exactly 1 (leveling bypassed). */
	enabled: boolean;
	/**
	 * User preamp offset in dB, added to the tag (or to 0 when untagged).
	 * Defaults to 0; non-finite values are treated as 0.
	 */
	preampDb?: number;
}

/**
 * Plain-decimal gain text with an optional case-insensitive "dB" suffix:
 * "-7.00 dB", "+2.5dB", "  +0.75 DB  ". Bare numbers in real-world tags are
 * accepted too ("-3.25" parses); exponents, hex and doubled signs are not
 * gain text and are rejected.
 */
const GAIN_TEXT_PATTERN = /^\s*([+-]?(?:\d+(?:\.\d+)?|\.\d+))\s*(?:db)?\s*$/i;

/**
 * Parse ReplayGain tag text such as "-7.00 dB" to dB. Bare finite numbers
 * pass through; anything else (missing, non-numeric, doubled signs, NaN or
 * infinite) is null so the caller falls back to unity gain.
 */
export function parseGainDb(value: unknown): number | null {
	if (typeof value === 'number') {
		return Number.isFinite(value) ? value : null;
	}
	if (typeof value !== 'string') {
		return null;
	}
	const match = GAIN_TEXT_PATTERN.exec(value);
	if (match === null || match[1] === undefined) {
		return null;
	}
	const parsed = Number(match[1]);
	return Number.isFinite(parsed) ? parsed : null;
}

/** Amplitude conversion: dB to linear gain, `10^(db/20)`. */
export function gainDbToLinear(db: number): number {
	return Math.pow(10, db / 20);
}

/** Parse a ReplayGain peak (linear, full scale = 1); invalid peaks are null. */
function parsePeak(value: unknown): number | null {
	const parsed = typeof value === 'number' ? value : typeof value === 'string' ? Number(value.trim()) : NaN;
	if (!Number.isFinite(parsed) || parsed <= 0) {
		return null;
	}
	return parsed;
}

/**
 * Scale `linear` back to exactly full scale only when `peak * linear` would
 * clip; headroom passes through untouched (the same value, not a
 * recomputation) and invalid peaks are ignored.
 */
export function applyClippingGuard(linear: number, peak: unknown): number {
	const parsed = parsePeak(peak);
	if (parsed === null) {
		return linear;
	}
	if (parsed * linear > 1) {
		return 1 / parsed;
	}
	return linear;
}

/**
 * Resolve the linear gain for one track: the track tag wins, the album tag
 * is the fallback, and untagged (or garbage-tagged) tracks resolve from 0 dB
 * — exactly 1 with the default preamp. The preamp offset adds to the tag.
 * Track peak (then album peak) routes through the clipping guard. While
 * leveling is disabled every track is exactly 1.
 */
export function resolveTrackLinearGain(tags: LevelingTags, opts: LevelingOptions): number {
	if (!opts.enabled) {
		return 1;
	}
	const tagDb = parseGainDb(tags.trackGainDb) ?? parseGainDb(tags.albumGainDb) ?? 0;
	const preampDb = typeof opts.preampDb === 'number' && Number.isFinite(opts.preampDb) ? opts.preampDb : 0;
	const linear = gainDbToLinear(tagDb + preampDb);
	const peak = parsePeak(tags.trackPeak) ?? parsePeak(tags.albumPeak);
	if (peak === null) {
		return linear;
	}
	return applyClippingGuard(linear, peak);
}

/** Drive a GainNode-like with the resolved linear gain. */
export function applyGainToNode(node: GainNodeLike, linear: number): void {
	node.gain.value = linear;
}

const STORED_OFF = '0';

/**
 * Read the persisted leveling toggle. Missing, corrupt, or unavailable
 * storage reads ON: leveling is the default, the toggle opts out.
 */
export function loadLevelingEnabled(storage: StorageLike | null | undefined): boolean {
	if (storage === null || storage === undefined) {
		return true;
	}
	try {
		return storage.getItem(LEVELING_STORAGE_KEY) !== STORED_OFF;
	} catch {
		return true;
	}
}

/** Persist the leveling toggle; unavailable storage is a silent no-op. */
export function saveLevelingEnabled(
	storage: StorageLike | null | undefined,
	enabled: boolean
): void {
	if (storage === null || storage === undefined) {
		return;
	}
	try {
		storage.setItem(LEVELING_STORAGE_KEY, enabled ? '1' : STORED_OFF);
	} catch {
		// Storage can be unavailable (private mode); the toggle still works
		// for the session, it just does not persist.
	}
}
