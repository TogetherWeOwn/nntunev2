/**
 * Cache-Control values for player media.
 *
 * Audio is per-user and grant-gated, so it must never sit in a shared
 * cache: `private` with a short `max-age`. Artwork is content-addressed
 * and immutable, so it can live in shared caches for a year.
 */

export const AUDIO_CACHE_CONTROL = 'private, max-age=60, must-revalidate';

export const ARTWORK_CACHE_CONTROL = 'public, max-age=31536000, immutable';

export type MediaKind = 'audio' | 'artwork';

/** `Cache-Control` value for a media kind. */
export function cacheControlFor(kind: MediaKind): string {
	return kind === 'audio' ? AUDIO_CACHE_CONTROL : ARTWORK_CACHE_CONTROL;
}

/** Response headers for an audio (stream play) response. */
export function audioCacheHeaders(): Record<string, string> {
	return { 'cache-control': AUDIO_CACHE_CONTROL };
}

/** Response headers for an artwork response. */
export function artworkCacheHeaders(): Record<string, string> {
	return { 'cache-control': ARTWORK_CACHE_CONTROL };
}
