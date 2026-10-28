// Independently owned acceptance contract. Builder must not edit this file.
// Import: named synchronous renderHealthAlert(verdict, context)
// from ../render-health-alert.mjs.
// Command (repo root):
// node --experimental-vm-modules --test workers/observability/tests/render-health-alert.test.mjs
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
// evaluateHealthWindow output on main, which is not imported here):
// {schema_version: 1, service: 'api', environment: 'staging',
//  verdict: 'breach', reason: 'breach',
//  counts: {valid: integer >= 0, red: integer 0..valid, malformed: integer >= 0},
//  longest_red_span_ms: integer >= 0, red_for_ms: integer 1..3600000}
// Only the exact pair verdict 'breach' + reason 'breach' renders. 'ok'/'healthy',
// 'unknown'/'insufficient_data', mismatched pairs, and any other status fail
// closed to null. Non-object verdicts, missing/mistyped required structure,
// wrong schema/service/environment labels, bad counts, bad spans, bad thresholds,
// or a longest span below the threshold (longest_red_span_ms < red_for_ms) fail
// closed to null. Coherence is longest_red_span_ms >= red_for_ms only; the
// renderer does not re-derive evaluator semantics beyond the frozen contract.
// Other fields are not validated by this contract.
//
// Frozen context input: {logsUrl, windowLabel}, both non-empty strings with no
// trimming; logsUrl must match /^https?:\/\/[^\/\s]\S*$/" and parse via WHATWG
// URL with an http/https protocol and a non-empty hostname (http and https both
// render; anything else fails closed to null); windowLabel must contain a
// non-space character. Extra fields on the verdict or the context are ignored,
// never echoed, never fatal.
//
// Output: exactly {content, embeds} (no other top-level keys) ONLY for a
// breach verdict, else null. Never a throw for any input, never a fabricated
// alert. content is a non-empty string. embeds is exactly one object.
// Breach payloads carry, somewhere in the JSON: the verdict status (pinned
// case-insensitively), the exact rational as "<red>/<valid>" (e.g. "2/5"),
// the longest red span and the threshold verbatim in ms (e.g. "130000" and
// "120000"), the service and environment labels, the logs URL verbatim, and
// the window label verbatim (all for normal-length inputs). Build-SHA
// correlation flows through the logs URL only; health samples carry no release
// identity, so the renderer must not invent one.
//
// Discord bounds (pinned subset; lengths are JS string .length, total is
// JSON.stringify(payload).length): content <= 2000 chars, exactly one embed,
// payload total <= 6000 chars, <= 25 fields per embed, field name 1..256,
// field value 1..1024, title <= 256, description <= 4096, footer text <= 2048,
// author name <= 256. Present-but-oversized values are tolerated and bounded,
// never rejected: oversized strings truncate to a prefix plus "…" (U+2026,
// single ellipsis char, no lone surrogates), oversized lists cap; identical
// input always yields byte-identical output. Wrong types or missing required
// structure, by contrast, fail closed to null.
// Tests only: no source placeholder; the Builder adds the module, README and CI.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import vm from 'node:vm';

const MODULE_URL = new URL('../render-health-alert.mjs', import.meta.url);
const LOGS_URL = 'https://logs.example.invalid/window/2026-10-22-1200';
const WINDOW_LABEL = 'health window 12:00-12:05 UTC';
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
  assert.equal(typeof namespace?.renderHealthAlert, 'function', loadFailure ?? 'missing named export');
  const before = FORBIDDEN.length;
  let result;
  let thrown;
  try { result = namespace.renderHealthAlert(verdict, contextArg); } catch (error) { thrown = error; }
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

const WEBHOOK_HOST = 'discord.com';
// Leading-dot suffix: exact host or any subdomain, never a superstring
// collision such as evil-discord.com. The leading dot is also the CodeQL
// js/incomplete-url-substring-sanitization whitelist shape for endsWith.
const WEBHOOK_SUFFIX = '.discord.com';

function collectStringLeaves(value, out = []) {
  if (typeof value === 'string') {
    out.push(value);
  } else if (Array.isArray(value)) {
    for (const item of value) collectStringLeaves(item, out);
  } else if (value && typeof value === 'object') {
    for (const item of Object.values(value)) collectStringLeaves(item, out);
  }
  return out;
}

