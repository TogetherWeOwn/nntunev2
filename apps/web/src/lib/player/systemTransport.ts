import type { Readable } from 'svelte/store';
import { displayArtist, displayTitle } from '../library/order.js';
import {
	applyMediaSession, clearMediaSession, getMediaSession, setMediaSessionPosition, type MediaSessionHandlers
} from './mediaSession.js';
import type { QueueState, Track } from './queue.js';
import { bindSystemSeek, type SystemSeekOwner } from './systemSeek.js';

/** The native attachment owns this registration, not a route or queue revision. */
export function bindSystemTransport(
	queue: Readable<QueueState>,
	audio: HTMLAudioElement,
	readTrack: () => Track | null,
	{ playCurrent, pauseCurrent, onNext, onPrevious }: Pick<MediaSessionHandlers, 'onNext' | 'onPrevious'> & {
		playCurrent: () => Promise<void>; pauseCurrent: () => void;
	},
	signal: AbortSignal, seekOwner: SystemSeekOwner
): void {
	const session = getMediaSession();
	if (!session || signal.aborted) return;
	const options = { session };
	const guard = (action: () => void) => () => {
		// Unregistering cannot recall a callback already queued by the OS.
		if (!signal.aborted && readTrack() && !signal.aborted) action();
	};
	const handlers = {
		onPlay: guard(() => { void playCurrent().catch(() => { /* OS controls have no error channel. */ }); }),
		onPause: guard(pauseCurrent),
		onNext: onNext && guard(onNext), onPrevious: onPrevious && guard(onPrevious)
	};
	let published: Track | null = null;
	let unsubscribe: (() => void) | undefined;
	signal.addEventListener('abort', () => {
		unsubscribe?.();
		if (published) clearMediaSession(options);
	}, { once: true });
	function readPosition(track: Track) {
		const state = !audio.error && audio.readyState >= audio.HAVE_METADATA ? {
			duration: audio.duration, position: audio.currentTime, playbackRate: audio.playbackRate
		} : null;
		// A read from the decoder must not outlive its source or attachment.
		return readTrack() === track && !signal.aborted ? state : undefined;
	}
	function observePosition(event: Event): void {
		if (signal.aborted || event.target !== audio) return;
		const track = readTrack();
		if (!track || track !== published) return;
		const state = readPosition(track);
		if (state !== undefined) setMediaSessionPosition(state, options);
	}
	for (const type of ['loadedmetadata', 'durationchange', 'timeupdate', 'seeking', 'seeked',
		'ratechange', 'playing', 'pause', 'ended', 'emptied', 'error']) {
		audio.addEventListener(type, observePosition, { signal });
	}
	let systemSeek: ReturnType<typeof bindSystemSeek> | undefined;
	unsubscribe = queue.subscribe(() => {
		systemSeek?.refresh();
		if (signal.aborted) return;
		const track = readTrack();
		if (!track) {
			if (published && !signal.aborted) {
				published = null;
				clearMediaSession(options);
			}
			return;
		}
		if (track === published) return;
		const positionState = readPosition(track);
		if (positionState === undefined) return;
		published = track;
		systemSeek ??= bindSystemSeek(session, seekOwner, signal);
		if (signal.aborted) return;
		// Match the compact identity; don't introduce OS requests for external cover art.
		applyMediaSession({ ...track, title: displayTitle(track), artist: displayArtist(track) ?? '', artworkUrl: null },
			{ ...handlers, ...systemSeek.handlers }, { ...options, positionState });
	});
	if (signal.aborted) unsubscribe();
}
