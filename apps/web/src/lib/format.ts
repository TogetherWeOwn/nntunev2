/** Format seconds as `m:ss`, or `h:mm:ss` once an hour or more. */
export function formatDuration(totalSeconds: number): string {
	const whole = Math.max(0, Math.floor(totalSeconds));
	const hours = Math.floor(whole / 3600);
	const minutes = Math.floor((whole % 3600) / 60);
	const seconds = whole % 60;
	const mm = hours > 0 ? String(minutes).padStart(2, '0') : String(minutes);
	const ss = String(seconds).padStart(2, '0');
	return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}

/** Format a byte count with one decimal, e.g. `1.5 KB`. */
export function formatBytes(bytes: number): string {
	if (bytes <= 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	const index = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
	const value = bytes / 1024 ** index;
	const rounded = Math.round(value * 10) / 10;
	return `${Number.isInteger(rounded) && index > 0 ? rounded.toFixed(0) : rounded} ${units[index]}`;
}
