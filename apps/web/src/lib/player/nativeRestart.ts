/** Restart is a native seek, never a play/pause/load or queue operation. */
export function restartIsAvailable(element: HTMLAudioElement): boolean {
	try {
		if (!Number.isFinite(element.duration) || element.duration <= 0 || !Number.isFinite(element.currentTime)) return false;
		const ranges = element.seekable;
		for (let index = 0; index < ranges.length; index += 1) {
			const start = ranges.start(index);
			const end = ranges.end(index);
			if (Number.isFinite(start) && Number.isFinite(end) && start <= 0 && end >= 0) return true;
		}
	} catch { /* Unreadable native state is unavailable. */ }
	return false;
}

export interface NativeResetBoundary {
	isCurrent: () => boolean;
	onAssigned: () => void;
}

export function restartNative(element: HTMLAudioElement, title: string, boundary?: NativeResetBoundary): string {
	const withTitle = (words: string): string => {
		const suffix = ` ${title}.`;
		if (words.length + suffix.length <= 99) return `${words}${suffix}`;
		let end = 97 - words.length;
		const last = title.charCodeAt(end - 1);
		if (last >= 0xd800 && last <= 0xdbff) end -= 1;
		return `${words} ${title.slice(0, end)}…`;
	};
	const current = () => !boundary || boundary.isCurrent();
	// Availability may have changed since the last native event.
	if (!restartIsAvailable(element) || !current()) return withTitle('Could not restart');
	let before = Number.NaN;
	try { before = element.currentTime; } catch { /* Refuse below. */ }
	if (!Number.isFinite(before) || !current()) return withTitle('Could not restart');
	if (Math.abs(before) <= 0.05) {
		let assigned = false;
		try { element.currentTime = 0; assigned = true; } catch { /* Already at the start. */ }
		if (!current()) return withTitle('Could not restart');
		if (assigned) boundary?.onAssigned();
		return current() ? withTitle('Already at the start of') : withTitle('Could not restart');
	}
	try {
		element.currentTime = 0;
	} catch { return withTitle('Could not restart'); }
	if (!current()) return withTitle('Could not restart');
	// Completion precedes readback, even when the native setter silently refuses.
	boundary?.onAssigned();
	if (!current()) return withTitle('Could not restart');
	try {
		const readback = element.currentTime;
		if (current() && Number.isFinite(readback) && Math.abs(readback) <= 0.05) return withTitle('Restarted');
	} catch { /* A throwing or sticky readback must never claim success. */ }
	return withTitle('Could not restart');
}
