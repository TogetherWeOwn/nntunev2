import { formatDuration } from '../format.js';

/** Seconds moved by one Back/Forward activation. */
export const RELATIVE_JUMP_SECONDS = 10;

export type JumpDirection = 'back' | 'forward';

export type JumpOutcome =
	| { kind: 'jumped'; target: number; clamped: boolean }
	| { kind: 'boundary'; edge: 'start' | 'end' }
	| { kind: 'unavailable' }
	| { kind: 'refused' };

type JumpMedia = Pick<HTMLMediaElement, 'currentTime' | 'duration'>;

/**
 * Move native media by the fixed step, clamped to [0, duration]. Media
 * without a finite positive duration and an in-range position is
 * unavailable and left untouched. Never plays or loads, so paused/playing
 * intent is unchanged.
 */
export function jumpMedia(media: JumpMedia, direction: JumpDirection): JumpOutcome {
	const position = media.currentTime;
	const duration = media.duration;
	if (
		!Number.isFinite(duration) ||
		duration <= 0 ||
		!Number.isFinite(position) ||
		position < 0 ||
		position > duration
	) {
		return { kind: 'unavailable' };
	}
	const step = direction === 'back' ? -RELATIVE_JUMP_SECONDS : RELATIVE_JUMP_SECONDS;
	const raw = position + step;
	const target = Math.min(Math.max(raw, 0), duration);
	if (target === position) {
		return { kind: 'boundary', edge: direction === 'back' ? 'start' : 'end' };
	}
	try {
		media.currentTime = target;
	} catch {
		return { kind: 'refused' };
	}
	return { kind: 'jumped', target, clamped: target !== raw };
}

/** Polite status text; only a jump that happened is described as one. */
export function describeJump(direction: JumpDirection, outcome: JumpOutcome): string {
	const back = direction === 'back';
	const word = back ? 'Back' : 'Forward';
	switch (outcome.kind) {
		case 'jumped': {
			const at = formatDuration(outcome.target);
			return outcome.clamped
				? `${word} to the ${back ? 'start' : 'end'}, ${at}.`
				: `${word} ${RELATIVE_JUMP_SECONDS} seconds, now at ${at}.`;
		}
		case 'boundary':
			return `Already at the ${outcome.edge}.`;
		case 'unavailable':
			return 'Cannot jump yet: this track has not reported its length.';
		case 'refused':
			return 'The player did not accept that jump.';
	}
}
