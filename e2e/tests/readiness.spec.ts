import { test, expect } from '@playwright/test';
import { assertPromotionReady } from '../helpers/readiness.js';

// Acceptance (offline, no staging, no credentials): a promotion-readiness
// check must reject false-green /api/ready responses. The readiness block in
// tests/health.spec.ts calls this same helper, so these controlled fixtures
// exercise the checker the live smoke uses — not a copy of it.
//
// Contract the builder implements in e2e/helpers/readiness.ts:
//   export function assertPromotionReady(
//     probe: { status: number; headers: Record<string, string>; bodyText: string; url: string },
//     context: string,
//   ): void
// (the probe shape matches WallProbe in helpers/access.ts, so callers can
// pass the object they already build for assertNoAccessWall).
//
// Pass (return without throwing) ONLY when all of these hold:
//   - status is exactly 200, and
//   - bodyText parses as JSON, and
//   - the parsed value is an object whose `ok` is strictly boolean true.
// Every other response throws an Error whose message:
//   - contains `[${context}]` and the phrase `promotion-readiness`, and
//   - for a non-200 response, contains the HTTP status code.
// In particular: 404 is a failure (never "not yet implemented"), and 503 is
// a failure even when a Retry-After header is present — promotion is about
// being ready now, not about a well-formed not-ready answer. The helper is
// pure: no network, no environment reads, and it never skips.
//
// This file intentionally has NO STAGING_BASE_URL skip guard: these cases
// must run (and be able to fail) pre-deploy. A skipped live smoke is not a
// pass, and a skipped unit acceptance would prove nothing either.

const READY_URL = 'https://staging.example/api/ready';

function probe(overrides: {
  status: number;
  bodyText: string;
  headers?: Record<string, string>;
  url?: string;
}): { status: number; headers: Record<string, string>; bodyText: string; url: string } {
  return {
    status: overrides.status,
    headers: overrides.headers ?? { 'content-type': 'application/json' },
    bodyText: overrides.bodyText,
    url: overrides.url ?? READY_URL,
  };
}

function failureMessage(p: ReturnType<typeof probe>): string {
  try {
    assertPromotionReady(p, 'ready');
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error('expected assertPromotionReady to throw');
}

test.describe('assertPromotionReady (promotion readiness, offline fixtures)', () => {
  test('valid 200 JSON {ok:true} succeeds', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 200, bodyText: '{"ok":true}' }), 'ready'),
    ).not.toThrow();
  });

  test('404 fails promotion-readiness (missing route is not ready)', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 404, bodyText: '{"error":"not found"}' }), 'ready'),
    ).toThrow(/promotion-readiness/);
    expect(() =>
      assertPromotionReady(probe({ status: 404, bodyText: '{"error":"not found"}' }), 'ready'),
    ).toThrow(/404/);
  });

  test('500 fails promotion-readiness even when the body claims ok:true', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 500, bodyText: '{"ok":true}' }), 'ready'),
    ).toThrow(/promotion-readiness/);
    expect(() =>
      assertPromotionReady(probe({ status: 500, bodyText: '{"ok":true}' }), 'ready'),
    ).toThrow(/500/);
  });

  test('503 fails promotion-readiness even with Retry-After and an ok:true body', () => {
    expect(() =>
      assertPromotionReady(
        probe({
          status: 503,
          bodyText: '{"ok":true}',
          headers: { 'content-type': 'application/json', 'retry-after': '5' },
        }),
        'ready',
      ),
    ).toThrow(/promotion-readiness/);
    expect(() =>
      assertPromotionReady(
        probe({
          status: 503,
          bodyText: '{"ok":true}',
          headers: { 'content-type': 'application/json', 'retry-after': '5' },
        }),
        'ready',
      ),
    ).toThrow(/503/);
  });

  test('200 HTML fails promotion-readiness (app shell / error page is not readiness JSON)', () => {
    expect(() =>
      assertPromotionReady(
        probe({
          status: 200,
          bodyText: '<!doctype html><html><head><title>NNTune</title></head><body>shell</body></html>',
          headers: { 'content-type': 'text/html; charset=utf-8' },
        }),
        'ready',
      ),
    ).toThrow(/promotion-readiness/);
  });

  test('200 malformed JSON fails promotion-readiness', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 200, bodyText: '{"ok":true' }), 'ready'),
    ).toThrow(/promotion-readiness/);
  });

  test('200 {ok:false} fails promotion-readiness', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 200, bodyText: '{"ok":false}' }), 'ready'),
    ).toThrow(/promotion-readiness/);
  });

  test('200 JSON without an ok field fails promotion-readiness', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 200, bodyText: '{}' }), 'ready'),
    ).toThrow(/promotion-readiness/);
  });

  test('200 {ok:"true"} fails promotion-readiness (ok must be strictly boolean true)', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 200, bodyText: '{"ok":"true"}' }), 'ready'),
    ).toThrow(/promotion-readiness/);
  });

  test('failure messages name the caller context', () => {
    expect(() =>
      assertPromotionReady(probe({ status: 404, bodyText: '{"error":"not found"}' }), 'ready'),
    ).toThrow(/\[ready\]/);
  });

  test('failure messages never echo the probe URL (host or path)', () => {
    const sentinelUrl = 'https://sentinel-staging-host.example/api/ready';
    const cases = [
      probe({ status: 404, bodyText: '{"error":"not found"}', url: sentinelUrl }),
      probe({ status: 503, bodyText: '{"ok":false}', url: sentinelUrl }),
      probe({
        status: 200,
        bodyText: '<!doctype html><html><body>shell</body></html>',
        headers: { 'content-type': 'text/html; charset=utf-8' },
        url: sentinelUrl,
      }),
    ];
    for (const p of cases) {
      const message = failureMessage(p);
      expect(message).toContain('[ready]');
      expect(message).toContain('promotion-readiness');
      expect(message).toContain(`HTTP ${p.status}`);
      expect(message).not.toContain('sentinel-staging-host.example');
      expect(message).not.toContain('/api/ready');
      expect(message).not.toContain(sentinelUrl);
    }
  });
});
