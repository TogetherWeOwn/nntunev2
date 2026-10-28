/**
 * Pure helpers for the library seek timeline. Everything is derived from the
 * live media element's own `currentTime`, `duration` and `seekable`; never
 * from catalog duration or queue totals. No DOM globals beyond the element
 * handed in, so unit tests can use plain fakes.
 */

/** The slice of an audio element the timeline reads and seeks. */
export interface TimelineMedia {
	currentTime: number;
	readonly duration: number;
	readonly seekable: {
		readonly length: number;
		start(index: number): number;
		end(index: number): number;
	};
}

export interface TimelineSnapshot {
	/** Finite, non-negative position, clamped to the duration when known. */
	currentTime: number;
	/** Finite duration in seconds, or null when unknown/infinite/negative. */
	duration: number | null;
	/** Seekable ranges clipped to `[0, duration]`, dropping empty ones. */
	ranges: Array<[number, number]>;
	/** True only when a finite positive duration has a usable seekable range. */
	seekable: boolean;
}

/** Read the element once; a throwing or odd getter reads as unknown. */
export function readTimeline(media: TimelineMedia): TimelineSnapshot {
	let rawTime = 0;
	let rawDuration = Number.NaN;
	try {
		rawTime = media.currentTime;
		rawDuration = media.duration;
	} catch {
		// Unreadable element: report unknown rather than invent a position.
	}
	const duration =
		typeof rawDuration === 'number' && Number.isFinite(rawDuration) && rawDuration >= 0
			? rawDuration
			: null;
	const time = typeof rawTime === 'number' && Number.isFinite(rawTime) ? Math.max(0, rawTime) : 0;
	const ranges: Array<[number, number]> = [];
	if (duration !== null) {
		try {
			const list = media.seekable;
			for (let index = 0; index < list.length; index += 1) {
				const start = Math.max(0, list.start(index));
				const end = Math.min(duration, list.end(index));
				if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
					ranges.push([start, end]);
				}
			}
		} catch {
			// Unreadable seekable ranges mean nothing is seekable.
			ranges.length = 0;
		}
	}
	return {
		currentTime: duration === null ? time : Math.min(time, duration),
		duration,
		ranges,
		seekable: duration !== null && duration > 0 && ranges.length > 0
	};
}

/**
 * Where an activation should land: the requested time clamped to
 * `[0, duration]`, then, when it falls outside every seekable range, moved
 * to the nearest range edge. Null when nothing is seekable.
 */
export function seekTarget(snapshot: TimelineSnapshot, requested: number): number | null {
	if (!snapshot.seekable || snapshot.duration === null || !Number.isFinite(requested)) return null;
	const wanted = Math.min(Math.max(0, requested), snapshot.duration);
	let best = Number.NaN;
	let bestGap = Number.POSITIVE_INFINITY;
	for (const [start, end] of snapshot.ranges) {
		if (wanted >= start && wanted <= end) return wanted;
		const edge = wanted < start ? start : end;
		const gap = Math.abs(wanted - edge);
		if (gap < bestGap) {
			best = edge;
			bestGap = gap;
		}
	}
	return Number.isFinite(best) ? best : null;
}
