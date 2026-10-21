import { test, expect } from '@playwright/test';
import { Buffer } from 'node:buffer';
import { API } from '../helpers/contract.js';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import { apiGet } from '../helpers/accounts.js';
import {
  DEMO_MEDIA_BYTES,
  demoTrackSalt,
  expectedSyntheticBody,
  firstByteDifference,
} from '../helpers/demo-audio.js';

// Staging proves it serves the synthetic contract — and never mis-routed R2
// or library bytes — at the no-auth demo-audio route. The route needs no
// account (no signup seam, no session bearer): Access pair only, so these
// checks are parallel-safe and independent of the session-gating slice.
//
// NO file-level skip here by design: the offline describe below pins the
// vendored formula on every PR (via `npm run test:unit`) with no staging,
// while each live test skips green when STAGING_BASE_URL is unset and fails
// closed once it is configured.

test.describe('vendored synthetic contract (offline, no staging)', () => {
  test('default body is a 256 KiB deterministic WAV', () => {
    const body = expectedSyntheticBody(null);
    expect(body.length).toBe(DEMO_MEDIA_BYTES);
    expect(body.toString('ascii', 0, 4)).toBe('RIFF');
    expect(body.toString('ascii', 8, 12)).toBe('WAVE');
    expect(body.toString('ascii', 12, 16)).toBe('fmt ');
    expect(body.toString('ascii', 36, 40)).toBe('data');
  });

  test('sample formula pins its constants, not just self-consistency', () => {
    const body = expectedSyntheticBody(null);
    // sample 0: (0 * 7919 + 13) % 65536 - 32768; sample 1 adds one 7919 step.
    expect(body.readInt16LE(44)).toBe(-32755);
    expect(body.readInt16LE(46)).toBe(-24836);
    // The default call and an explicit unknown track are the same stream.
    expect(expectedSyntheticBody('no-such-track').equals(body)).toBe(true);
  });

  test('salted tracks diverge at the first sample; unknown tracks do not', () => {
    const plain = expectedSyntheticBody(null);
    const salted = expectedSyntheticBody('second-signal');
    expect(salted.length).toBe(DEMO_MEDIA_BYTES);
    expect(salted.readInt16LE(44)).toBe(-32714);
    expect(firstByteDifference(salted, plain)).toBe(44);
    expect(firstByteDifference(expectedSyntheticBody('no-such-track'), plain)).toBeNull();
  });

  test('track salts match the app allowlist; prototype names stay default', () => {
    expect(demoTrackSalt(null)).toBe(0);
    expect(demoTrackSalt('nope')).toBe(0);
    expect(demoTrackSalt('constructor')).toBe(0);
    expect(demoTrackSalt('__proto__')).toBe(0);
    expect(demoTrackSalt('second-signal')).toBe(41);
    // Every vendored salt is pinned, or a typo in the helper passes silently:
    // sample 0 with salt 97 is (0 * 7919 + 13 + 97) % 65536 - 32768.
    expect(demoTrackSalt('third-light')).toBe(97);
    expect(expectedSyntheticBody('third-light').readInt16LE(44)).toBe(-32658);
  });
});

test.describe('staging demo audio matches the synthetic contract', () => {
  test('default track serves the full vendored body as audio/wav', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'demo-audio');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const url = `${baseURL}${API.demoAudio}`;
    // No-auth route: Access pair only, never an account session.
    const res = await apiGet(request, url, accessHeaders(), 'demo-audio');
    const headers = res.headers();
    // Binary success bodies are probed headers-only (no megabyte buffering);
    // a 200 audio/wav full body from the app is self-evidently not a wall.
    assertNoAccessWall({ status: res.status(), headers, bodyText: '', url }, 'demo-audio');
    if (res.status() !== 200) {
      const bodyText = await res.text().catch(() => '');
      expect(res.status(), `[demo-audio] expected HTTP 200 for the default track, body: ${bodyText.slice(0, 500)}`).toBe(
        200,
      );
    }
    const contentType = headers['content-type'] ?? '';
    expect(contentType, '[demo-audio] default track must serve content-type audio/wav').toBe(
      'audio/wav',
    );
    const body = Buffer.from(await res.body());
    expect(
      body.length,
      `[demo-audio] default track must serve the full ${DEMO_MEDIA_BYTES}-byte body, got ${body.length}`,
    ).toBe(DEMO_MEDIA_BYTES);
    const expected = expectedSyntheticBody(null);
    const diff = firstByteDifference(body, expected);
    expect(
      diff,
      `[demo-audio] default track diverges from the vendored synthetic formula at byte ${diff} ` +
        `(expected ${expected[diff ?? 0]}, got ${body[diff ?? 0]}) — staging is not serving syntheticBody`,
    ).toBeNull();
  });

  test('?track=second-signal is salted; unknown ?track= serves the default stream', async ({
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'demo-audio-tracks');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = accessHeaders();
    const fetchTrack = async (query: string, context: string): Promise<Buffer> => {
      const url = `${baseURL}${API.demoAudio}${query}`;
      const res = await apiGet(request, url, headers, context);
      const resHeaders = res.headers();
      assertNoAccessWall(
        { status: res.status(), headers: resHeaders, bodyText: '', url },
        context,
      );
      if (res.status() !== 200) {
        const bodyText = await res.text().catch(() => '');
        expect(
          res.status(),
          `[${context}] expected HTTP 200, body: ${bodyText.slice(0, 500)}`,
        ).toBe(200);
      }
      return Buffer.from(await res.body());
    };

    const plain = await fetchTrack('', 'demo-audio-tracks');
    const salted = await fetchTrack('?track=second-signal', 'demo-audio-tracks');
    const unknown = await fetchTrack('?track=no-such-track', 'demo-audio-tracks');

    // Divergence alone would pass a wrong salt: require the exact salted body.
    const expectedSalted = expectedSyntheticBody('second-signal');
    const saltContractDiff = firstByteDifference(salted, expectedSalted);
    expect(
      saltContractDiff,
      `[demo-audio-tracks] ?track=second-signal diverges from the vendored salted formula at byte ${saltContractDiff} ` +
        `(expected ${expectedSalted[saltContractDiff ?? 0]}, got ${salted[saltContractDiff ?? 0]}) — wrong salt or mis-routed bytes`,
    ).toBeNull();
    const unknownDiff = firstByteDifference(unknown, plain);
    expect(
      unknownDiff,
      `[demo-audio-tracks] unknown ?track= diverged from the default stream at byte ${unknownDiff} — it must serve the default stream`,
    ).toBeNull();
  });
});
