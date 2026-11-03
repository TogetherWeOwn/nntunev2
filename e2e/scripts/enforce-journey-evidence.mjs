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
 *   5. any skipped test outside the allow-list below (each entry binds to
 *      its owning spec file and is consumed once, so a same-titled skip
 *      from another file cannot take its budget slot);
 *   6. any failure or error (including errors="1" with failures="0" from a
 *      thrown non-expect Error, the dominant seam-helper failure mode);
 *   7. REQUIRE_QA_AUTH_TOKEN=true with QA_AUTH_TOKEN unset: the full journey
 *      needs the QA test-account seam token, so a configured run without it
 *      stays red instead of passing silently. The token is checked for
 *      presence after evidence is parsed, so the counts table is still
 *      published on a token-missing red run.
 *
 * Skip allow-list (each entry carries its reason). Only these skips are
 * accepted, and only up to MAX_ALLOWED_SKIPS of them:
 *   - "ingested track carries recording_mbids": test.fixme in
 *     e2e/tests/ingest-play-meter.spec.ts (recording_mbids contract pending).
 *   - nine parked legs (TWO-1635): every journey leg that needs a
 *     user-uploaded track skips by name in both modes — user NZB ingest is
 *     retired (TWO-1630) and the assertions return via admin-ingested tracks
 *     once admin ingest can seed a curated track on staging.
 *
 * Real-NZB mode (REAL_NZB_MODE=true, set by the workflow after it stages the
 * secret-held RAR-set fixture — the only journey mode since the checked-in
 * NZBs were deleted): the legs below skip by name because the RAR set can
 * feed no completable ingest (the pipeline has no RAR read) and hence no
 * streamable track until RAR read lands (one upload is one library row
 * until the per-file manifest lands). Live legs in this mode are signup,
 * library arrival, returning-user login, the provider 430 control and the
 * stream plan-limit refusal leg (the BYOP wizard and the ingest-refusal leg
 * retired with user ingest itself).
 * The extended allow-list is fixed here (each entry still binds to its owning
 * spec file and is consumed once), and the effective budget becomes the
 * extended list's length — MAX_ALLOWED_SKIPS keeps governing the non-staged
 * (offline) mode. Any failure, or any skip outside the active list, still
 * fails the gate in both modes. The provider 430 control is never allow-
 * listed: it must run.
 *
 * Environment:
 *   JUNIT_PATH            junit evidence to parse (default: test-results/junit.xml,
 *                         relative to the e2e/ working directory in CI).
 *   MAX_ALLOWED_SKIPS     small allow-list budget (default: 10, the fixme plus
 *                         the nine parked legs below; standard mode only —
 *                         real-NZB mode budgets the extended allow-list
 *                         below instead).
 *   REAL_NZB_MODE         'true' when the run staged the secret-held real NZB
 *                         (workflow sets it from the prepare step's output).
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
const MAX_ALLOWED_SKIPS = Number(process.env.MAX_ALLOWED_SKIPS ?? '10');
const REAL_NZB_MODE = (process.env.REAL_NZB_MODE ?? '') === 'true';
const REQUIRE_QA_AUTH_TOKEN = (process.env.REQUIRE_QA_AUTH_TOKEN ?? '') === 'true';
const SUMMARY_PATH = process.env.GITHUB_STEP_SUMMARY || null;

const TAG = '[journey-gate]';

// Allow-list: every skipped test's "classname name" must contain one entry's
// owning spec file AND match string, and each entry is consumed at most
// once. Reasons live next to the entries so the budget is auditable.
// TWO-1635 parked legs: user NZB ingest is retired (TWO-1630), so every leg
// that needs a user-uploaded track skips by name in both modes (the specs
// carry the matching guard). Un-park (remove the spec guard and the entry
// here) once admin ingest can seed a curated track on staging.
const USER_NZB_RETIRED_REASON =
  'user NZB ingest retired; reseeds when admin ingest can seed a curated track on staging';

const ALLOWED_SKIPS = [
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'ingested track carries recording_mbids',
    reason: 'test.fixme in e2e/tests/ingest-play-meter.spec.ts (recording_mbids contract pending)',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'NZB fixture ingests and appears in the library',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'ingested library persists across reload without re-import',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'streaming answers byte Ranges with 206 + Content-Range',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'usage meter increases after streaming and shows plan metadata',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'ingested audio actually plays in the browser',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'player UI seek moves the active audio position and playback continues',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'Media Session metadata reflects the playing ingested track',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'player-queue-order.spec.ts',
    match: 'NZB fixture ingests and appears in the library',
    reason: USER_NZB_RETIRED_REASON,
  },
  {
    file: 'player-queue-order.spec.ts',
    match: 'player queue advances in library-selection order',
    reason: USER_NZB_RETIRED_REASON,
  },
];

