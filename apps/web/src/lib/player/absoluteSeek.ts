type SeekMedia = Pick<HTMLMediaElement, 'currentTime' | 'duration' | 'error' | 'seekable' | 'seeking'>;

const emptyObservation = { position: 0, duration: 0, enabled: false, seeking: false };
export const emptySeek = { ...emptyObservation, elapsed: 0, attachment: 0, feedback: '', feedbackVersion: 0 };
export type AbsoluteSeekState = typeof emptySeek;
export const SEEK_READBACK_TOLERANCE = 0.05;

export function seekable(media: Pick<SeekMedia, 'seekable'>, duration: number, target?: number, unavailable?: () => void): boolean {
	const ranges = media.seekable;
	let available = false;
	for (let i = 0; i < ranges.length; i++) {
		const start = Math.max(0, ranges.start(i)), end = Math.min(duration, ranges.end(i));
		available ||= end > start;
		if (end > start && (target === undefined || (target >= start && target <= end))) return true;
	}
	if (!available) unavailable?.();
	return false;
}

export function readAbsoluteSeek(media: SeekMedia): typeof emptyObservation {
	try {
		const { currentTime, duration, error } = media;
		if (error) return emptyObservation;
		const known = Number.isFinite(duration) && duration > 0;
		const position = Number.isFinite(currentTime) ? Math.max(0, currentTime) : 0;
		return { position: known ? Math.min(position, duration) : position, duration: known ? duration : 0,
			seeking: known && media.seeking === true, enabled: known && Number.isFinite(currentTime) && seekable(media, duration) };
	} catch {
		return emptyObservation;
	}
}

export function writeAbsoluteSeek(media: SeekMedia, seconds: number, matches: () => boolean, unavailable?: () => void, landed?: (target: number, duration: number, observed: number) => void): string {
	let validating = true;
	try {
		const { duration, currentTime, error } = media;
		if (error || !Number.isFinite(duration) || duration <= 0 || !Number.isFinite(currentTime)) {
			unavailable?.();
			return 'Seeking is unavailable for this track.';
		}
		const target = Math.min(duration, Math.max(0, seconds));
		if (!seekable(media, duration, target, unavailable)) return 'That position is not seekable yet.';
		validating = false;
		const canStartEndpointSeek = landed && target === duration && !media.seeking;
		if (!matches() || target === currentTime) return '';
		media.currentTime = target;
		if (!landed && !matches()) return '';
		const observed = media.currentTime;
		if (Number.isFinite(observed) && Math.abs(observed - target) <= SEEK_READBACK_TOLERANCE) {
			if (observed !== currentTime || (canStartEndpointSeek && media.seeking)) landed?.(target, duration, observed);
			return '';
		}
	} catch { if (validating) unavailable?.(); }
	return 'The player did not accept that seek.';
}
