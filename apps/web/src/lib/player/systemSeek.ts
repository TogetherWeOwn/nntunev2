import { derived, get, type Writable } from 'svelte/store';
import { readAbsoluteSeek, type AbsoluteSeekState } from './absoluteSeek.js';
import { setSeekHandlers, type MediaSessionHandlers, type MediaSessionLike, type MediaSessionSeekDetails } from './mediaSession.js';
import { RELATIVE_JUMP_SECONDS } from './relativeJump.js';

export type SystemSeekGuard = { active: () => boolean; unavailable: () => void };

export type SystemSeekOwner = {
	audio: HTMLAudioElement;
	attachment: number;
	availability: Writable<AbsoluteSeekState>;
	matches: () => boolean;
	positionIntent: () => number;
	dispatch: (action: 'seekto' | 'seekforward' | 'seekbackward', seconds: number, guard: SystemSeekGuard) => void;
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
			// The OS can retain callbacks across unregistration and attachment teardown.
			const active = () => !signal.aborted && handlers === registration;
			const guard = { active, unavailable() {
				if (!active()) return;
				refresh(false);
				// Publish the loss so even an unchanged recovery observation can rebind.
				if (!signal.aborted && !handlers) owner.availability.update((state) => state.enabled && state.attachment === owner.attachment
					? { ...state, enabled: false } : state);
			} };
			const action = (name: Parameters<SystemSeekOwner['dispatch']>[0]) => (details: MediaSessionSeekDetails) => {
				if (details?.action !== name || !active()) return;
				const absolute = name === 'seekto';
				const { seekTime, seekOffset = RELATIVE_JUMP_SECONDS } = details;
				const seconds = absolute ? seekTime : seekOffset;
				if (typeof seconds !== 'number' || !Number.isFinite(seconds) || (absolute ? seconds < 0 : seconds <= 0)) return;
				const intent = owner.positionIntent();
				if (!owner.matches() || !active() || owner.positionIntent() !== intent) return;
				const { enabled } = readAbsoluteSeek(owner.audio);
				if (!owner.matches() || !active() || owner.positionIntent() !== intent) return;
				if (enabled) owner.dispatch(name, seconds, guard);
				else guard.unavailable();
			};
			const registration: SeekHandlers = {
				onSeekTo: action('seekto'), onSeekForward: action('seekforward'), onSeekBackward: action('seekbackward')
			};
			handlers = registration;
		}
		setSeekHandlers(session, handlers);
	};
	const unsubscribe = eligible.subscribe(refresh);
	if (signal.aborted) unsubscribe();
	else signal.addEventListener('abort', unsubscribe, { once: true });
	return { refresh: () => refresh(get(eligible)), get handlers() { return handlers; } };
}
