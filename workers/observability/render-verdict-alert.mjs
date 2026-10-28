// Verdict-to-alert payload renderer. Pure, deterministic and side-effect-free:
// no imports, network, environment reads, logging, clocks, randomness,
// timers or storage. Renders one breach verdict from evaluateErrorWindow
// plus caller context into an alert payload object ({content, embeds})
// shaped for a Discord webhook body; every other input fails closed to
// null — never a fabricated alert, never a throw.
//
// Validation (all required, no coercion or defaults; extras ignored):
// verdict: {schema_version: 1, service: 'api', environment: 'staging',
//   verdict: 'breach', reason: 'breach',
//   counts: {total: integer >= 1, failed: integer 0..total},
//   rate: {failed, total} with the same rational rules plus a decimal
//     string matching /^\d+\.\d{4}$/,
//   thresholds: {breached_rules: [1..N non-empty strings]},
//   breakdowns: {by_error_class/by_release/by_route:
//     {buckets: [{key: string}, ...]}}}
// Only the exact pair verdict 'breach' + reason 'breach' renders.
// counts and rate must describe the same value: rate.failed/total must
// equal counts.failed/total and the decimal must equal the producer's
// half-up formatDecimal(failed, total), otherwise the verdict is
// contradictory and fails closed. Breakdown buckets keep only their key
// for rendering but rank by failures (valid non-negative integer failed
// counts sort first, ties break by key), so the capped "Top" fields show
// the highest-failure entries instead of alphabetical prefixes.
// context: {logsUrl, windowLabel}, both non-empty strings; logsUrl must
// match /^https?:\/\/[^\/\s]\S*$/ (http/https, no whitespace) and parse
// via WHATWG URL with an http/https protocol and a non-empty hostname;
// windowLabel must contain a non-space character.
//
// Bounds: content <= 2000 chars, exactly one embed, payload total
// <= 6000 chars, <= 25 fields per embed, field names 1..256 chars,
// field values 1..1024 chars. Present-but-oversized values are tolerated
// and bounded, never rejected: oversized strings truncate to a prefix
// plus "…" (U+2026), dropping a trailing high surrogate so truncation
// never emits a lone surrogate, and oversized lists cap to a leading
// prefix with a "+N more" marker; identical input always yields
// byte-identical output.
// The source-only runtime imports this pure helper; it carries no webhook
// URL or secret and performs no delivery or deployment itself.

const SCHEMA_VERSION = 1;
const EXPECTED_SERVICE = 'api';
const EXPECTED_ENVIRONMENT = 'staging';
const BREACH = 'breach';

const DECIMAL_RE = /^\d+\.\d{4}$/;
const LOGS_URL_RE = /^https?:\/\/[^/\s]\S*$/;
const NON_BLANK_RE = /\S/;
const DECIMAL_SCALE = 10000;
const DECIMAL_PLACES = 4;
// Largest total for which failed * 20000 + total stays an exact integer
// (20001 * MAX_EXACT_TOTAL < 2^53). The producer caps totals at 1e6, so
// anything above this bound cannot be producer output and fails closed
// instead of risking an inexact decimal check.
const MAX_EXACT_TOTAL = 450000000000;

const CONTENT_MAX = 2000;
const PAYLOAD_MAX = 6000;
const TITLE_MAX = 256;
const FIELD_NAME_MAX = 256;
const FIELD_VALUE_MAX = 1024;

const FULL_CAPS = {
  rules: 10,
  buckets: 5,
  label: 1024,
  logs: 1024,
  ruleLength: 200,
  keyLength: 200,
  decimal: 64,
};
const CAP_SCHEDULE = [
  { buckets: 3 },
  { rules: 5, buckets: 2 },
  { rules: 3, buckets: 1, label: 512, logs: 512 },
  { rules: 2, label: 256, logs: 256, ruleLength: 100, keyLength: 100 },
  { rules: 1, label: 128, ruleLength: 64, keyLength: 64 },
  { buckets: 0, label: 64 },
];
const MINIMAL_CAPS = {
  rules: 1,
  buckets: 0,
  label: 64,
  logs: 256,
  ruleLength: 64,
  keyLength: 64,
  decimal: 16,
};

