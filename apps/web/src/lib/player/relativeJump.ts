import { formatDuration } from '../format.js';
import { seekable } from './absoluteSeek.js';

/** Seconds moved by one Back/Forward activation. */
export const RELATIVE_JUMP_SECONDS = 10;

export type JumpDirection = 'back' | 'forward';

export type JumpOutcome =
	| { kind: 'jumped'; target: number; clamped: boolean }
	| { kind: 'boundary'; edge: 'start' | 'end' }
	| { kind: 'unavailable' }
	| { kind: 'refused' };

type JumpMedia = Pick<HTMLMediaElement, 'currentTime' | 'duration' | 'seekable'> & Partial<Pick<HTMLMediaElement, 'error'>>;

// Readback slack for float/quantised positions, as in the restart readback.
const TOLERANCE = 0.05;

function seekableCovers(ranges: TimeRanges, target: number): boolean {
	for (let index = 0; index < ranges.length; index += 1) {
		const start = ranges.start(index), end = ranges.end(index);
		if (Number.isFinite(start) && Number.isFinite(end) && end > start &&
			start <= target + TOLERANCE && end >= target - TOLERANCE) {
			return true;
		}
	}
	return false;
}

/**
 * Move native media by the fixed step, clamped to [0, duration]. Media
 * without a finite positive duration, a finite position, or a native
 * seekable range covering the target is unavailable and left untouched (a
 * source served without range support reports only [0, 0]). Success needs
 * readback at the target. Never plays or loads, so paused/playing intent is
 * unchanged.
 */
export function jumpMedia(
	media: JumpMedia & { readonly seeking?: boolean }, direction: JumpDirection, matches: () => boolean = () => true, assigned?: () => void,
	seconds = RELATIVE_JUMP_SECONDS, onSystemUnavailable?: () => void, landed?: (target: number, duration: number, observed: number) => void
): JumpOutcome {
	if ((direction !== 'back' && direction !== 'forward') || !Number.isFinite(seconds) || seconds <= 0) {
		return { kind: 'unavailable' };
	}
	let duration: number, position: number;
	let target: number;
	let raw: number;
	try {
		({ duration, currentTime: position } = media);
		if (!Number.isFinite(duration) || duration <= 0 || !Number.isFinite(position) || media.error) {
			onSystemUnavailable?.();
			return { kind: 'unavailable' };
		}
		const back = direction === 'back';
		const slack = onSystemUnavailable ? 0 : TOLERANCE;
		if (back ? position <= slack : position >= duration - slack) {
			if (onSystemUnavailable && !seekable(media, duration, undefined, onSystemUnavailable)) return { kind: 'unavailable' };
			return { kind: 'boundary', edge: back ? 'start' : 'end' };
		}
		raw = position + (back ? -seconds : seconds);
		target = Math.min(Math.max(raw, 0), duration);
		if (!(onSystemUnavailable ? seekable(media, duration, target, onSystemUnavailable) : seekableCovers(media.seekable, target))) return { kind: 'unavailable' };
		if (onSystemUnavailable && target === position) return { kind: 'refused' };
	} catch {
		onSystemUnavailable?.();
		return { kind: 'unavailable' };
	}
	try {
		const canStartEndpointSeek = onSystemUnavailable && target === duration && media.seeking === false;
		if (!matches()) return { kind: 'unavailable' };
		media.currentTime = target;
		assigned?.();
		if (!landed && !matches()) return { kind: 'unavailable' };
		const readback = media.currentTime;
		const roundedEndpoint = readback === position && canStartEndpointSeek && media.seeking;
		const tolerance = onSystemUnavailable && !roundedEndpoint ? Math.min(TOLERANCE, Math.abs(target - position) / 2) : TOLERANCE;
		if (!Number.isFinite(readback) || Math.abs(readback - target) > tolerance) {
			return { kind: 'refused' };
		}
		if (readback !== position || roundedEndpoint) landed?.(target, duration, readback);
	} catch {
		return { kind: 'refused' };
	}
	return { kind: 'jumped', target, clamped: target !== raw };
}

/** Polite status text; only a jump that happened is described as one. */
export function describeJump(direction: JumpDirection, outcome: JumpOutcome, seconds = RELATIVE_JUMP_SECONDS): string {
	const back = direction === 'back';
	const word = back ? 'Back' : 'Forward';
	switch (outcome.kind) {
		case 'jumped': {
			const at = formatDuration(outcome.target);
			return outcome.clamped
				? `${word} to the ${back ? 'start' : 'end'}, ${at}.`
				: `${word} ${seconds} ${seconds === 1 ? 'second' : 'seconds'}, now at ${at}.`;
		}
		case 'boundary':
			return `Already at the ${outcome.edge}.`;
		case 'unavailable':
			return 'Cannot jump: this track cannot seek yet.';
		case 'refused':
			return 'The player did not accept that jump.';
	}
}
