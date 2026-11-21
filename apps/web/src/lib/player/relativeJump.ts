import { formatDuration } from '../format.js';

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
 * Move native media by the requested step, clamped to [0, duration]. Media
 * without a finite positive duration, a finite position, or a native
 * seekable range covering the target is unavailable and left untouched (a
 * source served without range support reports only [0, 0]). Success needs
 * readback near the target and movement in the requested direction. Never
 * plays or loads, so paused/playing intent is unchanged.
 */
export function jumpMedia(
	media: JumpMedia, direction: JumpDirection, matches: () => boolean = () => true, assigned?: () => void,
	seconds = RELATIVE_JUMP_SECONDS
): JumpOutcome {
	const back = direction === 'back';
	let position: number;
	let target: number;
	let raw: number;
	try {
		const duration = media.duration;
		position = media.currentTime;
		if (!Number.isFinite(duration) || duration <= 0 || !Number.isFinite(position)) {
			return { kind: 'unavailable' };
		}
		if (back ? position <= TOLERANCE : position >= duration - TOLERANCE) {
			return { kind: 'boundary', edge: back ? 'start' : 'end' };
		}
		raw = position + (back ? -seconds : seconds);
		target = Math.min(Math.max(raw, 0), duration);
		if (!seekableCovers(media.seekable, target)) return { kind: 'unavailable' };
	} catch {
		return { kind: 'unavailable' };
	}
	try {
		if (!matches()) return { kind: 'unavailable' };
		media.currentTime = target;
		assigned?.();
		if (!matches()) return { kind: 'unavailable' };
		const readback = media.currentTime;
		const moved = back ? readback < position : readback > position;
		if (!moved || !Number.isFinite(readback) || Math.abs(readback - target) > TOLERANCE) {
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
