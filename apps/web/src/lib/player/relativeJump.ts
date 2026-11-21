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
 * Move native media by the fixed step, clamped to [0, duration]. Media
 * without a finite positive duration, a finite position, or a native
 * seekable range covering the target is unavailable and left untouched (a
 * source served without range support reports only [0, 0]). Success needs
 * readback at the target. Never plays or loads, so paused/playing intent is
 * unchanged.
 */
export function jumpMedia(
	media: JumpMedia & { readonly seeking?: boolean }, direction: JumpDirection, matches: () => boolean = () => true, assigned?: () => void,
	seconds = RELATIVE_JUMP_SECONDS, onSystemUnavailable?: () => void, landed?: (target: number, duration: number) => void
): JumpOutcome {
	let duration: number, position: number, target: number, raw: number;
	try {
		({ duration, currentTime: position } = media);
		if (!Number.isFinite(duration) || duration <= 0 || !Number.isFinite(position) || (onSystemUnavailable && 'error' in media && media.error)) {
			onSystemUnavailable?.();
			return { kind: 'unavailable' };
		}
		const back = direction === 'back';
		raw = position + (back ? -seconds : seconds);
		target = Math.min(Math.max(raw, 0), duration);
		const slack = onSystemUnavailable ? 0 : TOLERANCE;
		const atEdge = back ? position <= slack : position >= duration - slack;
		if (!(onSystemUnavailable ? seekable(media, duration, atEdge ? undefined : target, onSystemUnavailable)
			: atEdge || seekableCovers(media.seekable, target))) return { kind: 'unavailable' };
		if (atEdge) return { kind: 'boundary', edge: back ? 'start' : 'end' };
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
		if (!Number.isFinite(readback) || Math.abs(readback - target) > tolerance) return { kind: 'refused' };
		if (readback !== position || roundedEndpoint) landed?.(target, duration);
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
