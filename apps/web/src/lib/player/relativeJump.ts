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

type JumpMedia = Pick<HTMLMediaElement, 'currentTime' | 'duration' | 'seekable'>;

// Readback slack for float/quantised positions, as in the restart readback.
const TOLERANCE = 0.05;

function seekableCovers(ranges: TimeRanges, target: number): boolean {
	for (let index = 0; index < ranges.length; index += 1) {
		if (ranges.start(index) <= target + TOLERANCE && ranges.end(index) >= target - TOLERANCE) {
			return true;
		}
	}
	return false;
}

/**
 * Move native media by a step, clamped to [0, duration]. Media
 * without a finite positive duration, a finite position, or a native
 * seekable range covering the target is unavailable and left untouched (a
 * source served without range support reports only [0, 0]). Success needs
 * readback at the target. Never plays or loads, so paused/playing intent is
 * unchanged.
 */
export function jumpMedia(
	media: JumpMedia, direction: JumpDirection, matches: () => boolean = () => true, assigned?: () => void,
	seconds = RELATIVE_JUMP_SECONDS, onSystemUnavailable?: () => void
): JumpOutcome {
	const back = direction === 'back';
	const boundary = { kind: 'boundary', edge: back ? 'start' : 'end' } as const;
	let position: number;
	let target: number;
	let raw: number;
	try {
		const duration = media.duration;
		position = media.currentTime;
		if (!Number.isFinite(duration) || duration <= 0 || !Number.isFinite(position) || (onSystemUnavailable && 'error' in media && media.error)) {
			onSystemUnavailable?.();
			return { kind: 'unavailable' };
		}
		if (!onSystemUnavailable && (back ? position <= TOLERANCE : position >= duration - TOLERANCE)) return boundary;
		raw = position + (back ? -seconds : seconds);
		target = Math.min(Math.max(raw, 0), duration);
		const atEdge = onSystemUnavailable && (back ? target >= position : target <= position);
		const covered = onSystemUnavailable ? seekable(media, duration, atEdge ? undefined : target, onSystemUnavailable)
			: seekableCovers(media.seekable, target);
		if (!covered) return { kind: 'unavailable' };
		if (atEdge) return boundary;
	} catch {
		onSystemUnavailable?.();
		return { kind: 'unavailable' };
	}
	try {
		if (!matches()) return { kind: 'unavailable' };
		media.currentTime = target;
		assigned?.();
		if (!matches()) return { kind: 'unavailable' };
		const readback = media.currentTime;
		// For small system jumps, scale readback slack to the clamped displacement.
		const tolerance = onSystemUnavailable ? Math.min(TOLERANCE, Math.abs(target - position) / 2) : TOLERANCE;
		if (!Number.isFinite(readback) || Math.abs(readback - target) > tolerance) {
			return { kind: 'refused' };
		}
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
