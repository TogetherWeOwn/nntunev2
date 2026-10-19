import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { queueOrderFailure } from '../helpers/queue-order.js';
import { twoTrackPreconditionFailure } from '../helpers/two-track-fixture.js';

// Offline acceptance for the queue-order verdict used by the live
// "player queue advances in library-selection order" spec in
// player-queue-order.spec.ts. No staging, no credentials, no skip guard:
// these cases pin that a missing queue, a short library, or any divergence
// between library-selection order and queue order can never be reported as
// matching, while a genuine multi-track match passes.
//
// Library ids here are synthetic fixture ids (never credentials), so failure
// messages may name them — unlike URLs, tokens and passwords, which must
// stay out of public CI artifacts.

// Source pins keep the live leg connected to the offline fixture/verdict
// contracts. They prove wiring, not staging acceptance.
const journey = readFileSync(new URL('./player-queue-order.spec.ts', import.meta.url), 'utf8');

test.describe('queue-order journey wiring (offline)', () => {
  test('ingest resolves and uploads both single-file fixtures in the same account', () => {
    expect(journey).toContain('singleTrack: path.resolve(FIXTURES_DIR, SINGLE_TRACK_FIXTURE_FILE)');
    expect(journey).toContain('secondSingle: path.resolve(FIXTURES_DIR, SECOND_SINGLE_TRACK_FIXTURE_FILE)');
    expect(journey).toMatch(/resolveTwoTrackFixture\([\s\S]*?existsSync/);
    expect(journey).toMatch(/for \(const nzb of staging\.staged\) \{[\s\S]*?await fileInput\.setInputFiles\(nzb\)/);
    expect(journey).toContain('sharedAccount = minted');
    expect(journey).toContain('const account = sharedAccount ??');
    expect(journey).toContain('const headers = accountHeaders(account)');
  });

  test('queue-order is active when configured and skips only pre-deploy', () => {
    expect(journey).toContain("test('player queue advances in library-selection order'");
    expect(journey).not.toContain('test.fixme(');
    expect(journey).toMatch(/test\.skip\(\s*!process\.env\.STAGING_BASE_URL/);
    expect(journey).toContain("stagingBaseUrl('skip', 'queue-order')");
  });

  test('distinct-track precondition fails by name before browser queue waits', () => {
    const preconditionAt = journey.indexOf('twoTrackPreconditionFailure(apiIds)');
    expect(preconditionAt).toBeGreaterThan(-1);
    expect(journey.indexOf('const rows = page.getByTestId(TESTIDS.libraryItem)')).toBeGreaterThan(preconditionAt);
    expect(journey).toContain("expect(precondition, `[queue-order] ${precondition ?? 'two-track precondition met'}`).toBeNull()");
    expect(twoTrackPreconditionFailure(['only-track'])).toMatch(/only 1 track.*second synthetic track is missing/);
    expect(twoTrackPreconditionFailure(['only-track', 'only-track'])).not.toBeNull();
    expect(twoTrackPreconditionFailure(['track-a', 'track-b'])).toBeNull();
  });
});

test.describe('queueOrderFailure (queue vs library-selection order, offline fixtures)', () => {
  test('matching multi-track order passes', () => {
    expect(
      queueOrderFailure(['track-a', 'track-b', 'track-c'], ['track-a', 'track-b', 'track-c']),
    ).toBeNull();
  });

  test('matching two-track order passes (minimum multi-track case)', () => {
    expect(queueOrderFailure(['track-a', 'track-b'], ['track-a', 'track-b'])).toBeNull();
  });

  test('empty library fails naming the ingest precondition', () => {
    const failure = queueOrderFailure([], []);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/ingest|library is empty/i);
  });

  test('single-track library fails (queue order needs multiple tracks)', () => {
    const failure = queueOrderFailure(['only-track'], ['only-track']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/≥2|multiple|only 1/i);
  });

  test('absent queue fails naming the expected queue hooks', () => {
    const failure = queueOrderFailure(['track-a', 'track-b'], null);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/queue-item|queue-list|now-playing|player-next/);
  });

  test('empty queue fails instead of passing vacuously', () => {
    const failure = queueOrderFailure(['track-a', 'track-b'], []);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/empty|length|2.*0|0.*2/);
  });

  test('reversed order fails naming the first divergence', () => {
    const failure = queueOrderFailure(['track-a', 'track-b'], ['track-b', 'track-a']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/track-a|track-b/);
    expect(failure).toMatch(/0/);
  });

  test('rotated order fails', () => {
    expect(
      queueOrderFailure(['track-a', 'track-b', 'track-c'], ['track-b', 'track-c', 'track-a']),
    ).toMatch(/track-a/);
  });

  test('shorter queue fails naming the length mismatch', () => {
    const failure = queueOrderFailure(['track-a', 'track-b', 'track-c'], ['track-a', 'track-b']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/3.*2|2.*3|length/i);
  });

  test('longer queue fails naming the extra id', () => {
    const failure = queueOrderFailure(['track-a', 'track-b'], ['track-a', 'track-b', 'track-c']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/track-c/);
  });

  test('duplicate id in the queue fails', () => {
    const failure = queueOrderFailure(['track-a', 'track-b'], ['track-a', 'track-a']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/duplicate|track-a/i);
  });
});
