import { test, expect } from '@playwright/test';
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
