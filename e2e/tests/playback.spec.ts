import { test, expect } from '@playwright/test';
import { playbackFailure, type PlaybackEvidence } from '../helpers/playback.js';

// Offline acceptance for the browser-playback verdict used by the live
// "synthetic audio actually plays in the browser" spec in
// ingest-play-meter.spec.ts. No staging, no credentials, no skip guard:
// these cases pin that an HTTP 206 / loaded / paused element can never be
// reported as playing, while a genuinely advancing element can.
//
// The synthetic staged track is a ~0.1 s WAV (1724 bytes — see
// e2e/fixtures/synthetic.nzb), so "played through and ended" is a pass:
// by the time a poll samples the element it may already be ended. Ended
// only passes together with recorded playing/advancement evidence.

function evidence(overrides: Partial<PlaybackEvidence> = {}): PlaybackEvidence {
  return {
    sawPlaying: false,
    maxCurrentTime: 0,
    paused: true,
    ended: false,
    currentTime: 0,
    duration: 0.11,
    readyState: 4,
    errorCode: null,
    hasSource: true,
    ...overrides,
  };
}

test.describe('playbackFailure (browser playback verdict, offline fixtures)', () => {
  test('playing with an advanced position passes', () => {
    expect(
      playbackFailure(evidence({ sawPlaying: true, paused: false, currentTime: 0.05, maxCurrentTime: 0.05 }), 0),
    ).toBeNull();
  });

  test('played through to ended passes for the sub-second synthetic track', () => {
    expect(
      playbackFailure(
        evidence({ sawPlaying: true, paused: true, ended: true, currentTime: 0.11, maxCurrentTime: 0.11 }),
        0,
      ),
    ).toBeNull();
  });

  test('loaded but paused with no advancement fails (a 206 alone is not playback)', () => {
    const failure = playbackFailure(evidence(), 0);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/not playing|did not advance/);
  });

  test('playing event without any position advancement fails', () => {
    expect(playbackFailure(evidence({ sawPlaying: true, paused: false }), 0)).toMatch(/did not advance/);
  });

  test('position that only reached the baseline fails', () => {
    expect(
      playbackFailure(evidence({ sawPlaying: true, paused: false, currentTime: 0.04, maxCurrentTime: 0.04 }), 0.04),
    ).toMatch(/did not advance/);
  });

  test('a media error fails even if the position advanced', () => {
    expect(
      playbackFailure(
        evidence({ sawPlaying: true, paused: false, currentTime: 0.05, maxCurrentTime: 0.05, errorCode: 4 }),
        0,
      ),
    ).toMatch(/media error 4/);
  });

  test('no source fails', () => {
    expect(playbackFailure(evidence({ hasSource: false }), 0)).toMatch(/no source/);
  });

  test('unknown or zero duration fails (metadata never decoded)', () => {
    expect(
      playbackFailure(
        evidence({ sawPlaying: true, paused: false, currentTime: 0.05, maxCurrentTime: 0.05, duration: Number.NaN }),
        0,
      ),
    ).toMatch(/duration/);
    expect(
      playbackFailure(
        evidence({ sawPlaying: true, paused: false, currentTime: 0.05, maxCurrentTime: 0.05, duration: 0 }),
        0,
      ),
    ).toMatch(/duration/);
  });

  test('readyState below HAVE_CURRENT_DATA without a playing event fails', () => {
    expect(
      playbackFailure(evidence({ paused: false, currentTime: 0.05, maxCurrentTime: 0.05, readyState: 1 }), 0),
    ).toMatch(/readyState|not playing/);
  });

  test('ended without playing or advancement fails', () => {
    expect(playbackFailure(evidence({ ended: true, currentTime: 0.11, maxCurrentTime: 0 }), 0)).not.toBeNull();
  });

  test('non-finite positions fail instead of passing vacuously', () => {
    expect(
      playbackFailure(
        evidence({ sawPlaying: true, paused: false, currentTime: Number.NaN, maxCurrentTime: Number.NaN }),
        0,
      ),
    ).not.toBeNull();
  });
});
