import { formatDuration } from '../format.js';

/** Seconds moved by one Back/Forward activation. */
export const RELATIVE_JUMP_SECONDS = 10;

export type JumpDirection = 'back' | 'forward';

export type JumpOutcome =
	| { kind: 'jumped'; target: number; clamped: boolean }
	| { kind: 'pending'; target: number; clamped: boolean }
	| { kind: 'boundary'; edge: 'start' | 'end' }
	| { kind: 'unavailable' }
	| { kind: 'refused' };

type JumpMedia = Pick<HTMLMediaElement, 'currentTime' | 'duration'> &
	Partial<Pick<HTMLMediaElement, 'seeking' | 'seekable'>>;

function reached(media: JumpMedia, target: number): boolean {
	try {
		return !media.seeking && Number.isFinite(media.currentTime) &&
			Math.abs(media.currentTime - target) <= 0.05;
	} catch {
		return false;
	}
}

/** One bounded native seek receipt, cancelled on another gesture/source/auth. */
export function watchJump(
	media: HTMLMediaElement,
	pending: Extract<JumpOutcome, { kind: 'pending' }>,
	publish: (outcome: JumpOutcome) => void
): () => void {
	let alive = true;
	const cancel = () => {
		alive = false;
		clearTimeout(timer);
		media.removeEventListener('seeked', settled);
		media.removeEventListener('error', failed);
	};
	const finish = (success: boolean) => {
		if (!alive) return;
		cancel();
		publish(success ? { ...pending, kind: 'jumped' } : { kind: 'refused' });
	};
	const settled = () => finish(reached(media, pending.target));
	const failed = () => finish(false);
	const timer = setTimeout(failed, 2000);
	media.addEventListener('seeked', settled);
	media.addEventListener('error', failed);
	return cancel;
}

/**
 * Move a mounted native media element by the fixed step, clamped to
 * [0, duration]. Usable media is a finite positive native duration and a
 * finite currentTime inside it; anything else is reported as unavailable
 * without writing. Reported native ranges must cover the target. A refused
 * assignment is caught once; a nonthrowing assignment still needs readback.
 * A native asynchronous seek stays pending until watchJump observes seeked.
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
		const ranges = media.seekable;
		if (ranges && ranges.length > 0) {
			let covered = false;
			for (let index = 0; index < ranges.length; index += 1) {
				const start = ranges.start(index), end = ranges.end(index);
				if (Number.isFinite(start) && Number.isFinite(end) && start <= target && end >= target) {
					covered = true;
				}
			}
			if (!covered) return { kind: 'unavailable' };
		}
		media.currentTime = target;
		if (media.seeking) return { kind: 'pending', target, clamped: target !== raw };
	} catch {
		return { kind: 'refused' };
	}
	return reached(media, target)
		? { kind: 'jumped', target, clamped: target !== raw }
		: { kind: 'refused' };
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
		case 'pending':
			return 'Waiting for the player to confirm the jump.';
		case 'boundary':
			return `Already at the ${outcome.edge}.`;
		case 'unavailable':
			return 'Cannot jump yet: this track has not reported its length.';
		case 'refused':
			return 'The player did not accept that jump.';
	}
}
