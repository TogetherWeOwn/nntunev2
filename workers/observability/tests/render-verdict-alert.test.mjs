// Independently owned acceptance contract. Builder must not edit this file.
// Import: named synchronous renderVerdictAlert(verdict, context)
// from ../render-verdict-alert.mjs.
// Command (repo root):
// node --experimental-vm-modules --test workers/observability/tests/render-verdict-alert.test.mjs
//
// The tests exercise the actual ESM in an isolated VM, with static/dynamic
// imports, ambient I/O, logging, clocks, random values, timers, environment
// access and storage forbidden. Pure WHATWG URL parsing is available because
// logs-URL validation is pure string work; fetch/WebSocket remain forbidden.
// This is a dependency-free payload renderer, not a webhook delivery, Tail
// consumer, scheduler, dashboard, or alerting deployment. No webhook URL or
// secret may appear in the module, the tests, or any rendered payload.
//
// Frozen verdict input (the subset the renderer validates; mirrors the frozen
// evaluateErrorWindow output on main, which is not imported here):
// {schema_version: 1, service: 'api', environment: 'staging',
//  verdict: 'breach', reason: 'breach',
//  counts: {total: integer >= 1, failed: integer 0..total, ...},
//  rate: {failed: integer 0..total, total: integer >= 1, decimal: /^\d+\.\d{4}$/},
//  thresholds: {overall: {...}, breached_rules: [1..N non-empty strings]},
//  breakdowns: {by_error_class/by_release/by_route: {buckets: [{key: string, ...}], ...}}}
// Only the exact pair verdict 'breach' + reason 'breach' renders. 'pass'/'ok',
// 'unknown'/'insufficient_data', mismatched pairs, and any other status fail
// closed to null. Non-object verdicts, missing/mistyped required structure,
// wrong schema/service/environment labels, bad counts/rate rationals, an empty
// breached_rules list, or missing bucket arrays fail closed to null.
// Other counts/rate/bucket fields are not validated by this contract.
//
// Frozen context input: {logsUrl, windowLabel}, both non-empty strings with no
// trimming; logsUrl must match /^https?:\/\/\S+$/ with a non-empty host (http
// and https both render; anything else fails closed to null). Extra fields on
// the verdict or the context are ignored, never echoed, never fatal.
//
// Output: exactly {content, embeds} (no other top-level keys) ONLY for a
// breach verdict, else null. Never a throw for any input, never a fabricated
// alert. content is a non-empty string. embeds is a 1..10 array of objects.
// Breach payloads carry, somewhere in the JSON: the verdict status (pinned
// case-insensitively), the exact rational as "<failed>/<total>" (e.g. "3/4"),
// the 4-decimal rate verbatim (e.g. "0.7500"), every breached rule name, top
// breakdown entry keys, release SHAs, the logs URL verbatim, and the window
// label verbatim (all for normal-length inputs).
//
// Discord bounds (pinned subset; lengths are JS string .length, total is
// JSON.stringify(payload).length): content <= 2000 chars, 1..10 embeds,
// payload total <= 6000 chars, <= 25 fields per embed, field name 1..256,
// field value 1..1024, title <= 256, description <= 4096, footer text <= 2048,
// author name <= 256. Present-but-oversized values are tolerated and bounded,
// never rejected: oversized strings truncate to a prefix plus "…" (U+2026,
// single ellipsis char), oversized lists cap to a leading prefix; identical
// input always yields byte-identical output. Wrong types or missing required
// structure, by contrast, fail closed to null.
// Tests only: no source placeholder; the Builder adds the module, README and CI.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import vm from 'node:vm';

