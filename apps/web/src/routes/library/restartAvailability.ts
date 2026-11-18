import { seekableIncludesStart } from '#lib/player/seekableIncludesStart.js';

export function restartIsAvailable(element: HTMLAudioElement): boolean {
	try {
		return (
			Number.isFinite(element.duration) &&
			element.duration > 0 &&
			Number.isFinite(element.currentTime) &&
			seekableIncludesStart(element)
		);
	} catch {
		return false;
	}
}