// Exact-equality replacement for text.includes(logsUrl): the logs URL must
// appear verbatim as a whole payload string. Fails on drop/mangle, with no
// URL-substring check for CodeQL to flag.
function assertCarriesLogsUrl(payload, logsUrl, message) {
  const carried = collectStringLeaves(payload).some((value) => value === logsUrl);
  assert.equal(carried, true, message);
}

function isWebhookHost(hostname) {
  const host = String(hostname).toLowerCase();
  return host === WEBHOOK_HOST || host.endsWith(WEBHOOK_SUFFIX);
}

// WHATWG-parse replacement for text.includes('discord.com'): each token is
// parsed as a URL where possible, else treated as a bare host, and the host
// is compared exactly (plus leading-dot subdomains). Stricter than substring:
// evil-discord.com and discord.com.evil.net correctly pass. Parentheses are
// delimiters so Markdown links "[x](https://host/...)" tokenize the URL bare.
function assertNoWebhookHost(text, message) {
  const tokens = String(text).split(/[\s"'<>\[\](){}|,;]+/);
  for (const token of tokens) {
    if (token.length === 0) continue;
    let hostname;
    try {
      hostname = new URL(token).hostname;
    } catch {
      hostname = token.split('/')[0].split(':')[0].split('?')[0].split('#')[0].split('@').pop();
    }
    assert.equal(isWebhookHost(hostname), false, message);
  }
}

// WHATWG-parse survival check for truncated logs URLs: the combined worst
// case truncates the URL, so verbatim equality cannot hold. Instead assert
// that some payload string still parses to the expected logs host. Exact
// hostname equality, no URL-substring check for CodeQL to flag.
function assertLogsHostSurvives(payload, hostname, message) {
  const carried = collectStringLeaves(payload).some((value) => {
    try {
      return new URL(value).hostname === hostname;
    } catch {
      return false;
    }
  });
  assert.equal(carried, true, message);
}

// Lone-surrogate scan: every high surrogate must be followed by a low
// surrogate, every low preceded by a high. Truncation that splits a
// non-BMP pair leaves a lone surrogate a strict receiver can reject.
function assertNoLoneSurrogates(payload, message) {
  for (const leaf of collectStringLeaves(payload)) {
    for (let i = 0; i < leaf.length; i += 1) {
      const code = leaf.charCodeAt(i);
      if (code >= 0xD800 && code <= 0xDBFF) {
        const next = leaf.charCodeAt(i + 1);
        assert.equal(next >= 0xDC00 && next <= 0xDFFF, true,
          `${message} (lone high surrogate at index ${i})`);
      } else if (code >= 0xDC00 && code <= 0xDFFF) {
        const prev = i > 0 ? leaf.charCodeAt(i - 1) : 0;
        assert.equal(prev >= 0xD800 && prev <= 0xDBFF, true,
          `${message} (lone low surrogate at index ${i})`);
      }
    }
  }
}

function assertDiscordBounds(payload) {
  assert.equal(typeof payload.content, 'string', 'content must be a string');
  assert.equal(payload.content.length >= 1, true, 'breach content must not be empty');
  assert.equal(payload.content.length <= 2000, true,
    `content exceeds 2000 chars (${payload.content.length})`);
  assert.equal(Array.isArray(payload.embeds), true, 'embeds must be an array');
  assert.equal(payload.embeds.length === 1, true,
    `embeds must be exactly one (got ${payload.embeds.length})`);
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
    counts: { valid: 5, red: 2, malformed: 1 },
    longest_red_span_ms: 130000,
    red_for_ms: 120000,
    ...overrides,
  };
}

function contextFixture(overrides = {}) {
  return { logsUrl: LOGS_URL, windowLabel: WINDOW_LABEL, ...overrides };
}

function assertBreachCarries(payload, {
  rational = '2/5',
  longest = '130000',
  threshold = '120000',
  logsUrl = LOGS_URL,
  windowLabel = WINDOW_LABEL,
} = {}) {
  assert.notEqual(payload, null, 'breach must render a payload');
  assert.deepEqual(Object.keys(payload), ['content', 'embeds']);
  assertDiscordBounds(payload);
  const text = JSON.stringify(payload);
  assert.equal(text.toLowerCase().includes('breach'), true, 'payload must carry the verdict status');
  assert.equal(text.includes(rational), true, `payload must carry the exact rational ${rational}`);
  assert.equal(text.includes(longest), true, `payload must carry the longest red span ${longest}`);
  assert.equal(text.includes(threshold), true, `payload must carry the threshold ${threshold}`);
  assert.equal(text.toLowerCase().includes('ms'), true, 'payload must carry ms units for spans');
  // Fixtures keep service/environment labels out of the URL and window label,
  // so these substrings must come from the verdict labels.
  assert.equal(text.includes('api'), true, 'payload must carry the service label');
  assert.equal(text.includes('staging'), true, 'payload must carry the environment label');
  assertCarriesLogsUrl(payload, logsUrl, 'payload must carry the logs URL');
  assert.equal(text.includes(windowLabel), true, 'payload must carry the window label');
  assertNoWebhookHost(text, 'payload must not contain a webhook URL');
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
  assert.equal(loadFailure, undefined, 'implement render-health-alert.mjs with the frozen named export');
  assert.equal(typeof namespace?.renderHealthAlert, 'function');
  assert.deepEqual(FORBIDDEN, []);
});