const DIMENSIONS = ['by_error_class', 'by_release', 'by_route'];
const DIMENSION_TITLES = ['Top error classes', 'Top releases', 'Top routes'];

function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function extractRational(value) {
  try {
    if (!isRecord(value)) return null;
    const failed = value.failed;
    const total = value.total;
    if (typeof failed !== 'number' || !Number.isInteger(failed) || failed < 0) return null;
    if (typeof total !== 'number' || !Number.isInteger(total) || total < 1) return null;
    if (failed > total) return null;
    return { failed, total };
  } catch {
    return null;
  }
}

function extractRules(thresholds) {
  try {
    if (!isRecord(thresholds)) return null;
    const rules = thresholds.breached_rules;
    if (!Array.isArray(rules)) return null;
    if (rules.length < 1) return null;
    const out = [];
    for (let i = 0; i < rules.length; i += 1) {
      const rule = rules[i];
      if (typeof rule !== 'string' || rule.length < 1) return null;
      out.push(rule);
    }
    return out;
  } catch {
    return null;
  }
}

function extractBucketKeys(dimension) {
  try {
    if (!isRecord(dimension)) return null;
    const buckets = dimension.buckets;
    if (!Array.isArray(buckets)) return null;
    const entries = [];
    for (let i = 0; i < buckets.length; i += 1) {
      const bucket = buckets[i];
      if (!isRecord(bucket)) return null;
      const key = bucket.key;
      if (typeof key !== 'string') return null;
      // Ranking signal only, never a rejection: buckets without a usable
      // failed count sort as zero. Reads stay inside this try so hostile
      // getters fail closed with the verdict instead of throwing.
      const failed = bucket.failed;
      const score = typeof failed === 'number' && Number.isInteger(failed) && failed >= 0 ? failed : 0;
      entries.push({ key, score });
    }
    // Highest failures first, ties by key: the producer sorts buckets
    // alphabetically, so without ranking the capped "Top" fields would show
    // alphabetical prefixes and could omit the breaching entry entirely.
    entries.sort((a, b) => {
      if (a.score !== b.score) return a.score > b.score ? -1 : 1;
      if (a.key === b.key) return 0;
      return a.key < b.key ? -1 : 1;
    });
    return entries.map((entry) => entry.key);
  } catch {
    return null;
  }
}

function formatRateDecimal(failed, total) {
  // The producer's half-up to 4 places with exact integer math; see
  // evaluate-error-window.mjs. Callers must keep total within
  // MAX_EXACT_TOTAL so every intermediate value stays exact.
  const numerator = failed * 2 * DECIMAL_SCALE + total;
  const denominator = 2 * total;
  const scaled = (numerator - (numerator % denominator)) / denominator;
  const whole = (scaled - (scaled % DECIMAL_SCALE)) / DECIMAL_SCALE;
  const frac = scaled % DECIMAL_SCALE;
  return `${whole}.${(`000${frac}`).slice(-DECIMAL_PLACES)}`;
}

function extractVerdict(verdict) {
  try {
    if (!isRecord(verdict)) return null;
    if (verdict.schema_version !== SCHEMA_VERSION) return null;
    if (verdict.service !== EXPECTED_SERVICE) return null;
    if (verdict.environment !== EXPECTED_ENVIRONMENT) return null;
    if (verdict.verdict !== BREACH) return null;
    if (verdict.reason !== BREACH) return null;
    const counts = extractRational(verdict.counts);
    if (counts === null) return null;
    const rate = verdict.rate;
    const rateRational = extractRational(rate);
    if (rateRational === null) return null;
    if (rateRational.failed !== counts.failed || rateRational.total !== counts.total) return null;
    const decimal = rate.decimal;
    if (typeof decimal !== 'string' || !DECIMAL_RE.test(decimal)) return null;
    if (counts.total > MAX_EXACT_TOTAL) return null;
    if (decimal !== formatRateDecimal(counts.failed, counts.total)) return null;
    const rules = extractRules(verdict.thresholds);
    if (rules === null) return null;
    const breakdowns = verdict.breakdowns;
    if (!isRecord(breakdowns)) return null;
    const bucketsByDim = [];
    for (let i = 0; i < DIMENSIONS.length; i += 1) {
      const keys = extractBucketKeys(breakdowns[DIMENSIONS[i]]);
      if (keys === null) return null;
      bucketsByDim.push(keys);
    }
    return {
      failed: counts.failed,
      total: counts.total,
      decimal,
      rules,
      bucketsByDim,
    };
  } catch {
    return null;
  }
}

