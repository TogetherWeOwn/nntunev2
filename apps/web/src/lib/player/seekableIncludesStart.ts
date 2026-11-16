// True when position 0 lies inside a native seekable range. Any unreadable
// or non-finite range bound is unusable media, never an exception.
export function seekableIncludesStart(element: HTMLAudioElement): boolean {
	try {
		const ranges = element.seekable;
		for (let index = 0; index < ranges.length; index += 1) {
			const start = ranges.start(index);
			const end = ranges.end(index);
			if (Number.isFinite(start) && Number.isFinite(end) && start <= 0 && end >= 0) {
				return true;
			}
		}
	} catch {
		// Fall through: an unreadable range set cannot restart.
	}
	return false;
}
