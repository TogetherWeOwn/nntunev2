import { test, expect } from '@playwright/test';
import {
  mediaSessionFailure,
  type ExpectedTrack,
  type MediaSessionSnapshot,
} from '../helpers/media-session.js';

// Offline acceptance for the Media Session verdict used by the live
// "Media Session metadata reflects the playing track" spec in
// ingest-play-meter.spec.ts. No staging, no credentials, no skip guard:
// these cases pin that missing, stale or artwork-malformed metadata can
// never be reported as matching the playing ingested track.

const TRACK: ExpectedTrack = { title: 'Synthetic Fixture', artist: 'Fixture Band' };
const ARTWORK = 'https://staging.example/artwork/placeholder-artwork.svg';

function snapshot(overrides: Partial<MediaSessionSnapshot> = {}): MediaSessionSnapshot {
  return {
    title: TRACK.title,
    artist: TRACK.artist,
    artworkSrcs: [ARTWORK],
    ...overrides,
  };
}

test.describe('mediaSessionFailure (Media Session verdict, offline fixtures)', () => {
  test('metadata reflecting the playing track passes', () => {
    expect(mediaSessionFailure(snapshot(), TRACK)).toBeNull();
  });

  test('missing metadata fails (never applied or cleared by a construction failure)', () => {
    const failure = mediaSessionFailure(null, TRACK);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/missing/);
  });

  test('stale title from a previous track fails', () => {
    expect(mediaSessionFailure(snapshot({ title: 'Previous Track' }), TRACK)).toMatch(/title/);
  });

  test('stale artist from a previous track fails', () => {
    expect(mediaSessionFailure(snapshot({ artist: 'Other Band' }), TRACK)).toMatch(/artist/);
  });

  test('blank snapshot artist matches a track with no artist', () => {
    const noArtist: ExpectedTrack = { title: TRACK.title, artist: null };
    expect(mediaSessionFailure(snapshot({ artist: '' }), noArtist)).toBeNull();
    expect(mediaSessionFailure(snapshot({ artist: null }), noArtist)).toBeNull();
  });

  test('blank snapshot artist fails against a track that has one', () => {
    expect(mediaSessionFailure(snapshot({ artist: '' }), TRACK)).toMatch(/artist/);
  });

  test('empty artwork fails (placeholder fallback never applied)', () => {
    const failure = mediaSessionFailure(snapshot({ artworkSrcs: [] }), TRACK);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/artwork is empty/);
  });

  test('relative artwork src fails (artwork-malformed regression)', () => {
    const failure = mediaSessionFailure(
      snapshot({ artworkSrcs: ['/artwork/placeholder-artwork.svg'] }),
      TRACK,
    );
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/non-absolute-https/);
  });

  test('malformed artwork src fails without throwing', () => {
    const failure = mediaSessionFailure(snapshot({ artworkSrcs: ['https://['] }), TRACK);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/non-absolute-https/);
  });

  test('non-https artwork src fails (staging is https-only)', () => {
    const failure = mediaSessionFailure(
      snapshot({ artworkSrcs: ['http://staging.example/artwork.svg'] }),
      TRACK,
    );
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/non-absolute-https/);
  });

  test('one malformed src among good ones still fails', () => {
    const failure = mediaSessionFailure(
      snapshot({ artworkSrcs: [ARTWORK, 'not-a-url'] }),
      TRACK,
    );
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/1 non-absolute-https/);
  });

  test('failure messages never interpolate track or artwork values', () => {
    const failures = [
      mediaSessionFailure(null, TRACK),
      mediaSessionFailure(snapshot({ title: 'Leaked Title' }), TRACK),
      mediaSessionFailure(snapshot({ artworkSrcs: ['https://leak.example/a.svg', 'bad'] }), TRACK),
    ];
    for (const failure of failures) {
      expect(failure).not.toBeNull();
      expect(failure).not.toContain('Leaked Title');
      expect(failure).not.toContain('leak.example');
    }
  });
});
