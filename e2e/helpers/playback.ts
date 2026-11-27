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
 * The staged track is a sub-second tone (the in-memory sine corpus — no
 * checked-in NZB fixtures since TWO-1169), so an element that played through
 * and ended passes — but only with a recorded `playing` event and a position
 * that advanced past the baseline taken before the gesture. `ended` alone, a
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
  /** The element has a resolved source. Never the URL itself. */
  hasSource: boolean;
}

type AudioWithPlaybackRecorder = HTMLAudioElement & {
  __nnPlayback?: { source: string; sawPlaying: boolean; maxCurrentTime: number };
};

/** Runs inside Locator.evaluate. Only the numeric and boolean sample leaves the page. */
export function observePlayback(element: Element, mode: 'arm' | 'reset' | 'sample'): PlaybackEvidence {
  if (!(element instanceof HTMLAudioElement) || !['arm', 'reset', 'sample'].includes(mode)) {
    throw new Error('Playback observation requires an audio element and a known mode');
  }
  const player = element as AudioWithPlaybackRecorder;
  // The staging player uses a direct src. currentSrc can still describe the
  // outgoing media while the browser is selecting the newly assigned src.
  const readSource = () => !player.seeking && player.currentSrc === player.src ? player.currentSrc : '';
  const reset = (source: string) => {
    const recorder = { source, sawPlaying: false, maxCurrentTime: 0 };
    player.__nnPlayback = recorder;
    return recorder;
  };
  const forSource = (source: string) =>
    player.__nnPlayback?.source === source ? player.__nnPlayback : reset(source);

  const source = readSource();
  let recorder = player.__nnPlayback;
  if (!recorder && mode !== 'sample') {
    const record = (event: Event) => {
      const source = readSource();
      const recorder = forSource(source);
      if (!source) return;
      recorder.sawPlaying ||= event.type === 'playing';
      recorder.maxCurrentTime = Math.max(recorder.maxCurrentTime, player.currentTime || 0);
    };
    player.addEventListener('playing', record);
    player.addEventListener('timeupdate', record);
    player.addEventListener('emptied', () => reset(''));
    player.addEventListener('seeking', () => reset(''));
    recorder = reset(source);
  } else if (recorder && (mode === 'reset' || recorder.source !== source)) {
    recorder = reset(source);
  }

  if (mode === 'arm' && recorder && source) {
    recorder.sawPlaying ||= !player.paused && !player.ended && player.readyState >= 2;
    recorder.maxCurrentTime = Math.max(recorder.maxCurrentTime, player.currentTime || 0);
  }
  return {
    sawPlaying: recorder?.sawPlaying ?? false,
    maxCurrentTime: source ? Math.max(recorder?.maxCurrentTime ?? 0, player.currentTime || 0) : 0,
    paused: player.paused,
    ended: player.ended,
    currentTime: player.currentTime,
    duration: player.duration,
    readyState: player.readyState,
    errorCode: player.error?.code ?? null,
    hasSource: Boolean(source),
  };
}

/**
 * Return null when the evidence proves playback actually advanced past
 * `baselineTime` (seconds, measured before the gesture), otherwise a
 * human-readable failure naming the missing proof. Messages contain only
 * numbers/booleans from the snapshot — never a URL or source value.
 */
export function playbackFailure(evidence: PlaybackEvidence, baselineTime: number): string | null {
  if (!evidence || [evidence.hasSource, evidence.sawPlaying, evidence.paused, evidence.ended]
    .some((value) => typeof value !== 'boolean') || !Number.isInteger(evidence.readyState) ||
    evidence.readyState < 0 || evidence.readyState > 4) return 'Invalid audio playback state';
  if (evidence.errorCode !== null) {
    return Number.isInteger(evidence.errorCode) && evidence.errorCode >= 1 && evidence.errorCode <= 4
      ? `audio element reported media error ${evidence.errorCode}` : 'Invalid audio media error';
  }
  if (!evidence.hasSource) {
    return 'audio element has no source (the synthetic track was never loaded into the player)';
  }
  if (!Number.isFinite(evidence.duration) || evidence.duration <= 0) {
    return 'audio duration is not a positive finite number — metadata never decoded';
  }
  const playing = evidence.ended ? evidence.sawPlaying : !evidence.paused;
  // A recorded `playing` event already implies a decoded frame; without one
  // require HAVE_CURRENT_DATA so a merely-unpaused element cannot pass.
  if (!playing || (!evidence.sawPlaying && evidence.readyState < 2)) {
    return (
      `audio element is not playing (paused=${evidence.paused}, ended=${evidence.ended}, ` +
      `sawPlaying=${evidence.sawPlaying}, readyState=${evidence.readyState})`
    );
  }
  if (![baselineTime, evidence.currentTime, evidence.maxCurrentTime].every((time) =>
    Number.isFinite(time) && time >= 0 && time <= evidence.duration) || evidence.currentTime > evidence.maxCurrentTime) {
    return 'audio position is outside the valid playback bounds';
  }
  if (!(evidence.maxCurrentTime > baselineTime)) {
    return (
      `audio position did not advance past the pre-gesture baseline ` +
      `(maxCurrentTime=${evidence.maxCurrentTime}, baseline=${baselineTime})`
    );
  }
  return null;
}