// Real-NZB-only skips: legs the secret-held RAR set cannot feed (no
// completable ingest until RAR read lands — the pipeline fails an RAR-only
// subset permanent at transcode before any library row — hence no
// streamable track either; one upload is one library row until the per-file
// manifest lands). Active only when REAL_NZB_MODE=true.
const REAL_NZB_ALLOWED_SKIPS = [
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'NZB fixture ingests and appears in the library',
    reason:
      'real-NZB mode: RAR set cannot complete ingest (no RAR read in the pipeline) until RAR read lands',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'ingested library persists across reload without re-import',
    reason: 'real-NZB mode: no completable ingest means no library row to reload',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'streaming answers byte Ranges with 206 + Content-Range',
    reason: 'real-NZB mode: RAR set yields no streamable track until RAR read lands',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'usage meter increases after streaming and shows plan metadata',
    reason: 'real-NZB mode: meter needs the deterministic stream the RAR set cannot feed',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'ingested audio actually plays in the browser',
    reason: 'real-NZB mode: no decodable staged audio until RAR read lands',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'player UI seek moves the active audio position and playback continues',
    reason: 'real-NZB mode: player seek needs the playback the RAR set cannot feed',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'Media Session metadata reflects the playing ingested track',
    reason: 'real-NZB mode: metadata read runs during playback the RAR set cannot feed',
  },
  {
    file: 'ingest-play-meter.spec.ts',
    match: 'over-limit staging account is refused with a plan-limit status',
    reason:
      'real-NZB mode: capped stream leg needs a pre-seeded streamable track ' +
      '(the ingest-refusal leg retired with user ingest itself)',
  },
  {
    file: 'player-queue-order.spec.ts',
    match: 'player queue advances in library-selection order',
    reason: 'real-NZB mode: queue order needs 2+ tracks; the RAR set stages one row',
  },
  {
    file: 'player-queue-order.spec.ts',
    match: 'NZB fixture ingests and appears in the library',
    reason: 'real-NZB mode: its ingest leg cannot complete either (no RAR read)',
  },
];

// Union: the nine parked legs already cover the same-titled RAR-set legs in
// both modes, so the real-NZB-only entries extend the standard list without
// doubling any (file, match) pair — every skip title stays consumable exactly
// once (11 entries: the fixme, the nine parked legs, and the capped-stream
// leg, which runs live in standard mode and skips only in real-NZB mode).
const ACTIVE_ALLOWED_SKIPS = REAL_NZB_MODE
  ? [
      ...ALLOWED_SKIPS,
      ...REAL_NZB_ALLOWED_SKIPS.filter(
        (extra) =>
          !ALLOWED_SKIPS.some(
            (base) => base.file === extra.file && base.match === extra.match,
          ),
      ),
    ]
  : ALLOWED_SKIPS;

// Standard mode budgets MAX_ALLOWED_SKIPS; real-NZB mode budgets the full
// extended allow-list (each entry still consumed at most once below).
const EFFECTIVE_BUDGET = REAL_NZB_MODE ? ACTIVE_ALLOWED_SKIPS.length : MAX_ALLOWED_SKIPS;

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

