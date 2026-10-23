// Sustained health-red verdict-to-alert payload renderer. Pure, deterministic
// and side-effect-free: no imports, network, environment reads, logging,
// clocks, randomness, timers or storage. Renders one breach verdict from
// evaluateHealthWindow plus caller context into an alert payload object
// ({content, embeds}) shaped for a Discord webhook body; every other input
// fails closed to null — never a fabricated alert, never a throw.
//
// Validation (all required, no coercion or defaults; extras ignored):
// verdict: {schema_version: 1, service: 'api', environment: 'staging',
//   verdict: 'breach', reason: 'breach',
//   counts: {valid: integer >= 0, red: integer 0..valid,
//     malformed: integer >= 0},
//   longest_red_span_ms: integer >= 0,
//   red_for_ms: integer 1..3600000}
// Only the exact pair verdict 'breach' + reason 'breach' renders. Coherence
// is longest_red_span_ms >= red_for_ms only; a span below the threshold
// contradicts the breach and fails closed. The renderer does not re-derive
// evaluator semantics beyond this frozen contract.
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
// never emits a lone surrogate; identical input always yields
// byte-identical output. Build-SHA correlation flows through the logs URL
// only; health samples carry no release identity, so the renderer invents
// none.
// This module is not wired or deployed: no consumer, schedule, delivery,
// dashboard, or live alerting reads from it, and it carries no webhook
// URL or secret.

const SCHEMA_VERSION = 1;
const EXPECTED_SERVICE = 'api';
const EXPECTED_ENVIRONMENT = 'staging';
const BREACH = 'breach';

const LOGS_URL_RE = /^https?:\/\/[^/\s]\S*$/;
const NON_BLANK_RE = /\S/;
const THRESHOLD_MIN = 1;
const THRESHOLD_MAX = 3600000;

const CONTENT_MAX = 2000;
const PAYLOAD_MAX = 6000;
const TITLE_MAX = 256;
const FIELD_NAME_MAX = 256;
const FIELD_VALUE_MAX = 1024;

const FULL_CAPS = {
  label: 1024,
  logs: 1024,
};
const CAP_SCHEDULE = [
  { label: 512, logs: 512 },
  { label: 256, logs: 256 },
  { label: 128, logs: 128 },
];
const MINIMAL_CAPS = {
  label: 64,
  logs: 256,
};

function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isNonNegativeInteger(value) {
  return typeof value === 'number' && Number.isInteger(value) && value >= 0;
}

function extractCounts(counts) {
  try {
    if (!isRecord(counts)) return null;
    const valid = counts.valid;
    const red = counts.red;
    const malformed = counts.malformed;
    if (!isNonNegativeInteger(valid)) return null;
    if (!isNonNegativeInteger(red)) return null;
    if (!isNonNegativeInteger(malformed)) return null;
    if (red > valid) return null;
    return { valid, red, malformed };
  } catch {
    return null;
  }
}

function extractVerdict(verdict) {
  try {
    if (!isRecord(verdict)) return null;
    if (verdict.schema_version !== SCHEMA_VERSION) return null;
    if (verdict.service !== EXPECTED_SERVICE) return null;
    if (verdict.environment !== EXPECTED_ENVIRONMENT) return null;
    if (verdict.verdict !== BREACH) return null;
    if (verdict.reason !== BREACH) return null;
    const counts = extractCounts(verdict.counts);
    if (counts === null) return null;
    const longest = verdict.longest_red_span_ms;
    if (!isNonNegativeInteger(longest)) return null;
    const threshold = verdict.red_for_ms;
    if (typeof threshold !== 'number' || !Number.isInteger(threshold)) return null;
    if (threshold < THRESHOLD_MIN || threshold > THRESHOLD_MAX) return null;
    if (longest < threshold) return null;
    return {
      valid: counts.valid,
      red: counts.red,
      malformed: counts.malformed,
      longest,
      threshold,
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

function field(name, value) {
  return {
    name: truncateTo(name, FIELD_NAME_MAX),
    value: truncateTo(value, FIELD_VALUE_MAX),
  };
}

function buildPayload(data, contextData, caps) {
  const rational = `${data.red}/${data.valid}`;
  const content = truncateTo(
    `Staging API health BREACH: ${rational} red over ${data.longest}ms (threshold ${data.threshold}ms)`,
    CONTENT_MAX,
  );
  const fields = [
    field('Status', 'breach'),
    field('Window', truncateTo(contextData.windowLabel, caps.label)),
    field('Red samples', `${rational} red (${data.malformed} malformed)`),
    field('Longest red span', `${data.longest}ms (threshold ${data.threshold}ms)`),
    field('Service', EXPECTED_SERVICE),
    field('Environment', EXPECTED_ENVIRONMENT),
    field('Logs', truncateTo(contextData.logsUrl, caps.logs)),
  ];
  return {
    content,
    embeds: [{ title: truncateTo('Staging API health-red alert', TITLE_MAX), fields }],
  };
}

export function renderHealthAlert(verdict, context) {
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
