import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { demoTrackSalt, expectedSyntheticBody, firstByteDifference } from '../../helpers/demo-audio.js';
import { mediaSessionFailure } from '../../helpers/media-session.js';
import { playbackFailure, type PlaybackEvidence } from '../../helpers/playback.js';
import { seekProofFailure, seekViaPlayerUi, waitForPaused } from '../../helpers/player-ui.js';
import { assertPrivateMeteredPlay } from '../../helpers/private-music-journey.js';
import { privateStagingAudio } from '../../helpers/private-staging-fixture.js';
import { queueOrderFailure } from '../../helpers/queue-order.js';
import { assertStagingPlaybackProbe } from '../../helpers/staging-playback-probe.js';
import { privateCutoffObservation, privateRunIdentity, privateTrackObservation } from '../../private-staging/track-evidence.js';
import { uploadToLibrary } from '../../private-staging/upload-to-library.js';

test('demo bytes and metadata reject malformed evidence without coercion', () => {
  expect(demoTrackSalt(['second-signal'] as unknown as string)).toBe(0);
  expect(firstByteDifference(expectedSyntheticBody('constructor'), expectedSyntheticBody())).toBeNull();
  const track = { title: 'Fixture', artist: null };
  for (const src of ['https:example.test/image', 'https://user@example.test/art', 'https://exa\tmple.test/']) {
    expect(mediaSessionFailure({ ...track, artworkSrcs: [src] }, track)).toMatch(/non-absolute-https/);
  }
  expect(mediaSessionFailure({ ...track, artworkSrcs: new Array<string>(1) }, track)).toMatch(/non-absolute-https/);
});

test('playback and seek require bounded positions and actual state', () => {
  const sample: PlaybackEvidence = {
    sawPlaying: true, maxCurrentTime: 5, paused: false, ended: false,
    currentTime: 5, duration: 10, readyState: 4, errorCode: null, hasSource: true,
  };
  expect(playbackFailure(sample, 1)).toBeNull();
  expect(playbackFailure(sample, -Infinity)).not.toBeNull();
  for (const shape of [{ paused: true }, { currentTime: -1 }, { maxCurrentTime: 11 }, { readyState: NaN }]) {
    expect(playbackFailure({ ...sample, ...shape }, 1)).not.toBeNull();
  }
  const paused = { ...sample, paused: true };
  expect(seekProofFailure(paused, 0.5, true)).toBeNull();
  expect(seekProofFailure({ ...paused, currentTime: NaN }, 0.5, true)).not.toBeNull();
  expect(seekProofFailure(paused, NaN, true)).not.toBeNull();
});

test('invalid and ambiguous controls fail before any action', async () => {
  const page = { getByTestId: () => ({ count: async () => 2 }) } as unknown as Page;
  const player = {} as Locator;
  await expect(seekViaPlayerUi(page, player, Infinity)).rejects.toThrow('Seek fraction');
  await expect(seekViaPlayerUi(page, player, 0.5)).rejects.toThrow('Ambiguous');
  await expect(waitForPaused(player, true, NaN, 'pause')).rejects.toThrow('positive timeout');
});

test('queue and metering refuse malformed rows and vacuous identities', () => {
  const invalid = ['', 'https://example.test/private-detail'];
  expect(queueOrderFailure(invalid, invalid)).toMatch(/valid track ids/);
  expect(queueOrderFailure(['track-a', 'track-a'], ['track-a', 'track-b'])).toMatch(/duplicate/);
  const play = { userId: '', trackId: 'track-a', playId: 'play-a', fileBytes: 10, durationMs: 500,
    delivered: [{ start: 0, end: 10 }], unionBytes: 10, streamSeconds: 1 };
  expect(() => assertPrivateMeteredPlay(play, play)).toThrow('bound');
});

test('track annotations project validated evidence before callbacks can qualify it', async () => {
  const run = privateRunIdentity('a'.repeat(40), '1', '2');
  const trackId = '00000000-0000-4000-8000-000000000001';
  const sha256 = 'b'.repeat(64);
  const publication = { trackId, sha256 };
  const proof = privateTrackObservation({ ...run, sha256: 'redacted-marker' } as typeof run, publication, trackId);
  expect(proof).toEqual({ ...run, track_id: trackId, sha256 });
  expect(() => privateRunIdentity('invalid', '1', '2')).toThrow('build SHA');
  const unexpected = async () => { throw new Error('Unexpected callback'); };
  await expect(privateCutoffObservation({}, '', '', unexpected, unexpected)).rejects.toThrow('bindings');
  await expect(uploadToLibrary(run, 'invalid', [], unexpected, unexpected)).rejects.toThrow('digest');
  await expect(uploadToLibrary(run, sha256, [], async () => ({
    status: 200, text: JSON.stringify(publication).padEnd(1025),
  }), unexpected)).rejects.toThrow('bounded receipt');
});

test('probe disposes a redirect and spends no budget on a follow-up request', async () => {
  const origin = 'https://staging.nntune.com';
  const [fixture] = privateStagingAudio();
  let requested = 0;
  let disposed = 0;
  const request = { get: async (_url: string, options: Parameters<APIRequestContext['get']>[1]) => {
    requested += 1;
    expect(options?.maxRedirects).toBe(0);
    expect(options?.timeout).toBe(30_000);
    return { status: () => 302, headers: () => ({}), dispose: async () => { disposed += 1; } };
  } } as unknown as APIRequestContext;
  const env = { STAGING_BASE_URL: origin, CF_ACCESS_CLIENT_ID: 'offline-id', CF_ACCESS_CLIENT_SECRET: 'offline-secret' };
  const previous = Object.entries(env).map(([key]) => [key, process.env[key]] as const);
  Object.assign(process.env, env);
  try {
    await expect(assertStagingPlaybackProbe({ request, trackId: 'track-a', fixtureSha256: fixture!.sha256,
      sourceUrl: `${origin}/v1/plays/media/track-a/tab-a/private_browser_${'a'.repeat(22)}`,
    })).rejects.toThrow('unexpected redirect');
    expect([requested, disposed]).toEqual([1, 1]);
  } finally {
    for (const [key, value] of previous) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  }
});
