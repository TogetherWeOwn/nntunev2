/**
 * Browser-playback verdict for the staging E2E.
 *
 * The Range spec proves the server answers 206 + Content-Range; that says
 * nothing about whether a browser can decode and play the bytes. This pure
 * helper turns a snapshot of the page's <audio> element (plus the playing /
 * timeupdate evidence recorded on it before the user gesture) into either
 * "playing and advancing" (null) or a named failure. Keeping the verdict
 * pure lets e2e/tests/playback.spec.ts pin it offline: a loaded, paused or
 * errored element must never be reported as playing.
 *
 * The synthetic staged track is sub-second (~0.1 s WAV, see
 * e2e/fixtures/synthetic.nzb), so an element that played through and ended
 * passes — but only with a recorded `playing` event and a position that
 * advanced past the baseline taken before the gesture. `ended` alone, a
 * seek to the end, or metadata alone never passes.
 */

export interface PlaybackEvidence {
  /** A `playing` event fired (or the recorder found the element unpaused). */
  sawPlaying: boolean;
  /** Highest currentTime observed by the recorder / snapshot, in seconds. */
  maxCurrentTime: number;
  paused: boolean;
  ended: boolean;
  currentTime: number;
  duration: number;
  /** HTMLMediaElement.readyState (2 = HAVE_CURRENT_DATA). */
  readyState: number;
  /** MediaError.code, or null when the element has no error. */
  errorCode: number | null;
  /** The element has a resolved currentSrc/src. Never the URL itself. */
  hasSource: boolean;
}

/**
 * Return null when the evidence proves playback actually advanced past
 * `baselineTime` (seconds, measured before the gesture), otherwise a
 * human-readable failure naming the missing proof. Messages contain only
 * numbers/booleans from the snapshot — never a URL or source value.
 */
export function playbackFailure(evidence: PlaybackEvidence, baselineTime: number): string | null {
  if (evidence.errorCode !== null) {
    return `audio element reported media error ${evidence.errorCode}`;
  }
  if (!evidence.hasSource) {
    return 'audio element has no source (the synthetic track was never loaded into the player)';
  }
  if (!Number.isFinite(evidence.duration) || evidence.duration <= 0) {
    return `audio duration is not a positive finite number (duration=${evidence.duration}) — metadata never decoded`;
  }
  const playing = evidence.sawPlaying || (!evidence.paused && !evidence.ended);
  // A recorded `playing` event already implies a decoded frame; without one
  // require HAVE_CURRENT_DATA so a merely-unpaused element cannot pass.
  if (!playing || (!evidence.sawPlaying && evidence.readyState < 2)) {
    return (
      `audio element is not playing (paused=${evidence.paused}, ended=${evidence.ended}, ` +
      `sawPlaying=${evidence.sawPlaying}, readyState=${evidence.readyState})`
    );
  }
  if (!Number.isFinite(evidence.maxCurrentTime) || !Number.isFinite(evidence.currentTime)) {
    return `audio position is not finite (currentTime=${evidence.currentTime}, maxCurrentTime=${evidence.maxCurrentTime})`;
  }
  if (!(evidence.maxCurrentTime > baselineTime)) {
    return (
      `audio position did not advance past the pre-gesture baseline ` +
      `(maxCurrentTime=${evidence.maxCurrentTime}, baseline=${baselineTime})`
    );
  }
  return null;
}
