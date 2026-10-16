/**
 * MediaSession wiring: OS media keys / lock-screen controls follow the
 * queue. Artwork goes through `resolveArtwork` from player-resilience —
 * the placeholder fallback is reused here, never reimplemented — and is
 * absolutized, since MediaSession artwork is resolved outside the page.
 */
import { resolveArtwork } from './artwork.js';
import type { Track } from './queue.js';

export interface MediaArtworkInit {
	src: string;
	sizes?: string;
	type?: string;
}

export interface MediaMetadataInit {
	title?: string;
	artist?: string;
	album?: string;
	artwork?: MediaArtworkInit[];
}

/** Structural subset of `navigator.mediaSession` (injectable for tests). */
export interface MediaSessionLike {
	metadata: unknown;
	setActionHandler(action: string, handler: (() => void) | null): void;
}

export interface MediaSessionHandlers {
	onNext: () => void;
	onPrevious: () => void;
	onPlay?: () => void;
	onPause?: () => void;
}

export interface ApplyMediaSessionOptions {
	/** Defaults to `navigator.mediaSession` when present. Null = unsupported. */
	session?: MediaSessionLike | null;
	/** Defaults to the global `MediaMetadata` constructor when present. */
	MetadataCtor?: new (init: MediaMetadataInit) => unknown;
	/** Base for absolutizing artwork; defaults to `location.origin`. */
	baseUrl?: string;
}

const ARTWORK_SIZES = '512x512';

export function buildMediaMetadataInit(track: Track, baseUrl: string): MediaMetadataInit {
	const resolved = resolveArtwork(track.artworkUrl ?? null, { alt: `${track.title} artwork` });
	const src = new URL(resolved.src, baseUrl).toString();
	return {
		title: track.title,
		artist: track.artist,
		...(track.album ? { album: track.album } : {}),
		artwork: [{ src, sizes: ARTWORK_SIZES }]
	};
}

function defaultSession(): MediaSessionLike | null {
	if (typeof navigator === 'undefined' || !('mediaSession' in navigator)) return null;
	return navigator.mediaSession as unknown as MediaSessionLike;
}

function defaultMetadataCtor(): (new (init: MediaMetadataInit) => unknown) | null {
	if (typeof MediaMetadata === 'undefined') return null;
	return MediaMetadata as unknown as new (init: MediaMetadataInit) => unknown;
}

function setHandler(
	session: MediaSessionLike,
	action: string,
	handler: (() => void) | null | undefined
): void {
	try {
		session.setActionHandler(action, handler ?? null);
	} catch {
		// Some engines throw for unknown actions; one bad action must not
		// take down the rest of the transport wiring.
	}
}

const NO_HANDLERS: MediaSessionHandlers = { onNext: () => {}, onPrevious: () => {} };

/**
 * Drop metadata and every action handler. Call on player teardown: the
 * OS controls are global and would otherwise keep driving a destroyed page.
 */
export function clearMediaSession(options: ApplyMediaSessionOptions = {}): boolean {
	return applyMediaSession(null, NO_HANDLERS, options);
}

/**
 * Point MediaSession at `track` (or clear it when null). Returns false
 * when the API is unavailable so callers can stay silent on old engines.
 */
export function applyMediaSession(
	track: Track | null,
	handlers: MediaSessionHandlers,
	options: ApplyMediaSessionOptions = {}
): boolean {
	const session = options.session === undefined ? defaultSession() : options.session;
	if (session === null) return false;

	if (track === null) {
		session.metadata = null;
		setHandler(session, 'nexttrack', null);
		setHandler(session, 'previoustrack', null);
		setHandler(session, 'play', null);
		setHandler(session, 'pause', null);
		return true;
	}

	const MetadataCtor = options.MetadataCtor ?? defaultMetadataCtor();
	if (MetadataCtor !== null) {
		const baseUrl =
			options.baseUrl ?? (typeof location === 'undefined' ? 'http://localhost' : location.origin);
		session.metadata = new MetadataCtor(buildMediaMetadataInit(track, baseUrl));
	}
	setHandler(session, 'nexttrack', handlers.onNext);
	setHandler(session, 'previoustrack', handlers.onPrevious);
	setHandler(session, 'play', handlers.onPlay);
	setHandler(session, 'pause', handlers.onPause);
	return true;
}
