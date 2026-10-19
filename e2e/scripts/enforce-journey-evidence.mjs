#!/usr/bin/env node
/**
 * Journey evidence gate for the staging E2E workflow (`.github/workflows/e2e-staging.yml`).
 *
 * A Playwright run that skips every journey still exits 0, so "staging passed"
 * would mean nothing. This gate parses the junit evidence the run produced and
 * fails closed unless real journeys ran. It also publishes the ran / skipped /
 * failed counts to the job summary on every configured run (pass or fail),
 * because green runs upload no artifacts.
 *
 * Fail-closed violations (each exits 1 with a names-only message):
 *   1. junit evidence absent or unparseable on a configured run;
 *   2. zero journeys ran (no test cases at all);
 *   3. every journey skipped (ran == 0);
 *   4. skip count above MAX_ALLOWED_SKIPS;
 *   5. any skipped test outside the allow-list below;
 *   6. any failure or error;
 *   7. REQUIRE_QA_AUTH_TOKEN=true with QA_AUTH_TOKEN unset: the full journey
 *      needs the QA test-account seam token, so a configured run without it
 *      stays red instead of passing silently.
 *
 * Skip allow-list (each entry carries its reason). Only these skips are
 * accepted, and only up to MAX_ALLOWED_SKIPS of them:
 *   - "player queue advances in library-selection order": test.fixme in
 *     e2e/tests/player-queue-order.spec.ts (player queue order not implemented).
 *   - "ingested track carries recording_mbids": test.fixme in
 *     e2e/tests/ingest-play-meter.spec.ts (recording_mbids contract pending).
 *
 * Environment:
 *   JUNIT_PATH            junit evidence to parse (default: test-results/junit.xml,
 *                         relative to the e2e/ working directory in CI).
 *   MAX_ALLOWED_SKIPS     small allow-list budget (default: 2, the two fixmes above).
 *   REQUIRE_QA_AUTH_TOKEN 'true' requires QA_AUTH_TOKEN to be set (journey job only;
 *                         the synthetic smoke does not use the account seam).
 *   QA_AUTH_TOKEN         checked for presence only — its value is never read,
 *                         printed, or written anywhere by this script.
 *   GITHUB_STEP_SUMMARY   appended with the counts table when set (CI provides it).
 *
 * Only static test titles (classname + name) and counts ever reach stdout or
 * the summary — never failure messages, which can carry filled secrets.
 */

import { appendFileSync, readFileSync } from 'node:fs';

const JUNIT_PATH = process.env.JUNIT_PATH ?? 'test-results/junit.xml';
const MAX_ALLOWED_SKIPS = Number(process.env.MAX_ALLOWED_SKIPS ?? '2');
const REQUIRE_QA_AUTH_TOKEN = (process.env.REQUIRE_QA_AUTH_TOKEN ?? '') === 'true';
const SUMMARY_PATH = process.env.GITHUB_STEP_SUMMARY || null;

const TAG = '[journey-gate]';

// Allow-list: every skipped test's "classname name" must contain one entry's
// match string. Reasons live next to the entries so the budget is auditable.
const ALLOWED_SKIPS = [
  {
    match: 'player queue advances in library-selection order',
    reason: 'test.fixme in e2e/tests/player-queue-order.spec.ts (player queue order not implemented)',
  },
  {
    match: 'ingested track carries recording_mbids',
    reason: 'test.fixme in e2e/tests/ingest-play-meter.spec.ts (recording_mbids contract pending)',
  },
];

function fail(message) {
  process.stdout.write(`${TAG} FAIL: ${message}\n`);
  appendSummary({ verdict: `FAIL: ${message}` });
  process.exit(1);
}

function attr(tag, name) {
  // Anchor on whitespace: a bare `name=` pattern would also match the tail of
  // `classname="..."`. Attributes in junit tags are space-separated.
  const match = tag.match(new RegExp(`(?:\\s)${name}\\s*=\\s*"([^"]*)"`));
  return match ? match[1] : null;
}

function parseCounts(xml) {
  const suites = [...xml.matchAll(/<testsuite\b[^>]*>/g)].map((m) => m[0]);
  if (suites.length === 0) return null;
  let tests = 0;
  let failures = 0;
  let errors = 0;
  let skipped = 0;
  for (const suite of suites) {
    const num = (name) => {
      const raw = attr(suite, name);
      if (raw === null) return 0;
      const parsed = Number(raw);
      return Number.isInteger(parsed) && parsed >= 0 ? parsed : NaN;
    };
    const t = num('tests');
    const f = num('failures');
    const e = num('errors');
    const s = num('skipped');
    if ([t, f, e, s].some((n) => Number.isNaN(n))) return null;
    tests += t;
    failures += f;
    errors += e;
    skipped += s;
  }
  return { tests, failures, errors, skipped };
}

