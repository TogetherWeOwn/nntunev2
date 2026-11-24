import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import { parsePrivateStagingFixture, privateStagingAudio } from '../../helpers/private-staging-fixture.js';
import {
  assertCuratedLibrary,
  assertPrivateInvite,
  assertPrivateRefusals,
  assertPrivateTransport,
} from '../../helpers/private-music-journey.js';

const clips = new URL('../../../fixtures/synthetic-flac/admin-upload/', import.meta.url);

test('staging preflight rejects stale, non-synthetic and ambiguous evidence', () => {
  const now = 1_800_000_000;
  const fixture = {
    email: 'listener@example.test', inviteCode: 'A'.repeat(22), expiresAt: now + 180,
    publications: privateStagingAudio().map(({ sha256, body }, index) => {
      expect(body.length).toBeGreaterThan(69631);
      return { trackId: `fixture-${index}`, sha256 };
    }),
  };
  expect(parsePrivateStagingFixture(JSON.stringify(fixture), now)).toEqual(fixture);
  for (const invalid of [
    { ...fixture, email: 'listener@example.com' },
    { ...fixture, expiresAt: now },
    { ...fixture, expiresAt: now + 481 },
    { ...fixture, inviteCode: 'query&other=value' },
    { ...fixture, publications: [fixture.publications[0], fixture.publications[0]] },
    { ...fixture, publications: [fixture.publications[0], { ...fixture.publications[1], sha256: 'invalid' }] },
  ]) {
    expect(() => parsePrivateStagingFixture(JSON.stringify(invalid), now)).toThrow(
      'Private staging requires a fresh synthetic invite and two verified publication records',
    );
  }
});

test('synthetic invite redemption requires a usable Library session', () => {
  const redeemed = {
    privateListening: true,
    destination: '/library',
    inviteFragmentCleared: true,
    libraryStatus: 200,
  };
  expect(() => assertPrivateInvite(redeemed)).not.toThrow();
  for (const rejected of [
    { ...redeemed, privateListening: false },
    { ...redeemed, destination: '/settings/billing' },
    { ...redeemed, inviteFragmentCleared: false },
    { ...redeemed, libraryStatus: 401 },
  ]) {
    expect(() => assertPrivateInvite(rejected)).toThrow('working session and a cleared invite fragment');
  }
});

test('synthetic publications bind two exact FLAC inputs to invitee Library IDs', () => {
  const manifest = JSON.parse(readFileSync(new URL('manifest.json', clips), 'utf8')) as {
    tracks: Array<{ file: string; bytes: number; sha256: string }>;
  };
  const digests = ['fixture-1.flac', 'fixture-2.flac'].map((file) => {
    const body = readFileSync(new URL(file, clips));
    const digest = createHash('sha256').update(body).digest('hex');
    expect(manifest.tracks.find((track) => track.file === file)).toEqual({
      file, bytes: body.length, sha256: digest,
    });
    return digest;
  });
  const published = digests.map((sha256, index) => ({ sha256, trackId: `fixture-track-${index}` }));
  const library = ['fixture-track-1', 'fixture-track-0'];
  expect(assertCuratedLibrary(digests, published, library)).toEqual(library);
  expect(() => assertCuratedLibrary(digests, published, [])).toThrow('render every published track');
  expect(() => assertCuratedLibrary(digests, published, [library[0]!, library[0]!])).toThrow('exactly once');
  expect(() => assertCuratedLibrary(digests, published.slice(0, 1), library)).toThrow('one publication');
  expect(() => assertCuratedLibrary(digests, [
    { sha256: '0'.repeat(64), trackId: 'fixture-track-0' }, published[1]!,
  ], library)).toThrow('uploaded FLAC digest');
  expect(() => assertCuratedLibrary([digests[0]!, digests[0]!], published, library)).toThrow('distinct FLAC');
});

test('synthetic transport observations require advancement, paused seek and queue Next', () => {
  const order = ['fixture-track-1', 'fixture-track-0'];
  // These observations model longer audio. The 93 ms upload clips above
  // cannot establish an informative mid-track seek in a browser.
  const playing = {
    sawPlaying: true, maxCurrentTime: 1, paused: false, ended: false,
    currentTime: 1, duration: 12, readyState: 4, errorCode: null, hasSource: true,
  };
  const observed = {
    firstTrackId: order[0]!,
    baselineTime: 0,
    playback: playing,
    seek: { currentTime: 6, duration: 12, paused: true, fraction: 0.55, sawSeekedEvent: true },
    resume: {
      minimumTime: 6,
      playback: { ...playing, currentTime: 7, maxCurrentTime: 7 },
    },
    queueOrder: order,
    nextTrackId: order[1]!,
  };
  expect(() => assertPrivateTransport(order, observed)).not.toThrow();
  expect(() => assertPrivateTransport(order, {
    ...observed, playback: { ...observed.playback, maxCurrentTime: 0 },
  })).toThrow('decode and advance');
  for (const seek of [
    { ...observed.seek, sawSeekedEvent: false },
    { ...observed.seek, paused: false },
    { ...observed.seek, currentTime: 0 },
    { ...observed.seek, duration: NaN },
  ]) {
    expect(() => assertPrivateTransport(order, { ...observed, seek })).toThrow('mid-track seek');
  }
  for (const resume of [
    { ...observed.resume, minimumTime: 0 },
    { ...observed.resume, minimumTime: Infinity },
    { ...observed.resume, playback: { ...playing, currentTime: 6, maxCurrentTime: 6 } },
  ]) {
    expect(() => assertPrivateTransport(order, { ...observed, resume })).toThrow('advance from the sought position');
  }
  expect(() => assertPrivateTransport(order, { ...observed, queueOrder: null })).toThrow('Queue must match');
  expect(() => assertPrivateTransport(order, {
    ...observed, queueOrder: [...order].reverse(),
  })).toThrow('Queue must match');
  expect(() => assertPrivateTransport(order, {
    ...observed, nextTrackId: observed.firstTrackId,
  })).toThrow('Play and Next');
});

test('synthetic uninvited and revoked requests require explicit refusal', () => {
  const statuses = {
    missingInviteSignup: 401,
    anonymousLibrary: 401,
    uninvitedLibrary: 403,
    uninvitedPlay: 403,
    revokedLibrary: 403,
    revokedPlay: 403,
    revokedHeartbeat: 403,
  };
  expect(() => assertPrivateRefusals(statuses, 0)).not.toThrow();
  for (const key of Object.keys(statuses)) {
    for (const status of [200, 404, 503]) {
      expect(() => assertPrivateRefusals({ ...statuses, [key]: status }, 0)).toThrow(key);
    }
  }
  expect(() => assertPrivateRefusals(statuses, 1)).toThrow('clear the curated rows');
});
