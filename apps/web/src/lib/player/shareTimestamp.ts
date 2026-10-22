/**
 * Share links with timestamps: `?track=<id>&t=<seconds>`. Accepts plain
 * seconds ('90', '90.5') or clock forms ('m:ss', 'h:mm:ss').
 */

const CLOCK_PART = /^\d+$/;
const PLAIN_SECONDS = /^\d+(\.\d+)?$/;

export function parseTimestampParam(raw: string | null | undefined): number | null {
	if (raw === null || raw === undefined) return null;
	const trimmed = raw.trim();
	if (trimmed === '') return null;
	if (trimmed.includes(':')) {
		const parts = trimmed.split(':');
		if (parts.length !== 2 && parts.length !== 3) return null;
		if (!parts.every((part) => CLOCK_PART.test(part))) return null;
		const numbers = parts.map(Number);
		// Seconds/minutes below the leading part must stay under 60.
		if (numbers.slice(1).some((n) => n >= 60)) return null;
		return numbers.length === 2
			? numbers[0]! * 60 + numbers[1]!
			: numbers[0]! * 3600 + numbers[1]! * 60 + numbers[2]!;
	}
	if (!PLAIN_SECONDS.test(trimmed)) return null;
	const seconds = Number(trimmed);
	return Number.isFinite(seconds) ? seconds : null;
}

export interface ShareParams {
	track: string | null | undefined;
	t: string | null | undefined;
}

export interface ShareTarget {
	trackId: string;
	seconds: number;
}

/**
 * Resolve only when the track names a catalogue id AND `t` parses; otherwise
 * null (unknown/missing track or unparsable `t` — never partial).
 */
export function resolveShareTarget(
	params: ShareParams,
	catalogueIds: readonly string[]
): ShareTarget | null {
	const { track, t } = params;
	if (track === null || track === undefined) return null;
	if (!catalogueIds.includes(track)) return null;
	const seconds = parseTimestampParam(t);
	if (seconds === null) return null;
	return { trackId: track, seconds };
}

export function buildShareQuery(trackId: string, seconds: number): string {
	if (trackId.trim() === '') {
		throw new RangeError('share links need a non-blank track id');
	}
	if (!Number.isFinite(seconds) || seconds < 0) {
		throw new RangeError(`share timestamp must be finite non-negative seconds, got ${seconds}`);
	}
	return `track=${encodeURIComponent(trackId)}&t=${Math.floor(seconds)}`;
}
