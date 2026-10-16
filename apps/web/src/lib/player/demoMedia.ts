/**
 * Synthetic demo media. The body is deterministic PCM WAV audio; queue
 * tracks (`?track=<id>`) get distinct samples while retaining valid headers.
 */
export const MEDIA_BYTES = 256 * 1024;
const WAV_HEADER_BYTES = 44;
const SAMPLE_RATE = 8_000;

/** Allowlisted track ids -> sample salt. Unknown ids serve the default stream. */
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
	const header = new DataView(body.buffer);
	const dataBytes = MEDIA_BYTES - WAV_HEADER_BYTES;

	body.set(new TextEncoder().encode('RIFF'), 0);
	header.setUint32(4, MEDIA_BYTES - 8, true);
	body.set(new TextEncoder().encode('WAVEfmt '), 8);
	header.setUint32(16, 16, true);
	header.setUint16(20, 1, true);
	header.setUint16(22, 1, true);
	header.setUint32(24, SAMPLE_RATE, true);
	header.setUint32(28, SAMPLE_RATE * 2, true);
	header.setUint16(32, 2, true);
	header.setUint16(34, 16, true);
	body.set(new TextEncoder().encode('data'), 36);
	header.setUint32(40, dataBytes, true);

	for (let offset = WAV_HEADER_BYTES, sample = 0; offset < MEDIA_BYTES; offset += 2, sample += 1) {
		const value = ((sample * 7919 + 13 + salt) % 65_536) - 32_768;
		header.setInt16(offset, value, true);
	}
	return body;
}
