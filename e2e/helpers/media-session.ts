/**
 * Media Session verdict for the staging E2E.
 *
 * The playback spec proves the browser can decode the synthetic track; that
 * says nothing about the OS-visible face of playback. This pure helper turns
 * a snapshot of `navigator.mediaSession.metadata` (plus the expected
 * track identity from the library API) into either "metadata reflects the
 * playing track" (null) or a named failure. Keeping the verdict pure lets
 * e2e/tests/media-session.spec.ts pin it offline: stale, missing or
 * artwork-malformed metadata must never be reported as matching.
 *
 * Failure messages carry only booleans/counts — never titles, artists or
 * artwork URLs, which can embed the staging origin or provider detail that
 * must stay out of public CI artifacts.
 */

export interface MediaSessionSnapshot {
  /** Value of `metadata.title` at read time (absent when metadata is null). */
  title: string | null;
  /** Value of `metadata.artist` at read time (absent when metadata is null). */
  artist: string | null;
  /** Resolved `src` of each entry in `metadata.artwork` (empty when none). */
  artworkSrcs: string[];
}

export interface ExpectedTrack {
  /** Library-API title of the playing track. */
  title: string;
  /** Library-API artist, or null when the track carries none. */
  artist: string | null;
}

/** Normalize an artist for comparison: null/undefined/blank are all "absent". */
function normalizedArtist(value: string | null | undefined): string | null {
  return typeof value === 'string' && value.trim() !== '' ? value : null;
}

/** True for an absolute https:// URL (staging is https-only; see access.ts). */
function isAbsoluteHttpsUrl(value: string): boolean {
  try {
    return new URL(value).protocol === 'https:';
  } catch {
    return false;
  }
}

/**
 * Return null when the snapshot proves Media Session metadata reflects the
 * expected playing track, otherwise a human-readable failure naming the
 * missing proof. See e2e/tests/media-session.spec.ts for the offline pins.
 */
export function mediaSessionFailure(
  snapshot: MediaSessionSnapshot | null,
  expected: ExpectedTrack,
): string | null {
  if (snapshot === null) {
    return (
      'Media Session metadata is missing during browser playback ' +
        '(navigator.mediaSession.metadata is null — the player never applied it, ' +
        'or a MediaMetadata construction failure cleared it)'
    );
  }
  if (snapshot.title !== expected.title) {
    return 'Media Session title does not match the playing ingested track';
  }
  if (normalizedArtist(snapshot.artist) !== normalizedArtist(expected.artist)) {
    return 'Media Session artist does not match the playing ingested track';
  }
  // Artwork-malformed regression guard (#173): `buildMediaMetadataInit`
  // absolutizes artwork, so a relative, empty or malformed src reaching the
  // OS means construction fell back or threw — the controls must not keep
  // showing a stale or broken entry.
  if (snapshot.artworkSrcs.length === 0) {
    return 'Media Session artwork is empty for the playing track (expected the shared placeholder fallback)';
  }
  const malformed = snapshot.artworkSrcs.filter((src) => !isAbsoluteHttpsUrl(src)).length;
  if (malformed > 0) {
    return (
      `Media Session artwork has ${malformed} non-absolute-https src(s) ` +
        `of ${snapshot.artworkSrcs.length} (artwork-malformed regression)`
    );
  }
  return null;
}
