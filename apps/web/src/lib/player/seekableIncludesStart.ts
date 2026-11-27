// True when position 0 lies inside a native seekable range. Any unreadable
// or non-finite range bound is unusable media, never an exception.
// Restart supplies a receipt check so a reentrant native read stops the scan.
export function seekableIncludesStart(element: HTMLAudioElement, matches = () => true, requirePositiveEnd = false): boolean {
	try {
		const ranges = element.seekable;
		for (let index = 0; matches() && index < ranges.length; index += 1) {
			if (!matches()) return false;
			const readStart = ranges.start;
			if (!matches()) return false;
			const start = readStart.call(ranges, index);
			if (!matches()) return false;
			const readEnd = ranges.end;
			if (!matches()) return false;
			const end = readEnd.call(ranges, index);
			if (!matches()) return false;
			if (Number.isFinite(start) && Number.isFinite(end) && start <= 0 && end >= 0 &&
				(!requirePositiveEnd || end > 0)) {
				return true;
			}
		}
	} catch {
		// Fall through: an unreadable range set cannot restart.
	}
	return false;
}
