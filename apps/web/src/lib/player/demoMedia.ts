/**
 * Synthetic demo media. The body is deterministic pseudo-audio; queue
 * tracks (`?track=<id>`) get distinct bytes so selecting a track really
 * switches the stream. No `track` keeps the original bytes untouched:
 * the seek specs assert against them.
 */
export const MEDIA_BYTES = 256 * 1024;

/** Allowlisted track ids -> byte salt. Unknown ids serve the default stream. */
const TRACK_SALT: Readonly<Record<string, number>> = {
	'second-signal': 41,
	'third-light': 97
};

export function trackSalt(track: string | null): number {
	if (track === null || !Object.hasOwn(TRACK_SALT, track)) return 0;
	return TRACK_SALT[track] ?? 0;
}

export function syntheticBody(track: string | null = null): Uint8Array {
	const salt = trackSalt(track);
	const body = new Uint8Array(MEDIA_BYTES);
	for (let i = 0; i < MEDIA_BYTES; i += 1) {
		body[i] = (i * 7919 + 13 + salt) % 256;
	}
	return body;
}
