// Error-rate window verdicts over explicit v1 Tail observations. Pure,
// deterministic and side-effect-free: no imports, network, environment reads,
// logging, clocks, randomness, timers or storage. Input is an array of v1
// observations the caller has already windowed: v1 observations carry no
// timestamp, so window membership is the caller's job, not this function's.
// Each call evaluates exactly one window; no cross-window state is kept.
//
// Frozen policy (all six required, no coercion or defaults; extras ignored):
// {service: 'api', environment: 'staging',
//  min_invocations: integer 1..1000000,
//  max_error_rate: {failed, total} with 0 <= failed <= total, total 1..1000000,
//  per_error_class: {0..64 entries, keys match [a-z][a-z0-9_]{0,63},
//  per_release: {0..64 entries, keys are 40 lowercase hex or 'unknown'}}
// Only the explicit staging API labels are accepted. Invalid policy or a
// non-array observations root => null, no throw, never a fabricated verdict.
//
// Per entry: non-objects, schema_version !== 1, non-string service or
// environment, or failure_flags that is not an object with all four of
// http_5xx/exception/exceeded_cpu/exceeded_memory as booleans => malformed.
// String labels not exactly equal to the policy pair => excluded, never
// merged. Otherwise in scope: any flag true => failed (counted once);
// breakdown keys that are invalid fold to an ordinary 'unknown' bucket.
// This module is not wired or deployed; it performs no delivery.

const SCHEMA_VERSION = 1;
const MIN_INVOCATIONS_MIN = 1;
const MIN_INVOCATIONS_MAX = 1000000;
const RATIONAL_TOTAL_MAX = 1000000;
const MAX_RULES = 64;
const BREAKDOWN_CAP = 32;
const MAX_ROUTE_CHARS = 128;
const DECIMAL_SCALE = 10000;
const DECIMAL_PLACES = 4;

const EXPECTED_SERVICE = 'api';
const EXPECTED_ENVIRONMENT = 'staging';
const UNKNOWN = 'unknown';

