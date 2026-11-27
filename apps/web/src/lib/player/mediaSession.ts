/**
 * MediaSession wiring: OS media keys / lock-screen controls follow the
 * queue. Artwork goes through `resolveArtwork` from player-resilience —
 * the placeholder fallback is reused here, never reimplemented — and is
 * absolutized, since MediaSession artwork is resolved outside the page.
 */
import { ARTWORK_PLACEHOLDER_SRC, resolveArtwork } from './artwork.js';
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

export interface MediaSessionSeekDetails {
	action: string;
	seekTime?: number;
	seekOffset?: number;
	fastSeek?: boolean;
}

export interface MediaSessionPosition {
	duration: number;
	position: number;
	playbackRate: number;
}

type ActionHandler = (details?: MediaSessionSeekDetails) => void;

/** Structural subset of `navigator.mediaSession` (injectable for tests). */
export interface MediaSessionLike {
	metadata: unknown;
	setActionHandler(action: string, handler: ActionHandler | null): void;
	setPositionState?(state?: MediaSessionPosition): void;
}

export interface MediaSessionHandlers {
	onNext?: () => void;
	onPrevious?: () => void;
	onPlay?: () => void;
	onPause?: () => void;
	onSeekTo?: (details: MediaSessionSeekDetails) => void;
	onSeekForward?: (details: MediaSessionSeekDetails) => void;
	onSeekBackward?: (details: MediaSessionSeekDetails) => void;
}

export interface ApplyMediaSessionOptions {
	/** Defaults to `navigator.mediaSession` when present. Null = unsupported. */
	session?: MediaSessionLike | null;
	/** Defaults to the global `MediaMetadata` constructor when present. */
	MetadataCtor?: new (init: MediaMetadataInit) => unknown;
	/** Base for absolutizing artwork; defaults to `location.origin`. */
	baseUrl?: string;
	/** Unknown/omitted duration clears any previous track's position state. */
	positionState?: MediaSessionPosition | null;
}

const ARTWORK_SIZES = '512x512';

function absolutizeArtworkSrc(src: string, baseUrl: string): string {
	try {
		const url = new URL(src, baseUrl);
		if (!url.username && !url.password && (!src.startsWith('/') ||
			(url.origin !== 'null' && url.origin === new URL(baseUrl).origin))) return url.href;
	} catch { /* Invalid artwork falls back to the shared placeholder. */ }
	try {
		return new URL(ARTWORK_PLACEHOLDER_SRC, new URL(baseUrl).origin).href;
	} catch {
		return ARTWORK_PLACEHOLDER_SRC;
	}
}

export function buildMediaMetadataInit(track: Track, baseUrl: string): MediaMetadataInit {
	const resolved = resolveArtwork(track.artworkUrl ?? null, { alt: `${track.title} artwork` });
	const src = absolutizeArtworkSrc(resolved.src, baseUrl);
	return {
		title: track.title,
		artist: track.artist,
		...(track.album ? { album: track.album } : {}),
		artwork: [{ src, sizes: ARTWORK_SIZES }]
	};
}

export function getMediaSession(): MediaSessionLike | null {
	if (typeof navigator === 'undefined') return null;
	try {
		return (navigator.mediaSession as unknown as MediaSessionLike | undefined) ?? null;
	} catch {
		return null;
	}
}

function defaultMetadataCtor(): (new (init: MediaMetadataInit) => unknown) | null {
	if (typeof MediaMetadata === 'undefined') return null;
	return MediaMetadata as unknown as new (init: MediaMetadataInit) => unknown;
}

const registeredActions = new WeakMap<MediaSessionLike, Map<string, { handler: ActionHandler; invoke: ActionHandler }>>();

