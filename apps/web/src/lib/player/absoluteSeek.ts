type SeekMedia = Pick<HTMLMediaElement, 'currentTime' | 'duration' | 'error' | 'seekable'>;

export const emptySeek = { position: 0, duration: 0, enabled: false };
export type AbsoluteSeekState = typeof emptySeek & { attachment: number; feedback: string };

function seekable(media: SeekMedia, duration: number, target?: number): boolean {
	const ranges = media.seekable;
	for (let i = 0; i < ranges.length; i++) {
		const start = Math.max(0, ranges.start(i)), end = Math.min(duration, ranges.end(i));
		if (end > start && (target === undefined || (target >= start && target <= end))) return true;
	}
	return false;
}

export function readAbsoluteSeek(media: SeekMedia): typeof emptySeek {
	try {
		const { currentTime, duration, error } = media;
		if (error) return emptySeek;
		const known = Number.isFinite(duration) && duration > 0;
		const position = Number.isFinite(currentTime) ? Math.max(0, currentTime) : 0;
		return { position: known ? Math.min(position, duration) : position, duration: known ? duration : 0,
			enabled: known && Number.isFinite(currentTime) && seekable(media, duration) };
	} catch {
		return emptySeek;
	}
}

export function writeAbsoluteSeek(media: SeekMedia, seconds: number, matches: () => boolean): string {
	let target: number;
	try {
		const { duration, currentTime, error } = media;
		if (error || !Number.isFinite(duration) || duration <= 0 || !Number.isFinite(currentTime)) {
			return 'Seeking is unavailable for this track.';
		}
		target = Math.min(duration, Math.max(0, seconds));
		if (!seekable(media, duration, target)) return 'That position is not seekable yet.';
		if (!matches() || target === currentTime) return '';
		media.currentTime = target;
		if (!matches()) return '';
		const observed = media.currentTime;
		if (Number.isFinite(observed) && Math.abs(observed - target) <= 0.05) return '';
	} catch { /* A native refusal cannot count as a seek. */ }
	return 'The player did not accept that seek.';
}
