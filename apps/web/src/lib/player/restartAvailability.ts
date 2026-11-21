import { seekableIncludesStart } from './seekableIncludesStart.js';

export function restartIsAvailable(element: HTMLAudioElement, matches = () => true): boolean {
	try {
		return (
			Number.isFinite(element.duration) && matches() &&
			element.duration > 0 && matches() &&
			Number.isFinite(element.currentTime) && matches() &&
			seekableIncludesStart(element, matches)
		);
	} catch {
		return false;
	}
}