function setHandler(
	session: MediaSessionLike,
	action: string,
	handler: ActionHandler | null | undefined
): void {
	let actions = registeredActions.get(session);
	if (!actions) registeredActions.set(session, actions = new Map());
	const previous = actions.get(action);
	if (previous && previous.handler === handler) return;
	// Revoke first: an OS callback can survive a rejected replacement or clear.
	actions.delete(action);
	try {
		if (!handler) {
			session.setActionHandler(action, null);
			return;
		}
		const invoke: ActionHandler = (details) => {
			if (actions.get(action)?.invoke === invoke) handler(details);
		};
		session.setActionHandler(action, invoke);
		actions.set(action, { handler, invoke });
	} catch {
		// Some engines throw for unknown actions; one bad action must not
		// take down the rest of the transport wiring.
	}
}

const SEEK_HANDLERS = [
	['seekto', 'onSeekTo'],
	['seekforward', 'onSeekForward'],
	['seekbackward', 'onSeekBackward']
] as const;
const seekingSessions = new WeakSet<MediaSessionLike>();

export function setSeekHandlers(session: MediaSessionLike, handlers?: MediaSessionHandlers): void {
	// Legacy consumers need not opt into seeking. Once opted in, every seek
	// action is replaced/cleared, even if one registration was unsupported.
	if (!seekingSessions.has(session) && !SEEK_HANDLERS.some(([, key]) => handlers?.[key])) return;
	seekingSessions.add(session);
	for (const [action, key] of SEEK_HANDLERS) {
		const callback = handlers?.[key];
		setHandler(session, action, callback ? (details = { action }) => callback(details) : null);
	}
}

/** Event-driven position updates without rebuilding metadata or transport handlers. */
export function setMediaSessionPosition(
	state: MediaSessionPosition | null,
	options: Pick<ApplyMediaSessionOptions, 'session'> = {}
): void {
	const session = options.session === undefined ? getMediaSession() : options.session;
	if (session === null) return;
	const valid =
		state !== null &&
		Number.isFinite(state.duration) && state.duration > 0 &&
		Number.isFinite(state.position) && state.position >= 0 && state.position <= state.duration &&
		Number.isFinite(state.playbackRate) && state.playbackRate > 0;
	try {
		// Calling without a state clears it. Unsupported/throwing position APIs
		// must not break metadata, transport controls or their cleanup.
		session.setPositionState?.(valid ? state : undefined);
	} catch {
		// Best effort on engines with only partial Media Session support.
	}
}

/**
 * Drop metadata and every action handler. Call on player teardown: the
 * OS controls are global and would otherwise keep driving a destroyed page.
 */
export function clearMediaSession(options: ApplyMediaSessionOptions = {}): boolean {
	return applyMediaSession(null, {}, options);
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
	const session = options.session === undefined ? getMediaSession() : options.session;
	if (session === null) return false;

	if (track === null) {
		try {
			session.metadata = null;
		} catch {
			// A setter rejecting null must not orphan OS controls: the
			// handler clears below still run via per-action isolation.
		}
		setHandler(session, 'nexttrack', null);
		setHandler(session, 'previoustrack', null);
		setHandler(session, 'play', null);
		setHandler(session, 'pause', null);
		setSeekHandlers(session);
		setMediaSessionPosition(null, { session });
		return true;
	}

	// Handlers first: a metadata failure below must never leave the
	// previous track's callbacks wired to the OS controls.
	setHandler(session, 'nexttrack', handlers.onNext);
	setHandler(session, 'previoustrack', handlers.onPrevious);
	setHandler(session, 'play', handlers.onPlay);
	setHandler(session, 'pause', handlers.onPause);
	setSeekHandlers(session, handlers);
	setMediaSessionPosition(options.positionState ?? null, { session });
	const MetadataCtor = options.MetadataCtor ?? defaultMetadataCtor();
	if (MetadataCtor !== null) {
		const baseUrl =
			options.baseUrl ?? (typeof location === 'undefined' ? 'http://localhost' : location.origin);
		try {
			session.metadata = new MetadataCtor(buildMediaMetadataInit(track, baseUrl));
		} catch {
			// Metadata is decorative next to working transport controls.
			// Drop any stale metadata so the OS does not keep showing the
			// previous track; the next good track repopulates it.
			try {
				session.metadata = null;
			} catch {
				// Best effort only; handlers above are already current.
			}
		}
	}
	return true;
}
