import { derived, get, type Readable } from 'svelte/store';
import { readAbsoluteSeek, type AbsoluteSeekState } from './absoluteSeek.js';
import { setSeekHandlers, type MediaSessionHandlers, type MediaSessionLike, type MediaSessionSeekDetails } from './mediaSession.js';
import { RELATIVE_JUMP_SECONDS, type JumpDirection } from './relativeJump.js';

export type SystemSeekOwner = {
	audio: HTMLAudioElement;
	attachment: number;
	availability: Readable<AbsoluteSeekState>;
	matches: () => boolean;
	seek: (attachment: number, seconds: number) => void;
	jump: (direction: JumpDirection, seconds: number) => void;
};

type SeekHandlers = Pick<MediaSessionHandlers, 'onSeekTo' | 'onSeekForward' | 'onSeekBackward'>;

/** Advertise only observed seekability, and recheck it before accepting a system gesture. */
export function bindSystemSeek(session: MediaSessionLike, owner: SystemSeekOwner, signal: AbortSignal) {
	const eligible = derived(owner.availability, (state) => state.attachment === owner.attachment && state.enabled);
	let handlers: SeekHandlers | undefined;
	const refresh = (seekable: boolean) => {
		if (signal.aborted) return;
		const enabled = seekable && owner.matches();
		if (signal.aborted || enabled === !!handlers) return;
		if (!enabled) handlers = undefined;
		else {
			const registration: SeekHandlers = {
				onSeekTo(details) {
					const seconds = details?.seekTime;
					if (details?.action === 'seekto' && typeof seconds === 'number' &&
						Number.isFinite(seconds) && seconds >= 0 && canDispatch()) {
						// Use the exact seek and readback path even when fastSeek is suggested.
						owner.seek(owner.attachment, seconds);
					}
				},
				onSeekForward: relative('seekforward', 'forward'),
				onSeekBackward: relative('seekbackward', 'back')
			};
			function canDispatch() {
				// The OS can retain callbacks across both unregistration and attachment teardown.
				const current = () => !signal.aborted && handlers === registration;
				return current() && owner.matches() && readAbsoluteSeek(owner.audio).enabled && owner.matches() && current();
			}
			function relative(action: string, direction: JumpDirection) {
				return (details: MediaSessionSeekDetails) => {
					if (details?.action !== action) return;
					const seconds = details.seekOffset === undefined ? RELATIVE_JUMP_SECONDS : details.seekOffset;
					if (typeof seconds === 'number' && Number.isFinite(seconds) && seconds > 0 && canDispatch()) {
						owner.jump(direction, seconds);
					}
				};
			}
			handlers = registration;
		}
		setSeekHandlers(session, handlers);
	};
	const unsubscribe = eligible.subscribe(refresh);
	if (signal.aborted) unsubscribe();
	else signal.addEventListener('abort', unsubscribe, { once: true });
	return { refresh: () => refresh(get(eligible)), get handlers() { return handlers; } };
}
