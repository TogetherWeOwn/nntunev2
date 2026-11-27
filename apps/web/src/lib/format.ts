function validMediaTime(seconds: number): boolean {
	return Number.isFinite(seconds) && seconds >= 0 && seconds <= Number.MAX_SAFE_INTEGER;
}

/** Format seconds as `m:ss`, or `h:mm:ss` once an hour or more. */
export function formatDuration(totalSeconds: number): string {
	const whole = validMediaTime(totalSeconds) ? Math.floor(totalSeconds) : 0;
	const hours = Math.floor(whole / 3600);
	const minutes = Math.floor((whole % 3600) / 60);
	const seconds = whole % 60;
	const mm = hours > 0 ? String(minutes).padStart(2, '0') : String(minutes);
	const ss = String(seconds).padStart(2, '0');
	return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}

/** Display strings for the current track's elapsed/remaining/total media time. */
export interface TrackTime {
	elapsed: string;
	remaining: string | null;
	duration: string | null;
}

/**
 * Derive elapsed/remaining/total display strings from media seconds. An
 * unknown duration (outside the safe nonnegative range) yields null coordinates, never
 * zero; a finite zero duration is known. Unknown elapsed normalizes to zero,
 * finite elapsed clamps to `[0, duration]` when known, and fractions floor
 * after clamping/subtraction.
 */
export function deriveTrackTime(currentTime: number, duration: number): TrackTime {
	const elapsedRaw = validMediaTime(currentTime) ? currentTime : 0;
	const known = validMediaTime(duration);
	const clamped = known ? Math.min(elapsedRaw, duration) : elapsedRaw;
	return {
		elapsed: formatDuration(clamped),
		remaining: known ? formatDuration(duration - clamped) : null,
		duration: known ? formatDuration(duration) : null
	};
}

/** Format a byte count with one decimal, e.g. `1.5 KB`. */
export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let index = Math.min(
		units.length - 1,
		Math.max(0, Math.floor(Math.log(bytes) / Math.log(1024)))
	);
	let rounded = Math.round((bytes / 1024 ** index) * 10) / 10;
	// A value that rounds up to 1024 belongs in the next unit (e.g. 1048575 B -> "1 MB").
	if (rounded >= 1024 && index < units.length - 1) {
		index += 1;
		rounded = Math.round((bytes / 1024 ** index) * 10) / 10;
	}
	return `${Number.isInteger(rounded) && index > 0 ? rounded.toFixed(0) : rounded} ${units[index]}`;
}
