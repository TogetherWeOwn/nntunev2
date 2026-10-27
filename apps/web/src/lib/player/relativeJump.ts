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
 * Move a mounted native media element by the fixed step, clamped to
 * [0, duration]. Usable media is a finite positive native duration and a
 * finite currentTime inside it; anything else is reported as unavailable
 * without writing. A refused assignment is caught and reported once.
 * Never plays, loads or fetches, so paused/playing intent is untouched.
 */
export function jumpMedia(media: JumpMedia, direction: JumpDirection): JumpOutcome {
	let position = Number.NaN;
	let duration = Number.NaN;
	try {
		position = media.currentTime;
		duration = media.duration;
	} catch {
		return { kind: 'unavailable' };
	}
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
	switch (outcome.kind) {
		case 'jumped': {
			const at = formatDuration(outcome.target);
			if (outcome.clamped) {
				return direction === 'back' ? `Back to the start, ${at}.` : `Forward to the end, ${at}.`;
			}
			return `${direction === 'back' ? 'Back' : 'Forward'} ${RELATIVE_JUMP_SECONDS} seconds, now at ${at}.`;
		}
		case 'boundary':
			return outcome.edge === 'start' ? 'Already at the start.' : 'Already at the end.';
		case 'unavailable':
			return 'Cannot jump yet: this track has not reported its length.';
		case 'refused':
			return 'The player did not accept that jump.';
	}
}
