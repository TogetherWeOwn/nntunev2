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
// context: {logsUrl, windowLabel}, both non-empty strings; logsUrl must
// match /^https?:\/\/[^\/\s]\S*$/ (http/https with a non-empty host and
// no whitespace) and windowLabel must contain a non-space character.
//
// Bounds: content <= 2000 chars, exactly one embed, payload total
// <= 6000 chars, <= 25 fields per embed, field names 1..256 chars,
// field values 1..1024 chars. Present-but-oversized values are tolerated
// and bounded, never rejected: oversized strings truncate to a prefix
// plus "…" (U+2026) and oversized lists cap to a leading prefix with a
// "+N more" marker; identical input always yields byte-identical output.
// This module is not wired or deployed: no consumer, schedule, delivery,
// dashboard, or live alerting reads from it, and it carries no webhook
// URL or secret.

const SCHEMA_VERSION = 1;
const EXPECTED_SERVICE = 'api';
const EXPECTED_ENVIRONMENT = 'staging';
const BREACH = 'breach';

const DECIMAL_RE = /^\d+\.\d{4}$/;
const LOGS_URL_RE = /^https?:\/\/[^/\s]\S*$/;
const NON_BLANK_RE = /\S/;

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
    const keys = [];
    for (let i = 0; i < buckets.length; i += 1) {
      const bucket = buckets[i];
      if (!isRecord(bucket)) return null;
      const key = bucket.key;
      if (typeof key !== 'string') return null;
      keys.push(key);
    }
    return keys;
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
    const counts = extractRational(verdict.counts);
    if (counts === null) return null;
    const rate = verdict.rate;
    if (extractRational(rate) === null) return null;
    const decimal = rate.decimal;
    if (typeof decimal !== 'string' || !DECIMAL_RE.test(decimal)) return null;
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

function extractContext(context) {
  try {
    if (!isRecord(context)) return null;
    const logsUrl = context.logsUrl;
    const windowLabel = context.windowLabel;
    if (typeof logsUrl !== 'string' || !LOGS_URL_RE.test(logsUrl)) return null;
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
  return text.slice(0, max - 1) + '…';
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