// Break caught: a breach that drops the status, rational, spans, threshold,
// service/environment labels, logs URL, or window label, or that exceeds bounds.
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
  const payload = render(verdict, contextFixture({ note: 'extra context field', trace_id: 'synthetic-1' }));
  const text = assertBreachCarries(payload);
  assert.equal(text.includes('extra top-level field'), false, 'extras must not be echoed');
  assert.equal(text.includes('example.invalid/private'), false, 'extras must not be echoed');
});

// Break caught: an ok or unknown verdict rendered as an alert (fabrication).
test('ok/healthy verdicts never render', () => {
  const verdict = breachVerdict({
    verdict: 'ok', reason: 'healthy',
    counts: { valid: 5, red: 0, malformed: 0 },
    longest_red_span_ms: 0,
  });
  assert.equal(render(verdict, contextFixture()), null);
});

test('unknown/insufficient_data verdicts never render', () => {
  const verdict = breachVerdict({
    verdict: 'unknown', reason: 'insufficient_data',
    counts: { valid: 0, red: 0, malformed: 0 },
    longest_red_span_ms: 0,
  });
  assert.equal(render(verdict, contextFixture()), null);
});

test('mismatched or mistyped verdict/reason pairs fail closed', () => {
  for (const [verdict, reason] of [
    ['breach', 'healthy'], ['breach', 'insufficient_data'], ['ok', 'breach'], ['unknown', 'breach'],
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
    'counts', 'longest_red_span_ms', 'red_for_ms']) {
    const verdict = breachVerdict();
    delete verdict[key];
    cases.push([`missing ${key}`, verdict]);
  }
  cases.push(['null counts', breachVerdict({ counts: null })]);
  cases.push(['array counts', breachVerdict({ counts: [] })]);
  cases.push(['string counts', breachVerdict({ counts: '5/2' })]);
  cases.push(['null longest span', breachVerdict({ longest_red_span_ms: null })]);
  cases.push(['null threshold', breachVerdict({ red_for_ms: null })]);
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

test('bad counts fail closed', () => {
  const base = { valid: 5, red: 2, malformed: 1 };
  for (const [name, counts] of [
    ['negative valid', { ...base, valid: -1 }],
    ['negative red', { ...base, red: -1 }],
    ['negative malformed', { ...base, malformed: -1 }],
    ['red above valid', { ...base, valid: 2, red: 3 }],
    ['red one above valid', { ...base, valid: 5, red: 6 }],
    ['fractional valid', { ...base, valid: 5.5 }],
    ['fractional red', { ...base, red: 2.5 }],
    ['fractional malformed', { ...base, malformed: 1.5 }],
    ['string valid', { ...base, valid: '5' }],
    ['string red', { ...base, red: '2' }],
    ['string malformed', { ...base, malformed: '1' }],
    ['NaN valid', { ...base, valid: NaN }],
    ['NaN red', { ...base, red: NaN }],
    ['Infinity valid', { ...base, valid: Infinity }],
    ['missing valid', { red: 2, malformed: 1 }],
    ['missing red', { valid: 5, malformed: 1 }],
    ['missing malformed', { valid: 5, red: 2 }],
    ['null valid', { ...base, valid: null }],
  ]) {
    assert.equal(render(breachVerdict({ counts }), contextFixture()), null, `${name} must not render`);
  }
});

test('bad longest spans fail closed', () => {
  for (const [name, longest_red_span_ms] of [
    ['negative span', -1],
    ['fractional span', 130000.5],
    ['string span', '130000'],
    ['NaN span', NaN],
    ['Infinity span', Infinity],
    ['null span', null],
    ['boolean span', true],
  ]) {
    assert.equal(render(breachVerdict({ longest_red_span_ms }), contextFixture()), null,
      `${name} must not render`);
  }
});

test('bad thresholds fail closed', () => {
  for (const [name, red_for_ms] of [
    ['zero threshold', 0],
    ['above cap', 3600001],
    ['negative threshold', -1],
    ['fractional threshold', 120000.5],
    ['string threshold', '120000'],
    ['NaN threshold', NaN],
    ['Infinity threshold', Infinity],
    ['null threshold', null],
    ['boolean threshold', true],
  ]) {
    assert.equal(render(breachVerdict({ red_for_ms }), contextFixture()), null,
      `${name} must not render`);
  }
});

// Break caught: a contradictory span below the threshold rendered as a breach,
// or an off-by-one at the boundary (coherence is >=, never >).
test('span below threshold fails closed', () => {
  assert.equal(render(breachVerdict({
    longest_red_span_ms: 119999, red_for_ms: 120000,
  }), contextFixture()), null, 'span one ms below threshold must not render');
  assert.equal(render(breachVerdict({
    longest_red_span_ms: 0, red_for_ms: 120000,
  }), contextFixture()), null, 'zero span below threshold must not render');
});

test('span exactly equal to threshold renders', () => {
  const payload = render(breachVerdict({
    longest_red_span_ms: 120000, red_for_ms: 120000,
  }), contextFixture());
  assertBreachCarries(payload, { longest: '120000', threshold: '120000' });
});

test('threshold boundaries 1 and 3600000 render when coherent', () => {
  const tiny = render(breachVerdict({
    longest_red_span_ms: 1, red_for_ms: 1,
  }), contextFixture());
  assertBreachCarries(tiny, { longest: '1', threshold: '1' });
  const huge = render(breachVerdict({
    longest_red_span_ms: 3600000, red_for_ms: 3600000,
  }), contextFixture());
  assertBreachCarries(huge, { longest: '3600000', threshold: '3600000' });
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

test('host-less logs URLs fail closed', () => {
  for (const logsUrl of ['https://?x=1', 'https://#f', 'https://:8080/x']) {
    assert.equal(render(breachVerdict(), contextFixture({ logsUrl })), null,
      `logsUrl ${JSON.stringify(logsUrl)} must not render`);
  }
});

test('window label with surrounding spaces but content renders verbatim', () => {
  const windowLabel = '  health window 12:00-12:05 UTC  ';
  const payload = render(breachVerdict(), contextFixture({ windowLabel }));
  assertBreachCarries(payload, { windowLabel });
});

// Break caught: unbounded output for adversarial strings, dropped short fields
// when truncating, or nondeterministic truncation across identical calls.
test('adversarial long window label stays within bounds without losing the logs URL', () => {
  const contextArg = contextFixture({ windowLabel: `window ${LONG}` });
  const first = render(breachVerdict(), contextArg);
  assert.notEqual(first, null, 'oversized label must truncate, not fail closed');
  assert.deepEqual(Object.keys(first), ['content', 'embeds']);
  assertDiscordBounds(first);
  const text = JSON.stringify(first);
  assert.equal(text.includes('…'), true, 'oversized label must truncate with an ellipsis marker');
  assertCarriesLogsUrl(first, LOGS_URL, 'logs URL must survive label truncation');
  assert.equal(text.includes('2/5'), true, 'rational must survive label truncation');
  assert.equal(text.includes('130000'), true, 'span must survive label truncation');
  assert.equal(text.includes('120000'), true, 'threshold must survive label truncation');
  assert.equal(JSON.stringify(render(breachVerdict(), contextFixture({ windowLabel: `window ${LONG}` }))), text,
    'label truncation must be deterministic');
});

test('adversarial long logs URL stays within bounds without losing the window label', () => {
  const logsUrl = `https://logs.example.invalid/${'L'.repeat(2000)}`;
  const first = render(breachVerdict(), contextFixture({ logsUrl }));
  assert.notEqual(first, null, 'oversized URL must truncate, not fail closed');
  assert.deepEqual(Object.keys(first), ['content', 'embeds']);
  assertDiscordBounds(first);
  const text = JSON.stringify(first);
  assert.equal(text.includes('…'), true, 'oversized URL must truncate with an ellipsis marker');
  assertLogsHostSurvives(first, 'logs.example.invalid', 'logs host must survive URL truncation');
  assert.equal(text.includes(WINDOW_LABEL), true, 'window label must survive URL truncation');
  assert.equal(text.includes('2/5'), true, 'rational must survive URL truncation');
  const second = render(breachVerdict(), contextFixture({ logsUrl }));
  assert.equal(JSON.stringify(second), text, 'URL truncation must be deterministic');
});

test('combined worst case stays within bounds via truncation', () => {
  const logsUrl = `https://logs.example.invalid/${'L'.repeat(2000)}`;
  const windowLabel = `window ${'W'.repeat(5000)}`;
  const first = render(breachVerdict(), contextFixture({ logsUrl, windowLabel }));
  assert.notEqual(first, null, 'combined worst case must render within bounds, not fail closed');
  assertDiscordBounds(first);
  const text = JSON.stringify(first);
  assert.equal(text.includes('2/5'), true, 'rational must survive combined truncation');
  assert.equal(text.includes('130000'), true, 'span must survive combined truncation');
  assert.equal(text.includes('120000'), true, 'threshold must survive combined truncation');
  assertLogsHostSurvives(first, 'logs.example.invalid', 'logs host must survive combined truncation');
  const second = render(breachVerdict(), contextFixture({ logsUrl, windowLabel }));
  assert.equal(JSON.stringify(second), text, 'combined render must be deterministic');
});

// Break caught: truncation splitting non-BMP pairs. Emoji straddling every
// cap (label, logs URL) must not emit lone surrogates.
test('emoji straddling truncation caps emits no lone surrogates', () => {
  const bell = '🔔';
  const contextArg = contextFixture({
    logsUrl: `https://logs.example.invalid/${bell.repeat(600)}`,
    windowLabel: bell.repeat(600),
  });
  const payload = render(breachVerdict(), contextArg);
  assert.notEqual(payload, null, 'emoji input must truncate, not fail closed');
  assertDiscordBounds(payload);
  assertNoLoneSurrogates(payload, 'payload must carry no lone surrogates');
  const text = JSON.stringify(payload);
  assert.equal(text.includes('2/5'), true, 'rational must survive emoji truncation');
  assertLogsHostSurvives(payload, 'logs.example.invalid', 'logs host must survive emoji truncation');
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
  verdict.counts.secret = CANARIES[0];
  verdict.counts.note = CANARIES[2];
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
  const contextProxy = new Proxy(contextFixture(), {
    get() { throw new Error('synthetic boom'); },
  });
  assert.equal(render(breachVerdict(), contextProxy), null);
});

test('cyclic verdicts never throw and never produce an unbounded payload', () => {
  const cyclic = breachVerdict();
  cyclic.self = cyclic;
  const out = render(cyclic, contextFixture());
  if (out !== null) assertDiscordBounds(out);
});

// Break caught: a renderer that invents release identity. Health samples carry
// no release, so a payload rendered from SHA-free inputs must contain no
// 40-hex SHA. Correlation flows through the logs URL only.
test('rendered payload invents no release SHA', () => {
  const payload = render(breachVerdict(), contextFixture());
  assert.notEqual(payload, null, 'breach must render a payload');
  const text = JSON.stringify(payload);
  assert.equal(/[0-9a-fA-F]{40}/.test(text), false,
    'payload must not invent a release SHA from SHA-free inputs');
});

test('module source carries no webhook URL', async () => {
  assert.equal(loadFailure, undefined, 'implement render-health-alert.mjs first');
  const source = await readFile(MODULE_URL, 'utf8');
  assertNoWebhookHost(source, 'module must not contain a webhook host');
});

test('webhook URLs inside Markdown links are detected', () => {
  assert.throws(() => assertNoWebhookHost('[x](https://discord.com/api/webhooks/a/b)',
    'webhook in Markdown link must be detected'));
});

test('no ambient effects across the whole suite', () => {
  assert.deepEqual(FORBIDDEN, []);
});