const SHA40_RE = /^[0-9a-f]{40}$/;
const ERROR_CLASS_RE = /^[a-z][a-z0-9_]{0,63}$/;
const METHOD_RE = /^[A-Z]+$/;
const PATH_RE = /^[A-Za-z0-9\-._~!$&'()*+,;=:@%/{}]+$/;

function compareKeys(a, b) {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isValidRational(value) {
  try {
    if (!isRecord(value)) return false;
    const failed = value.failed;
    const total = value.total;
    if (typeof failed !== 'number' || !Number.isInteger(failed) || failed < 0) return false;
    if (typeof total !== 'number' || !Number.isInteger(total) || total < 1 || total > RATIONAL_TOTAL_MAX) {
      return false;
    }
    if (failed > total) return false;
    return true;
  } catch {
    return false;
  }
}

function isValidRuleMap(value, isValidKey) {
  try {
    if (!isRecord(value)) return false;
    const keys = Object.keys(value);
    if (keys.length > MAX_RULES) return false;
    for (let i = 0; i < keys.length; i += 1) {
      if (!isValidKey(keys[i])) return false;
      if (!isValidRational(value[keys[i]])) return false;
    }
    return true;
  } catch {
    return false;
  }
}

function isValidErrorClassKey(key) {
  return ERROR_CLASS_RE.test(key);
}

function isValidReleaseKey(key) {
  return key === UNKNOWN || SHA40_RE.test(key);
}

function isValidPolicy(policy) {
  try {
    if (!isRecord(policy)) return false;
    if (policy.service !== EXPECTED_SERVICE) return false;
    if (policy.environment !== EXPECTED_ENVIRONMENT) return false;
    const minInvocations = policy.min_invocations;
    if (
      typeof minInvocations !== 'number'
      || !Number.isInteger(minInvocations)
      || minInvocations < MIN_INVOCATIONS_MIN
      || minInvocations > MIN_INVOCATIONS_MAX
    ) {
      return false;
    }
    if (!isValidRational(policy.max_error_rate)) return false;
    if (!isValidRuleMap(policy.per_error_class, isValidErrorClassKey)) return false;
    if (!isValidRuleMap(policy.per_release, isValidReleaseKey)) return false;
    return true;
  } catch {
    return false;
  }
}

function isValidRoute(value) {
  if (typeof value !== 'string') return false;
  if (value === UNKNOWN) return true;
  if (value.length === 0 || value.length > MAX_ROUTE_CHARS) return false;
  for (let i = 0; i < value.length; i += 1) {
    const code = value.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f) return false;
  }
  const firstSpace = value.indexOf(' ');
  if (firstSpace <= 0) return false;
  if (value.indexOf(' ', firstSpace + 1) !== -1) return false;
  if (firstSpace === value.length - 1) return false;
  const method = value.slice(0, firstSpace);
  const path = value.slice(firstSpace + 1);
  if (!METHOD_RE.test(method)) return false;
  if (path[0] !== '/') return false;
  if (path.indexOf('?') !== -1 || path.indexOf('#') !== -1 || path.indexOf('\\') !== -1) return false;
  if (!PATH_RE.test(path)) return false;
  return true;
}

function breakdownKeyFrom(entry, key, isValid) {
  let value;
  try {
    value = entry[key];
  } catch {
    return UNKNOWN;
  }
  try {
    if (typeof value !== 'string') return UNKNOWN;
    return isValid(value) ? value : UNKNOWN;
  } catch {
    return UNKNOWN;
  }
}

function classifyEntry(entry, service, environment) {
  try {
    if (!isRecord(entry)) return { kind: 'malformed' };
    if (entry.schema_version !== SCHEMA_VERSION) return { kind: 'malformed' };
    const entryService = entry.service;
    const entryEnvironment = entry.environment;
    if (typeof entryService !== 'string' || typeof entryEnvironment !== 'string') {
      return { kind: 'malformed' };
    }
    const flags = entry.failure_flags;
    if (!isRecord(flags)) return { kind: 'malformed' };
    const http5xx = flags.http_5xx;
    const exception = flags.exception;
    const exceededCpu = flags.exceeded_cpu;
    const exceededMemory = flags.exceeded_memory;
    if (typeof http5xx !== 'boolean') return { kind: 'malformed' };
    if (typeof exception !== 'boolean') return { kind: 'malformed' };
    if (typeof exceededCpu !== 'boolean') return { kind: 'malformed' };
    if (typeof exceededMemory !== 'boolean') return { kind: 'malformed' };
    if (entryService !== service || entryEnvironment !== environment) {
      return { kind: 'excluded' };
    }
    return {
      kind: 'ok',
      failed: http5xx || exception || exceededCpu || exceededMemory,
      http5xx,
      exception,
      exceededCpu,
      exceededMemory,
      errorClass: breakdownKeyFrom(entry, 'error_class', isValidErrorClassKey),
      release: breakdownKeyFrom(entry, 'release', isValidReleaseKey),
      route: breakdownKeyFrom(entry, 'route', isValidRoute),
    };
  } catch {
    return { kind: 'malformed' };
  }
}

function formatDecimal(failed, total) {
  if (total <= 0) return '0.0000';
  // Half-up to 4 places with exact integer math:
  // floor(failed / total * 10000 + 0.5) = floor((failed * 20000 + total) / (2 * total)).
  const numerator = failed * 2 * DECIMAL_SCALE + total;
  const denominator = 2 * total;
  const scaled = (numerator - (numerator % denominator)) / denominator;
  const whole = (scaled - (scaled % DECIMAL_SCALE)) / DECIMAL_SCALE;
  const frac = scaled % DECIMAL_SCALE;
  return `${whole}.${(`000${frac}`).slice(-DECIMAL_PLACES)}`;
}

function breaches(observedFailed, observedTotal, threshold) {
  return observedFailed * threshold.total > threshold.failed * observedTotal;
}

function collectBreaches(buckets, rules, prefix, breachedRules) {
  const ruleKeys = Object.keys(rules);
  for (let i = 0; i < ruleKeys.length; i += 1) {
    const bucket = buckets.get(ruleKeys[i]);
    if (bucket === undefined || bucket.total < 1) continue;
    if (breaches(bucket.failed, bucket.total, rules[ruleKeys[i]])) {
      breachedRules.push(prefix + ruleKeys[i]);
    }
  }
}

function accumulate(buckets, key, failed) {
  let bucket = buckets.get(key);
  if (bucket === undefined) {
    bucket = { total: 0, failed: 0 };
    buckets.set(key, bucket);
  }
  bucket.total += 1;
  if (failed) bucket.failed += 1;
}

function buildBreakdown(buckets) {
  const keys = [...buckets.keys()].sort(compareKeys);
  const shown = keys.slice(0, BREAKDOWN_CAP);
  const folded = keys.slice(BREAKDOWN_CAP);
  const entries = shown.map((key) => {
    const bucket = buckets.get(key);
    return {
      key,
      total: bucket.total,
      failed: bucket.failed,
      decimal: formatDecimal(bucket.failed, bucket.total),
    };
  });
  let overflowTotal = 0;
  let overflowFailed = 0;
  for (let i = 0; i < folded.length; i += 1) {
    const bucket = buckets.get(folded[i]);
    overflowTotal += bucket.total;
    overflowFailed += bucket.failed;
  }
  return {
    buckets: entries,
    overflow: { total: overflowTotal, failed: overflowFailed, distinct_keys: folded.length },
  };
}

export function evaluateErrorWindow(observations, policy) {
  try {
    if (!isValidPolicy(policy)) return null;
    if (!Array.isArray(observations)) return null;
    const service = policy.service;
    const environment = policy.environment;
    const minInvocations = policy.min_invocations;
    const maxErrorRate = policy.max_error_rate;
    const perErrorClass = policy.per_error_class;
    const perRelease = policy.per_release;

    let total = 0;
    let failed = 0;
    let excluded = 0;
    let malformed = 0;
    let http5xxCount = 0;
    let exceptionCount = 0;
    let exceededCpuCount = 0;
    let exceededMemoryCount = 0;
    const byErrorClass = new Map();
    const byRelease = new Map();
    const byRoute = new Map();

    for (let i = 0; i < observations.length; i += 1) {
      const result = classifyEntry(observations[i], service, environment);
      if (result.kind === 'malformed') {
        malformed += 1;
        continue;
      }
      if (result.kind === 'excluded') {
        excluded += 1;
        continue;
      }
      total += 1;
      if (result.failed) failed += 1;
      if (result.http5xx) http5xxCount += 1;
      if (result.exception) exceptionCount += 1;
      if (result.exceededCpu) exceededCpuCount += 1;
      if (result.exceededMemory) exceededMemoryCount += 1;
      accumulate(byErrorClass, result.errorClass, result.failed);
      accumulate(byRelease, result.release, result.failed);
      accumulate(byRoute, result.route, result.failed);
    }

    const evaluated = total >= minInvocations;
    const breachedRules = [];
    let overallBreached = false;
    if (evaluated) {
      overallBreached = breaches(failed, total, maxErrorRate);
      if (overallBreached) breachedRules.push('overall');
      collectBreaches(byErrorClass, perErrorClass, 'error_class:', breachedRules);
      collectBreaches(byRelease, perRelease, 'release:', breachedRules);
      breachedRules.sort(compareKeys);
    }
    const inBreach = breachedRules.length > 0;

    return {
      schema_version: SCHEMA_VERSION,
      service,
      environment,
      verdict: !evaluated ? 'unknown' : (inBreach ? 'breach' : 'pass'),
      reason: !evaluated ? 'insufficient_data' : (inBreach ? 'breach' : 'ok'),
      counts: {
        total,
        failed,
        healthy: total - failed,
        excluded,
        malformed,
        by_flag: {
          http_5xx: http5xxCount,
          exception: exceptionCount,
          exceeded_cpu: exceededCpuCount,
          exceeded_memory: exceededMemoryCount,
        },
      },
      rate: { failed, total, decimal: formatDecimal(failed, total) },
      thresholds: {
        overall: { failed: maxErrorRate.failed, total: maxErrorRate.total, breached: overallBreached },
        breached_rules: breachedRules,
      },
      breakdowns: {
        by_error_class: buildBreakdown(byErrorClass),
        by_release: buildBreakdown(byRelease),
        by_route: buildBreakdown(byRoute),
      },
    };
  } catch {
    return null;
  }
}