function parseSkippedNames(xml) {
  const names = [];
  // Match each testcase element on its own: self-closing tags first (they
  // cannot carry a <skipped> child), then paired tags with their own body. A
  // single lazy match from one <testcase> to the next </testcase> would span
  // several elements and attribute the skip to the wrong test.
  const element = /<testcase\b[^>]*\/>|<testcase\b[^>]*>[\s\S]*?<\/testcase>/g;
  for (const m of xml.matchAll(element)) {
    const text = m[0];
    if (!/<skipped[\s>/]/.test(text)) continue;
    const openTag = text.slice(0, text.indexOf('>') + 1);
    const classname = attr(openTag, 'classname');
    const name = attr(openTag, 'name') ?? '(unnamed test)';
    names.push(classname ? `${classname} ${name}` : name);
  }
  return names;
}

// Summary carries only counts and static test titles — never failure text.
let summaryLines = null;

function appendSummary({ verdict }) {
  const lines = summaryLines ?? [];
  const out = ['## Journey evidence', '', ...lines, '', verdict, ''];
  const text = out.join('\n');
  if (SUMMARY_PATH) {
    try {
      appendFileSync(SUMMARY_PATH, text);
    } catch {
      // The summary is evidence, not the gate: a failed append must not mask the verdict.
    }
  }
  process.stdout.write(`${text}\n`);
}

// 1. Seam-token prerequisite (presence only; the value is never touched).
if (REQUIRE_QA_AUTH_TOKEN && !process.env.QA_AUTH_TOKEN) {
  summaryLines = ['- seam token: missing (QA_AUTH_TOKEN unset)', ''];
  fail(
    'QA_AUTH_TOKEN is not set. The full journey requires the QA test-account ' +
      'seam token, so this configured run stays red instead of passing silently ' +
      '(secret names only — never values).',
  );
}

if (!Number.isInteger(MAX_ALLOWED_SKIPS) || MAX_ALLOWED_SKIPS < 0) {
  fail(`MAX_ALLOWED_SKIPS must be a non-negative integer, got ${process.env.MAX_ALLOWED_SKIPS ?? '(unset)'}.`);
}

// 2. Evidence must exist and parse.
let xml;
try {
  xml = readFileSync(JUNIT_PATH, 'utf8');
} catch {
  summaryLines = [`- junit evidence: absent (${JUNIT_PATH})`, ''];
  fail(
    `no junit evidence at ${JUNIT_PATH} on a configured run — refusing to pass ` +
      'without journey evidence.',
  );
}

const counts = parseCounts(xml);
if (!counts) {
  summaryLines = [`- junit evidence: unparseable (${JUNIT_PATH})`, ''];
  fail(
    `could not parse testsuite counts from ${JUNIT_PATH} — refusing to pass ` +
      'without legible journey evidence.',
  );
}

const { tests, failures, errors } = counts;
const { skipped } = counts;
const failed = failures + errors;
const ran = tests - skipped;
const skippedNames = parseSkippedNames(xml);
const unknownSkips = skippedNames.filter(
  (title) => !ALLOWED_SKIPS.some((entry) => title.includes(entry.match)),
);

summaryLines = [
  '| total | ran | skipped | failed |',
  '| --- | --- | --- | --- |',
  `| ${tests} | ${ran} | ${skipped} | ${failed} |`,
  '',
  skippedNames.length > 0
    ? `Skipped (${skippedNames.length} listed in junit evidence):\n${skippedNames.map((n) => `- ${n}`).join('\n')}`
    : 'Skipped: none listed in junit evidence.',
  '',
  `Skip budget: at most ${MAX_ALLOWED_SKIPS} from the documented allow-list ` +
    `(${ALLOWED_SKIPS.map((e) => `"${e.match}"`).join('; ')}).`,
  '',
];

if (tests === 0) fail('zero journeys ran (no test cases in junit evidence).');
if (ran === 0) {
  fail(`every journey skipped (0 ran, ${skipped} skipped) — staging evidence is empty.`);
}
if (skipped > MAX_ALLOWED_SKIPS) {
  fail(
    `skip count ${skipped} is above the allow-list budget of ${MAX_ALLOWED_SKIPS} ` +
      '(every extra skip needs a documented reason).',
  );
}
if (unknownSkips.length > 0) {
  fail(
    `${unknownSkips.length} skipped test(s) outside the allow-list:\n` +
      unknownSkips.map((n) => `- ${n}`).join('\n'),
  );
}
if (skippedNames.length < skipped) {
  fail(
    `${skipped - skippedNames.length} skipped test(s) could not be identified in the ` +
      'junit evidence — refusing to pass on unattributed skips.',
  );
}
if (failed > 0) fail(`${failed} failing test(s) — the journey is red.`);

appendSummary({ verdict: 'PASS: journey evidence complete.' });