// 1. Config validation (workflow misconfiguration, not journey evidence).
if (!Number.isInteger(MAX_ALLOWED_SKIPS) || MAX_ALLOWED_SKIPS < 0) {
  fail(`MAX_ALLOWED_SKIPS must be a non-negative integer, got ${process.env.MAX_ALLOWED_SKIPS ?? '(unset)'}.`);
}

// Seam-token state (presence only; the value is never touched). The failure
// is reported after evidence is parsed so the counts table is still
// published on a token-missing red run.
const tokenMissing = REQUIRE_QA_AUTH_TOKEN && !process.env.QA_AUTH_TOKEN;
const tokenLine = '- seam token: missing (QA_AUTH_TOKEN unset)';

// 2. Evidence must exist and parse.
let xml;
try {
  xml = readFileSync(JUNIT_PATH, 'utf8');
} catch {
  summaryLines = [
    `- junit evidence: absent (${JUNIT_PATH})`,
    '',
    ...(tokenMissing ? [tokenLine, ''] : []),
  ];
  if (tokenMissing) {
    fail(
      'QA_AUTH_TOKEN is not set. The full journey requires the QA test-account ' +
        'seam token, so this configured run stays red instead of passing silently ' +
        '(secret names only — never values).',
    );
  }
  fail(
    `no junit evidence at ${JUNIT_PATH} on a configured run — refusing to pass ` +
      'without journey evidence.',
  );
}

const counts = parseCounts(xml);
if (!counts) {
  summaryLines = [
    `- junit evidence: unparseable (${JUNIT_PATH})`,
    '',
    ...(tokenMissing ? [tokenLine, ''] : []),
  ];
  if (tokenMissing) {
    fail(
      'QA_AUTH_TOKEN is not set. The full journey requires the QA test-account ' +
        'seam token, so this configured run stays red instead of passing silently ' +
        '(secret names only — never values).',
    );
  }
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
// Each allow-list entry binds to its owning spec file and is consumed at
// most once: a same-titled skip from another file cannot take its slot,
// and one entry cannot cover two skips.
const consumedAllowances = new Set();
const unknownSkips = skippedNames.filter((title) => {
  const idx = ACTIVE_ALLOWED_SKIPS.findIndex(
    (entry, i) => !consumedAllowances.has(i) && title.includes(entry.file) && title.includes(entry.match),
  );
  if (idx === -1) return true;
  consumedAllowances.add(idx);
  return false;
});

summaryLines = [
  '| total | ran | skipped | failed |',
  '| --- | --- | --- | --- |',
  `| ${tests} | ${ran} | ${skipped} | ${failed} |`,
  '',
  skippedNames.length > 0
    ? `Skipped (${skippedNames.length} listed in junit evidence):\n${skippedNames.map((n) => `- ${n}`).join('\n')}`
    : 'Skipped: none listed in junit evidence.',
  '',
  `Skip budget (${REAL_NZB_MODE ? 'real-NZB mode' : 'standard mode'}): at most ${EFFECTIVE_BUDGET} ` +
    `from the documented allow-list (${ACTIVE_ALLOWED_SKIPS.map((e) => `"${e.match}"`).join('; ')}).`,
  '',
  ...(tokenMissing ? [tokenLine, ''] : []),
];

if (tokenMissing) {
  fail(
    'QA_AUTH_TOKEN is not set. The full journey requires the QA test-account ' +
      'seam token, so this configured run stays red instead of passing silently ' +
      '(secret names only — never values).',
  );
}
if (tests === 0) fail('zero journeys ran (no test cases in junit evidence).');
if (ran === 0) {
  fail(`every journey skipped (0 ran, ${skipped} skipped) — staging evidence is empty.`);
}
if (skipped > EFFECTIVE_BUDGET) {
  fail(
    `skip count ${skipped} is above the allow-list budget of ${EFFECTIVE_BUDGET} ` +
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
