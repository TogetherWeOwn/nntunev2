// Sustained health-red window verdicts over explicit health samples. Pure,
// deterministic and side-effect-free: no imports, network, environment reads,
// logging, clocks, randomness, timers or storage. Input is an array of
// {t, ok} health samples the caller has already windowed: each sample
// carries its own caller-supplied ms clock, so window membership is the
// caller's job, not this function's. Each call evaluates exactly one
// window; no cross-window state is kept.
//
// Frozen policy (all four required, no coercion or defaults; extras ignored):
// {service: 'api', environment: 'staging',
//  red_for_ms: integer 1..3600000,
//  min_samples: integer 1..100000}
// Only the explicit staging API labels are accepted. Invalid policy or a
// non-array samples root => null, no throw, never a fabricated verdict.
//
// Per entry: non-objects, arrays, records whose t is not a non-negative
// integer ms timestamp, or whose ok is not a boolean => malformed: skipped,
// counted, and splitting contiguity, so no breach may span untrusted
// evidence. Valid samples are ordered by t ascending (stable) within each
// malformed-delimited segment before runs are scanned. A contiguous not-ok
// run breaches iff (last_t - first_t) >= red_for_ms; a single red sample
// spans 0 and never breaches. Below the min_samples floor => unknown, never
// a breach, with the longest red span still reported.
// This module is not wired or deployed; it performs no delivery.

const SCHEMA_VERSION = 1;
const EXPECTED_SERVICE = 'api';
const EXPECTED_ENVIRONMENT = 'staging';
const RED_FOR_MS_MIN = 1;
const RED_FOR_MS_MAX = 3600000;
const MIN_SAMPLES_MIN = 1;
const MIN_SAMPLES_MAX = 100000;

function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

// Each policy field is read exactly once and the captured values are both
// validated and used, so a nondeterministic getter cannot pass validation
// with one value and have another value echoed into the verdict.
function readPolicy(policy) {
  try {
    if (!isRecord(policy)) return null;
    const service = policy.service;
    const environment = policy.environment;
    const redForMs = policy.red_for_ms;
    const minSamples = policy.min_samples;
    if (service !== EXPECTED_SERVICE) return null;
    if (environment !== EXPECTED_ENVIRONMENT) return null;
    if (
      typeof redForMs !== 'number'
      || !Number.isInteger(redForMs)
      || redForMs < RED_FOR_MS_MIN
      || redForMs > RED_FOR_MS_MAX
    ) {
      return null;
    }
    if (
      typeof minSamples !== 'number'
      || !Number.isInteger(minSamples)
      || minSamples < MIN_SAMPLES_MIN
      || minSamples > MIN_SAMPLES_MAX
    ) {
      return null;
    }
    return {
      service, environment, redForMs, minSamples,
    };
  } catch {
    return null;
  }
}

function classifySample(entry) {
  try {
    if (!isRecord(entry)) return null;
    const t = entry.t;
    const ok = entry.ok;
    if (typeof t !== 'number' || !Number.isInteger(t) || t < 0) return null;
    if (typeof ok !== 'boolean') return null;
    return { t, ok };
  } catch {
    return null;
  }
}

function compareSamples(a, b) {
  if (a.t < b.t) return -1;
  if (a.t > b.t) return 1;
  if (a.index < b.index) return -1;
  if (a.index > b.index) return 1;
  return 0;
}

export function evaluateHealthWindow(samples, policy) {
  try {
    const validated = readPolicy(policy);
    if (validated === null) return null;
    if (!Array.isArray(samples)) return null;
    const service = validated.service;
    const environment = validated.environment;
    const redForMs = validated.redForMs;
    const minSamples = validated.minSamples;

    let valid = 0;
    let red = 0;
    let malformed = 0;
    const segments = [];
    let current = [];
    for (let i = 0; i < samples.length; i += 1) {
      const sample = classifySample(samples[i]);
      if (sample === null) {
        malformed += 1;
        if (current.length > 0) {
          segments.push(current);
          current = [];
        }
        continue;
      }
      valid += 1;
      if (!sample.ok) red += 1;
      current.push({ t: sample.t, ok: sample.ok, index: i });
    }
    if (current.length > 0) {
      segments.push(current);
    }

    let longest = 0;
    for (let s = 0; s < segments.length; s += 1) {
      const segment = segments[s];
      segment.sort(compareSamples);
      let inRun = false;
      let runFirst = 0;
      for (let i = 0; i < segment.length; i += 1) {
        if (segment[i].ok) {
          inRun = false;
          continue;
        }
        if (!inRun) {
          inRun = true;
          runFirst = segment[i].t;
          continue;
        }
        const span = segment[i].t - runFirst;
        if (span > longest) longest = span;
      }
    }

    const evaluated = valid >= minSamples;
    const inBreach = evaluated && longest >= redForMs;
    return {
      schema_version: SCHEMA_VERSION,
      service,
      environment,
      verdict: !evaluated ? 'unknown' : (inBreach ? 'breach' : 'ok'),
      reason: !evaluated ? 'insufficient_data' : (inBreach ? 'breach' : 'healthy'),
      counts: { valid, red, malformed },
      longest_red_span_ms: longest,
      red_for_ms: redForMs,
    };
  } catch {
    return null;
  }
}
