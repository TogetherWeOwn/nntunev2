import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { queueOrderFailure } from '../helpers/queue-order.js';

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
  test('ingest resolves and uploads the staged real NZB in the same account', () => {
    // The serial ingest wait must not inherit the old 300s test bound.
    const serialAt = journey.indexOf("test.describe.serial('player queue follows library-selection order'");
    const ingestAt = journey.indexOf("test('NZB fixture ingests and appears in the library'");
    expect(serialAt).toBeGreaterThan(-1);
    expect(ingestAt).toBeGreaterThan(serialAt);
    expect(journey.slice(serialAt, ingestAt)).toContain('test.describe.configure({ timeout: 600_000 });');
    // One upload of the staged secret-held real NZB — lazy, so a missing
    // staging fails the run red (no checked-in fixtures since TWO-1169).
    expect(journey).toContain('const stagedNzb = requireRealNzbFixture()');
    expect(journey).toContain('await fileInput.setInputFiles(stagedNzb)');
    expect(journey).not.toContain('resolveTwoTrackFixture');
    expect(journey).not.toContain('FIXTURES_DIR');
    expect(journey).toContain('sharedAccount = minted');
    expect(journey).toContain('const account = sharedAccount ??');
    expect(journey).toContain('const headers = accountHeaders(account)');
  });

  test('queue-order is active when configured and parked by name in real-NZB mode', () => {
    expect(journey).toContain("test('player queue advances in library-selection order'");
    expect(journey).not.toContain('test.fixme(');
    expect(journey).toMatch(/test\.skip\(\s*!process\.env\.STAGING_BASE_URL/);
    expect(journey).toContain("stagingBaseUrl('skip', 'queue-order')");
    // The real NZB stages a single library row, so the ≥2-track assertion is
    // parked until RAR-set playback + multi-track staging land (TWO-1174) —
    // the skip reason links the follow-up and the gate allow-lists the title.
    expect(journey).toContain('REAL_NZB_RAR_SKIP');
    expect(journey).toContain('RAR_PLAYBACK_FOLLOW_UP');
  });

  test('distinct-track precondition fails by name before browser queue waits', () => {
    expect(journey).toContain('apiIds.length');
    expect(journey).toContain('[queue-order] library holds fewer than 2 tracks');
    expect(journey.indexOf('const rows = page.getByTestId(TESTIDS.libraryItem)')).toBeGreaterThan(
      journey.indexOf('[queue-order] library holds fewer than 2 tracks'),
    );
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
    const failure = queueOrderFailure(['track-a', 'track-b', 'track-c'], ['track-a', 'track-b', 'track-d']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/track-d/);
  });

  test('duplicate id in the queue fails', () => {
    const failure = queueOrderFailure(['track-a', 'track-b'], ['track-a', 'track-a']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/duplicate|track-a/i);
  });
});
