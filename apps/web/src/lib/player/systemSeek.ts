import { derived, get, type Writable } from 'svelte/store';
import { readAbsoluteSeek, type AbsoluteSeekState } from './absoluteSeek.js';
import { setSeekHandlers, type MediaSessionHandlers, type MediaSessionLike, type MediaSessionSeekDetails } from './mediaSession.js';
import { RELATIVE_JUMP_SECONDS } from './relativeJump.js';

export type SystemSeekGuard = { active: () => boolean; canReport: () => boolean; unavailable: () => void };
export type SystemSeekOwner = {
	audio: HTMLAudioElement; attachment: number; availability: Writable<AbsoluteSeekState>;
	matches: () => boolean; positionIntent: () => number;
	dispatch: (action: MediaSessionSeekDetails['action'], seconds: number, guard: SystemSeekGuard) => void;
};

export function bindSystemSeek(session: MediaSessionLike, owner: SystemSeekOwner, signal: AbortSignal) {
	const eligible = derived(owner.availability, (state) => state.attachment === owner.attachment && state.enabled);
	let handlers: Pick<MediaSessionHandlers, 'onSeekTo' | 'onSeekForward' | 'onSeekBackward'> | undefined;
	const refresh = (seekable: boolean) => {
		const enabled = !signal.aborted && seekable && owner.matches();
		if (signal.aborted || enabled === !!handlers) return;
		if (!enabled) handlers = undefined;
		else {
			const active = () => !signal.aborted && handlers === registration;
			const action = (name: MediaSessionSeekDetails['action']) => (details: MediaSessionSeekDetails) => {
				if (details?.action !== name || !active()) return;
				const { seekTime, seekOffset = RELATIVE_JUMP_SECONDS } = details, seconds = name === 'seekto' ? seekTime : seekOffset;
				if (typeof seconds !== 'number' || !Number.isFinite(seconds) || (name === 'seekto' ? seconds < 0 : seconds <= 0)) return;
				const intent = owner.positionIntent(), ownsIntent = () => owner.matches() && active() && owner.positionIntent() === intent;
				let retired = false; // Loss retires the registration, but this attempt may still report it.
				const unavailable = () => {
					if (!active() || !owner.matches() || !active()) return;
					refresh(false);
					if (!signal.aborted && !handlers) owner.availability.update((state) => state.enabled && state.attachment === owner.attachment
						? { ...state, enabled: false } : state);
					retired = true;
				};
				if (!ownsIntent()) return;
				if (!readAbsoluteSeek(owner.audio).enabled) return unavailable();
				if (ownsIntent()) owner.dispatch(name, seconds, { active, canReport: () => retired || active(), unavailable });
			};
			const registration = handlers = { onSeekTo: action('seekto'), onSeekForward: action('seekforward'), onSeekBackward: action('seekbackward') };
		}
		setSeekHandlers(session, handlers);
	};
	const unsubscribe = eligible.subscribe(refresh);
	if (signal.aborted) unsubscribe(); else signal.addEventListener('abort', unsubscribe, { once: true });
	return { refresh: () => refresh(get(eligible)), get handlers() { return handlers; } };
}