const MODULE_URL = new URL('../render-verdict-alert.mjs', import.meta.url);
const SHA_A = '0123456789abcdef0123456789abcdef01234567';
const SHA_B = 'fedcba9876543210fedcba9876543210fedcba98';
const LOGS_URL = 'https://logs.example.invalid/staging/api/window/2026-10-22-1200';
const WINDOW_LABEL = 'staging api error-rate window 12:00-12:05 UTC';
const LONG = 'x'.repeat(5000);
// All canaries are deliberately synthetic, never credentials or real user data.
const CANARIES = [
  'SYNTHETIC_SECRET_NEVER_EMIT_914',
  'synthetic-person-914@example.invalid',
  'synthetic-tenant-914',
  'SYNTHETIC_COOKIE_NEVER_EMIT_914',
  'SYNTHETIC_STACK_NEVER_EMIT_914',
];
const FORBIDDEN = [];
const deny = (name) => (..._args) => {
  FORBIDDEN.push(name);
  throw new Error(`renderer attempted forbidden ambient access: ${name}`);
};
const math = Object.create(Math);
math.random = deny('Math.random');
const context = vm.createContext({
  console: Object.fromEntries(['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'assert']
    .map((name) => [name, deny(`console.${name}`)])),
  fetch: deny('fetch'),
  WebSocket: deny('WebSocket'),
  XMLHttpRequest: deny('XMLHttpRequest'),
  Date: deny('Date'),
  JSON,
  URL,
  URLSearchParams,
  performance: new Proxy({}, { get: deny('performance') }),
  process: new Proxy({}, { get: deny('process/environment') }),
  crypto: new Proxy({}, { get: deny('crypto/random') }),
  Math: math,
  setTimeout: deny('setTimeout'), setInterval: deny('setInterval'),
  setImmediate: deny('setImmediate'), queueMicrotask: deny('queueMicrotask'),
  TextEncoder, TextDecoder,
});
let namespace;
let loadFailure;
let lastRawResult;
try {
  const source = await readFile(MODULE_URL, 'utf8');
  const module = new vm.SourceTextModule(source, {
    context,
    identifier: MODULE_URL.href,
    importModuleDynamically: deny('dynamic import'),
  });
  await module.link(deny('static import'));
  await module.evaluate({ timeout: 1000 });
  namespace = module.namespace;
} catch (error) {
  // Deliberately no fake implementation or source placeholder. Missing behavior
  // becomes an assertion failure, not an uncaught module-resolution exception.
  loadFailure = error.code === 'ENOENT' ? 'missing renderer module' : 'renderer did not load safely';
}

function render(verdict, contextArg) {
  assert.equal(typeof namespace?.renderVerdictAlert, 'function', loadFailure ?? 'missing named export');
  const before = FORBIDDEN.length;
  let result;
  let thrown;
  try { result = namespace.renderVerdictAlert(verdict, contextArg); } catch (error) { thrown = error; }
  if (thrown) assertNoCanaries(String(thrown) + String(thrown?.stack));
  assert.equal(FORBIDDEN.length, before, 'renderer attempted a forbidden side effect');
  assert.equal(thrown, undefined, 'JSON-compatible input must not throw');
  assert.notEqual(result, undefined, 'renderer must return a payload or null');
  assert.equal(typeof result?.then, 'undefined', 'renderer must be synchronous');
  assertNoCanaries(JSON.stringify(result));
  lastRawResult = result;
  // Normalize only the cross-realm prototypes, not the production field values.
  return result === null ? null : JSON.parse(JSON.stringify(result));
}

function assertNoCanaries(text) {
  for (const canary of CANARIES) {
    assert.equal(text.includes(canary), false, 'renderer leaked a synthetic secret/PII canary');
  }
}

function assertDiscordBounds(payload) {
  assert.equal(typeof payload.content, 'string', 'content must be a string');
  assert.equal(payload.content.length >= 1, true, 'breach content must not be empty');
  assert.equal(payload.content.length <= 2000, true,
    `content exceeds 2000 chars (${payload.content.length})`);
  assert.equal(Array.isArray(payload.embeds), true, 'embeds must be an array');
  assert.equal(payload.embeds.length >= 1 && payload.embeds.length <= 10, true,
    `embeds must be 1..10 (got ${payload.embeds.length})`);
  for (const embed of payload.embeds) {
    assert.equal(typeof embed === 'object' && embed !== null && !Array.isArray(embed), true,
      'each embed must be an object');
    if (embed.title !== undefined) {
      assert.equal(typeof embed.title, 'string');
      assert.equal(embed.title.length <= 256, true, `embed title exceeds 256 chars (${embed.title.length})`);
    }
    if (embed.description !== undefined) {
      assert.equal(typeof embed.description, 'string');
      assert.equal(embed.description.length <= 4096, true,
        `embed description exceeds 4096 chars (${embed.description.length})`);
    }
    if (embed.fields !== undefined) {
      assert.equal(Array.isArray(embed.fields), true, 'embed fields must be an array');
      assert.equal(embed.fields.length <= 25, true,
        `embed exceeds 25 fields (${embed.fields.length})`);
      for (const field of embed.fields) {
        assert.equal(typeof field?.name, 'string', 'each field needs a string name');
        assert.equal(typeof field?.value, 'string', 'each field needs a string value');
        assert.equal(field.name.length >= 1 && field.name.length <= 256, true,
          `field name must be 1..256 chars (got ${field.name.length})`);
        assert.equal(field.value.length >= 1 && field.value.length <= 1024, true,
          `field value must be 1..1024 chars (got ${field.value.length})`);
      }
    }
    if (embed.footer !== undefined) {
      assert.equal(typeof embed.footer === 'object' && embed.footer !== null, true);
      if (embed.footer.text !== undefined) {
        assert.equal(typeof embed.footer.text, 'string');
        assert.equal(embed.footer.text.length <= 2048, true,
          `footer text exceeds 2048 chars (${embed.footer.text.length})`);
      }
    }
    if (embed.author !== undefined) {
      assert.equal(typeof embed.author === 'object' && embed.author !== null, true);
      if (embed.author.name !== undefined) {
        assert.equal(typeof embed.author.name, 'string');
        assert.equal(embed.author.name.length <= 256, true,
          `author name exceeds 256 chars (${embed.author.name.length})`);
      }
    }
  }
  const total = JSON.stringify(payload).length;
  assert.equal(total <= 6000, true, `payload total exceeds 6000 chars (${total})`);
}

function breachVerdict(overrides = {}) {
  return {
    schema_version: 1,
    service: 'api',
    environment: 'staging',
    verdict: 'breach',
    reason: 'breach',
    counts: {
      total: 4, failed: 3, healthy: 1, excluded: 0, malformed: 0,
      by_flag: { http_5xx: 3, exception: 3, exceeded_cpu: 0, exceeded_memory: 0 },
    },
    rate: { failed: 3, total: 4, decimal: '0.7500' },
    thresholds: {
      overall: { failed: 1, total: 4, breached: true },
      breached_rules: ['error_class:internal_error', 'overall', `release:${SHA_B}`],
    },
    breakdowns: {
      by_error_class: {
        buckets: [
          { key: 'internal_error', total: 3, failed: 3, decimal: '1.0000' },
          { key: 'unknown', total: 1, failed: 0, decimal: '0.0000' },
        ],
        overflow: { total: 0, failed: 0, distinct_keys: 0 },
      },
      by_release: {
        buckets: [
          { key: SHA_A, total: 1, failed: 0, decimal: '0.0000' },
          { key: SHA_B, total: 3, failed: 3, decimal: '1.0000' },
        ],
        overflow: { total: 0, failed: 0, distinct_keys: 0 },
      },
      by_route: {
        buckets: [
          { key: 'GET /v1/library/search', total: 4, failed: 3, decimal: '0.7500' },
        ],
        overflow: { total: 0, failed: 0, distinct_keys: 0 },
      },
    },
    ...overrides,
  };
}

function contextFixture(overrides = {}) {
  return { logsUrl: LOGS_URL, windowLabel: WINDOW_LABEL, ...overrides };
}

function emptyOverflow() {
  return { total: 0, failed: 0, distinct_keys: 0 };
}

function assertBreachCarries(payload, {
  rational = '3/4', decimal = '0.7500',
  rules = ['error_class:internal_error', 'overall', `release:${SHA_B}`],
  keys = ['internal_error', 'GET /v1/library/search', SHA_A, SHA_B],
  logsUrl = LOGS_URL, windowLabel = WINDOW_LABEL,
} = {}) {
  assert.notEqual(payload, null, 'breach must render a payload');
  assert.deepEqual(Object.keys(payload), ['content', 'embeds']);
  assertDiscordBounds(payload);
  const text = JSON.stringify(payload);
  assert.equal(text.toLowerCase().includes('breach'), true, 'payload must carry the verdict status');
  assert.equal(text.includes(rational), true, `payload must carry the exact rational ${rational}`);
  assert.equal(text.includes(decimal), true, `payload must carry the 4-decimal rate ${decimal}`);
  for (const rule of rules) {
    assert.equal(text.includes(rule), true, `payload must name breached rule ${rule}`);
  }
  for (const key of keys) {
    assert.equal(text.includes(key), true, `payload must carry breakdown entry ${key}`);
  }
  assert.equal(text.includes(logsUrl), true, 'payload must carry the logs URL');
  assert.equal(text.includes(windowLabel), true, 'payload must carry the window label');
  assert.equal(text.includes('discord.com'), false, 'payload must not contain a webhook URL');
  return text;
}

function deepFreeze(value) {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

// Break caught: a missing export, import-time side effect, or dependency bypass.
test('loads the named renderer export without ambient effects or dependencies', () => {
  assert.equal(loadFailure, undefined, 'implement render-verdict-alert.mjs with the frozen named export');
  assert.equal(typeof namespace?.renderVerdictAlert, 'function');
  assert.deepEqual(FORBIDDEN, []);
});

// Break caught: a breach that drops the status, rational, rules, breakdowns,
// SHAs, logs URL, or window label, or that exceeds Discord bounds.
test('breach renders a bounded payload with all required fields', () => {
  assertBreachCarries(render(breachVerdict(), contextFixture()));
});

test('http and https logs URLs both render', () => {
  const payload = render(breachVerdict(), contextFixture({ logsUrl: 'http://logs.example.invalid/w/2' }));
  assertBreachCarries(payload, { logsUrl: 'http://logs.example.invalid/w/2' });
});

test('extra verdict and context fields are ignored, never echoed, never fatal', () => {
  const verdict = breachVerdict({ note: 'extra top-level field', dashboard_url: 'https://example.invalid/private' });
  verdict.counts.note = 'extra nested field';
  verdict.thresholds.note = 'extra nested field';
  const payload = render(verdict, contextFixture({ note: 'extra context field', trace_id: 'synthetic-1' }));
  const text = assertBreachCarries(payload);
  assert.equal(text.includes('extra top-level field'), false, 'extras must not be echoed');
  assert.equal(text.includes('example.invalid/private'), false, 'extras must not be echoed');
});

// Break caught: a pass or unknown verdict rendered as an alert (fabrication).
test('pass/ok verdicts never render', () => {
  const verdict = breachVerdict({
    verdict: 'pass', reason: 'ok',
    counts: {
      total: 4, failed: 1, healthy: 3, excluded: 0, malformed: 0,
      by_flag: { http_5xx: 1, exception: 1, exceeded_cpu: 0, exceeded_memory: 0 },
    },
    rate: { failed: 1, total: 4, decimal: '0.2500' },
    thresholds: { overall: { failed: 1, total: 4, breached: false }, breached_rules: [] },
  });
  assert.equal(render(verdict, contextFixture()), null);
});

test('unknown/insufficient_data verdicts never render', () => {
  const verdict = breachVerdict({
    verdict: 'unknown', reason: 'insufficient_data',
    counts: {
      total: 1, failed: 0, healthy: 1, excluded: 0, malformed: 0,
      by_flag: { http_5xx: 0, exception: 0, exceeded_cpu: 0, exceeded_memory: 0 },
    },
    rate: { failed: 0, total: 1, decimal: '0.0000' },
    thresholds: { overall: { failed: 1, total: 4, breached: false }, breached_rules: [] },
  });
  assert.equal(render(verdict, contextFixture()), null);
});

test('mismatched or mistyped verdict/reason pairs fail closed', () => {
  for (const [verdict, reason] of [
    ['breach', 'ok'], ['breach', 'insufficient_data'], ['pass', 'breach'], ['unknown', 'breach'],
    ['BREACH', 'breach'], ['breach', 'BREACH'], ['Breach', 'breach'], ['', 'breach'], ['breach', ''],
    ['ok', 'ok'], ['unknown', 'unknown'], [null, 'breach'], ['breach', null], [1, 'breach'],
  ]) {
    assert.equal(render(breachVerdict({ verdict, reason }), contextFixture()), null,
      `verdict=${String(verdict)} reason=${String(reason)} must not render`);
  }
});

// Break caught: a non-object, exotic, or structurally incomplete verdict
// rendered as an alert, or crashing the renderer instead of failing closed.
test('non-object and exotic verdicts fail closed', () => {
  for (const verdict of [null, undefined, 0, 1, true, false, 'breach', [], [breachVerdict()],
    () => 'breach', Symbol('breach'), 10n]) {
    assert.equal(render(verdict, contextFixture()), null, `typeof verdict ${typeof verdict} must not render`);
  }
});

test('verdicts with missing required structure fail closed', () => {
  const cases = [];
  for (const key of ['schema_version', 'service', 'environment', 'verdict', 'reason',
    'counts', 'rate', 'thresholds', 'breakdowns']) {
    const verdict = breachVerdict();
    delete verdict[key];
    cases.push([`missing ${key}`, verdict]);
  }
  cases.push(['null counts', breachVerdict({ counts: null })]);
  cases.push(['array counts', breachVerdict({ counts: [] })]);
  cases.push(['string rate', breachVerdict({ rate: '0.7500' })]);
  cases.push(['null thresholds', breachVerdict({ thresholds: null })]);
  cases.push(['null breakdowns', breachVerdict({ breakdowns: null })]);
  for (const [name, verdict] of cases) {
    assert.equal(render(verdict, contextFixture()), null, `${name} must not render`);
  }
});

test('wrong schema version, service, or environment fails closed', () => {
  for (const verdict of [
    breachVerdict({ schema_version: 2 }), breachVerdict({ schema_version: '1' }),
    breachVerdict({ service: 'web' }), breachVerdict({ service: 'API' }),
    breachVerdict({ service: '' }), breachVerdict({ environment: 'prod' }),
    breachVerdict({ environment: 'Staging' }), breachVerdict({ environment: '' }),
  ]) {
    assert.equal(render(verdict, contextFixture()), null, 'wrong schema/service/environment must not render');
  }
});

test('bad counts rationals fail closed', () => {
  const base = breachVerdict().counts;
  for (const [name, counts] of [
    ['zero total', { ...base, total: 0, failed: 0 }],
    ['failed above total', { ...base, total: 4, failed: 5 }],
    ['negative failed', { ...base, failed: -1 }],
    ['negative total', { ...base, total: -1 }],
    ['fractional total', { ...base, total: 4.5 }],
    ['string failed', { ...base, failed: '3' }],
    ['string total', { ...base, total: '4' }],
    ['NaN total', { ...base, total: NaN }],
    ['missing total', { failed: 3 }],
    ['missing failed', { total: 4 }],
  ]) {
    assert.equal(render(breachVerdict({ counts }), contextFixture()), null, `${name} must not render`);
  }
});

test('bad rate rationals or decimals fail closed', () => {
  for (const [name, rate] of [
    ['missing decimal', { failed: 3, total: 4 }],
    ['short decimal', { failed: 3, total: 4, decimal: '0.75' }],
    ['percent decimal', { failed: 3, total: 4, decimal: '75%' }],
    ['numeric decimal', { failed: 3, total: 4, decimal: 0.75 }],
    ['empty decimal', { failed: 3, total: 4, decimal: '' }],
    ['negative failed', { failed: -1, total: 4, decimal: '0.7500' }],
    ['failed above total', { failed: 5, total: 4, decimal: '1.2500' }],
    ['zero total', { failed: 0, total: 0, decimal: '0.0000' }],
    ['string total', { failed: 3, total: '4', decimal: '0.7500' }],
  ]) {
    assert.equal(render(breachVerdict({ rate }), contextFixture()), null, `${name} must not render`);
  }
});

test('breach without named rules fails closed', () => {
  const overall = { failed: 1, total: 4, breached: true };
  for (const [name, thresholds] of [
    ['empty rules', { overall, breached_rules: [] }],
    ['missing rules', { overall }],
    ['string rules', { overall, breached_rules: 'overall' }],
    ['numeric entry', { overall, breached_rules: ['overall', 42] }],
    ['empty-string entry', { overall, breached_rules: ['overall', ''] }],
    ['null entry', { overall, breached_rules: ['overall', null] }],
  ]) {
    assert.equal(render(breachVerdict({ thresholds }), contextFixture()), null, `${name} must not render`);
  }
});

test('bad breakdowns fail closed', () => {
  const good = breachVerdict().breakdowns;
  const oneBucket = (key) => ({ buckets: [{ key, total: 4, failed: 3, decimal: '0.7500' }], overflow: emptyOverflow() });
  const cases = [];
  for (const dim of ['by_error_class', 'by_release', 'by_route']) {
    const breakdowns = { ...good };
    delete breakdowns[dim];
    cases.push([`missing ${dim}`, breakdowns]);
  }
  cases.push(['null by_release', { ...good, by_release: null }]);
  cases.push(['string buckets', { ...good, by_route: { buckets: 'x', overflow: emptyOverflow() } }]);
  cases.push(['null bucket entry', { ...good, by_error_class: { buckets: [null], overflow: emptyOverflow() } }]);
  cases.push(['bucket without key', { ...good, by_error_class: { buckets: [{ total: 1, failed: 1 }], overflow: emptyOverflow() } }]);
  cases.push(['numeric bucket key', { ...good, by_error_class: oneBucket(42) }]);
  for (const [name, breakdowns] of cases) {
    assert.equal(render(breachVerdict({ breakdowns }), contextFixture()), null, `${name} must not render`);
  }
});

// Break caught: a missing, mistyped, or non-http(s) context rendered anyway,
// or a logs URL that smuggles a non-web scheme into the alert.
test('missing or mistyped context fails closed', () => {
  for (const contextArg of [null, undefined, 0, 'x', true, [], {},
    { logsUrl: LOGS_URL }, { windowLabel: WINDOW_LABEL },
    { logsUrl: LOGS_URL, windowLabel: '' }, { logsUrl: '', windowLabel: WINDOW_LABEL },
    { logsUrl: 42, windowLabel: WINDOW_LABEL }, { logsUrl: LOGS_URL, windowLabel: 42 },
    { logsUrl: null, windowLabel: WINDOW_LABEL }, { logsUrl: LOGS_URL, windowLabel: null },
    { logsUrl: '   ', windowLabel: WINDOW_LABEL }, { logsUrl: LOGS_URL, windowLabel: '   ' }]) {
    assert.equal(render(breachVerdict(), contextArg), null, 'bad context must not render');
  }
});

test('non-http(s) logs URLs fail closed', () => {
  for (const logsUrl of ['ftp://logs.example.invalid/w/1', 'notaurl', 'http://', 'https://',
    '//logs.example.invalid/w/1', 'javascript:alert(1)', ' https://logs.example.invalid/w/1',
    'https://logs.example.invalid/w/1 ', 'https:///w/1', 'data:text/plain,hi',
    'file:///etc/passwd', 'https://', 'http://exa mple.invalid/w/1']) {
    assert.equal(render(breachVerdict(), contextFixture({ logsUrl })), null,
      `logsUrl ${JSON.stringify(logsUrl)} must not render`);
  }
});

// Break caught: unbounded output for adversarial strings, dropped short fields
// when truncating, or nondeterministic truncation across identical calls.
test('adversarial long rule names, breakdown keys, and SHAs stay within bounds', () => {
  const verdict = breachVerdict({
    thresholds: {
      overall: { failed: 1, total: 4, breached: true },
      breached_rules: [`error_class:${LONG}`, 'overall', `release:${LONG}`],
    },
    breakdowns: {
      by_error_class: {
        buckets: [{ key: LONG, total: 3, failed: 3, decimal: '1.0000' }],
        overflow: emptyOverflow(),
      },
      by_release: {
        buckets: [{ key: LONG, total: 4, failed: 3, decimal: '0.7500' }],
        overflow: emptyOverflow(),
      },
      by_route: {
        buckets: [{ key: `GET /${LONG}`, total: 4, failed: 3, decimal: '0.7500' }],
        overflow: emptyOverflow(),
      },
    },
  });
  const first = render(verdict, contextFixture());
  assert.notEqual(first, null, 'oversized strings must truncate, not fail closed');
  assert.deepEqual(Object.keys(first), ['content', 'embeds']);
  assertDiscordBounds(first);
  const text = JSON.stringify(first);
  assert.equal(text.includes('…'), true, 'oversized strings must truncate with an ellipsis marker');
  assert.equal(text.includes('3/4'), true, 'rational must survive truncation');
  assert.equal(text.includes('0.7500'), true, 'decimal must survive truncation');
  assert.equal(text.includes('overall'), true, 'short rules must survive truncation');
  assert.equal(text.includes(LOGS_URL), true, 'logs URL must survive truncation');
  assert.equal(text.includes(WINDOW_LABEL), true, 'window label must survive truncation');
  const second = render(verdict, contextFixture());
  assert.equal(JSON.stringify(second), text, 'adversarial render must be deterministic');
});

test('adversarial long window label stays within bounds without losing the logs URL', () => {
  const contextArg = contextFixture({ windowLabel: `window ${LONG}` });
  const first = render(breachVerdict(), contextArg);
  assert.notEqual(first, null, 'oversized label must truncate, not fail closed');
  assertDiscordBounds(first);
  const text = JSON.stringify(first);
  assert.equal(text.includes('…'), true, 'oversized label must truncate with an ellipsis marker');
  assert.equal(text.includes(LOGS_URL), true, 'logs URL must survive label truncation');
  assert.equal(text.includes('3/4'), true, 'rational must survive label truncation');
  assert.equal(JSON.stringify(render(breachVerdict(), contextFixture({ windowLabel: `window ${LONG}` }))), text,
    'label truncation must be deterministic');
});

test('adversarial breakdown and rule cardinality stays within bounds', () => {
  const buckets = (prefix) => Array.from({ length: 100 }, (_, i) => ({
    key: `${prefix}${String(i).padStart(3, '0')}`, total: 1, failed: 1, decimal: '1.0000',
  }));
  const verdict = breachVerdict({
    thresholds: {
      overall: { failed: 1, total: 4, breached: true },
      breached_rules: ['overall',
        ...Array.from({ length: 99 }, (_, i) => `error_class:rule_${String(i).padStart(3, '0')}`)],
    },
    breakdowns: {
      by_error_class: { buckets: buckets('error_'), overflow: emptyOverflow() },
      by_release: { buckets: buckets('release_'), overflow: emptyOverflow() },
      by_route: { buckets: buckets('GET /route_'), overflow: emptyOverflow() },
    },
  });
  const first = render(verdict, contextFixture());
  assert.notEqual(first, null, 'oversized lists must cap, not fail closed');
  assertDiscordBounds(first);
  const text = JSON.stringify(first);
  assert.equal(text.includes('3/4'), true, 'rational must survive list capping');
  assert.equal(text.includes('0.7500'), true, 'decimal must survive list capping');
  assert.equal(text.includes(LOGS_URL), true, 'logs URL must survive list capping');
  assert.equal(text.includes(WINDOW_LABEL), true, 'window label must survive list capping');
  assert.equal(JSON.stringify(render(verdict, contextFixture())), text,
    'capped render must be deterministic');
});

// Break caught: nondeterministic output, mutated inputs, cached output, or a
// throw/ambient effect on hostile input instead of a clean null.
test('same verdict and context produce byte-identical JSON', () => {
  const first = render(breachVerdict(), contextFixture());
  const second = render(breachVerdict(), contextFixture());
  assert.deepEqual(second, first);
  assert.equal(JSON.stringify(second), JSON.stringify(first));
});

test('secrets/PII and unknown fields never escape in output or throws', () => {
  const verdict = breachVerdict({
    authorization: CANARIES[0], email: CANARIES[1], tenant_id: CANARIES[2],
    cookie: CANARIES[3], stack: CANARIES[4],
  });
  verdict.breakdowns.by_release.buckets[0].secret = CANARIES[0];
  verdict.breakdowns.by_error_class.buckets[0].note = CANARIES[2];
  verdict.breakdowns.by_route.buckets[0].trace = CANARIES[4];
  const payload = render(verdict, contextFixture({ note: CANARIES[3], token: CANARIES[0] }));
  assertBreachCarries(payload);
  assert.deepEqual(FORBIDDEN, [], 'no diagnostics or external/ambient effects permitted');
});

test('deeply frozen inputs are not mutated and repeated calls agree', () => {
  const verdict = breachVerdict();
  const contextArg = contextFixture();
  const beforeVerdict = JSON.stringify(verdict);
  const beforeContext = JSON.stringify(contextArg);
  deepFreeze(verdict); deepFreeze(contextArg);
  const first = render(verdict, contextArg);
  const second = render(verdict, contextArg);
  assert.deepEqual(second, first);
  assert.equal(JSON.stringify(verdict), beforeVerdict);
  assert.equal(JSON.stringify(contextArg), beforeContext);
});

test('mutating a returned payload cannot change later renders or inputs', () => {
  const verdict = breachVerdict();
  const contextArg = contextFixture();
  const firstText = JSON.stringify(render(verdict, contextArg));
  // Exercise the actual returned references; a JSON clone would hide caching.
  // Frozen output is also valid, hence Reflect.set rather than strict assignment.
  Reflect.set(lastRawResult, 'content', 'changed');
  Reflect.set(lastRawResult, 'embeds', []);
  assert.equal(JSON.stringify(render(verdict, contextArg)), firstText);
  assert.equal(verdict.verdict, 'breach');
  assert.equal(contextArg.logsUrl, LOGS_URL);
});

test('hostile throwing getters and proxies fail closed without throwing', () => {
  const getter = breachVerdict();
  Object.defineProperty(getter, 'verdict', {
    get() { throw new Error('synthetic boom'); }, configurable: true,
  });
  assert.equal(render(getter, contextFixture()), null);
  const proxy = new Proxy(breachVerdict(), {
    get() { throw new Error('synthetic boom'); },
  });
  assert.equal(render(proxy, contextFixture()), null);
});

test('cyclic verdicts never throw and never produce an unbounded payload', () => {
  const cyclic = breachVerdict();
  cyclic.self = cyclic;
  const out = render(cyclic, contextFixture());
  if (out !== null) assertDiscordBounds(out);
});

test('module source carries no webhook URL', async () => {
  assert.equal(loadFailure, undefined, 'implement render-verdict-alert.mjs first');
  const source = await readFile(MODULE_URL, 'utf8');
  assert.equal(source.toLowerCase().includes('discord.com'), false,
    'module must not contain a webhook host');
});

test('no ambient effects across the whole suite', () => {
  assert.deepEqual(FORBIDDEN, []);
});