function isValidLogsUrl(logsUrl) {
  // Shape first (http/https with no whitespace), then WHATWG parsing: the
  // regex alone admits host-less URLs such as "https://?x=1" that would
  // render a dead logs link instead of failing closed. Pure string work,
  // no network; the original string is rendered verbatim on success.
  if (typeof logsUrl !== 'string' || !LOGS_URL_RE.test(logsUrl)) return false;
  try {
    const parsed = new URL(logsUrl);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return false;
    if (parsed.hostname === '') return false;
    return true;
  } catch {
    return false;
  }
}

function extractContext(context) {
  try {
    if (!isRecord(context)) return null;
    const logsUrl = context.logsUrl;
    const windowLabel = context.windowLabel;
    if (!isValidLogsUrl(logsUrl)) return null;
    if (typeof windowLabel !== 'string' || windowLabel.length < 1 || !NON_BLANK_RE.test(windowLabel)) {
      return null;
    }
    return { logsUrl, windowLabel };
  } catch {
    return null;
  }
}

function truncateTo(text, max) {
  if (text.length <= max) return text;
  let end = max - 1;
  // Never split a surrogate pair: a high surrogate left at the cut would
  // become a lone surrogate a strict receiver can reject, silently
  // dropping the breach alert.
  if (end > 0) {
    const code = text.charCodeAt(end - 1);
    if (code >= 0xD800 && code <= 0xDBFF) end -= 1;
  }
  return text.slice(0, end) + '…';
}

function renderList(items, shown, itemCap, valueCap) {
  const head = items.slice(0, shown).map((item) => truncateTo(item, itemCap));
  if (items.length > shown) head.push(`+${items.length - shown} more`);
  const joined = head.join('\n');
  return truncateTo(joined.length === 0 ? '(none)' : joined, valueCap);
}

function field(name, value) {
  return {
    name: truncateTo(name, FIELD_NAME_MAX),
    value: truncateTo(value, FIELD_VALUE_MAX),
  };
}

function buildPayload(data, contextData, caps) {
  const rational = `${data.failed}/${data.total}`;
  const decimal = truncateTo(data.decimal, caps.decimal);
  const content = truncateTo(
    `Staging API error-rate BREACH: ${rational} failed (${decimal})`,
    CONTENT_MAX,
  );
  const fields = [
    field('Status', 'breach'),
    field('Window', truncateTo(contextData.windowLabel, caps.label)),
    field('Error rate', `${rational} failed (${decimal})`),
    field('Breached rules', renderList(data.rules, caps.rules, caps.ruleLength, FIELD_VALUE_MAX)),
    field('Logs', truncateTo(contextData.logsUrl, caps.logs)),
  ];
  for (let i = 0; i < DIMENSIONS.length; i += 1) {
    fields.push(field(
      DIMENSION_TITLES[i],
      renderList(data.bucketsByDim[i], caps.buckets, caps.keyLength, FIELD_VALUE_MAX),
    ));
  }
  return {
    content,
    embeds: [{ title: truncateTo('Staging API error-rate alert', TITLE_MAX), fields }],
  };
}

export function renderVerdictAlert(verdict, context) {
  try {
    const data = extractVerdict(verdict);
    if (data === null) return null;
    const contextData = extractContext(context);
    if (contextData === null) return null;
    const caps = { ...FULL_CAPS };
    let payload = buildPayload(data, contextData, caps);
    for (let i = 0; i < CAP_SCHEDULE.length && JSON.stringify(payload).length > PAYLOAD_MAX; i += 1) {
      Object.assign(caps, CAP_SCHEDULE[i]);
      payload = buildPayload(data, contextData, caps);
    }
    if (JSON.stringify(payload).length > PAYLOAD_MAX) {
      payload = buildPayload(data, contextData, MINIMAL_CAPS);
    }
    return payload;
  } catch {
    return null;
  }
}
