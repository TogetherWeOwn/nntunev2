#!/usr/bin/env node
/**
 * Offline self-check for `synthetic-two-track.nzb` (paired fixture build).
 *
 * Not a Playwright spec (so `e2e:list` and the traceability gate ignore it):
 * run with `node e2e/fixtures/two-track-self-check.mjs` from the repo root.
 * Exit 0 prints the two staged tracks; exit 1 names the missing/broken
 * track — a partial failure (e.g. file 2 edited to an unserved ID) fails
 * HERE by track name instead of surfacing later as a staging timeout.
 *
 * Pinned values are vendored from `crates/nntune-test-nntp/src/fixtures.rs`
 * (`default_corpus()`: message-IDs, payload byte counts, group) and the
 * single-track match query (the plain-title query synthetic.nzb's subject
 * derives via `derive_upload_query`; see the FIXMEd recording-mbids leg in
 * `e2e/tests/ingest-play-meter.spec.ts`). Keep in sync with those sources;
 * this file duplicates them on purpose so the fixture cannot silently drift
 * from the corpus it claims to reuse.
 */

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const NZB_PATH = path.resolve(here, 'synthetic-two-track.nzb');

/** Track label per <file> index — what failure messages name. */
const TRACK_LABELS = ['track 1 (sine-660)', 'track 2 (sine-440)'];

/**
 * Corpus-served segments: message-ID -> payload bytes before yEnc.
 * Vendored from `default_corpus()` in crates/nntune-test-nntp/src/fixtures.rs.
 */
const CORPUS_SEGMENTS = new Map([
  ['<multi-660-part1-of-3@nntune.test>', 575],
  ['<multi-660-part2-of-3@nntune.test>', 575],
  ['<multi-660-part3-of-3@nntune.test>', 574],
  ['<single-yenc-001@nntune.test>', 1004],
]);

/**
 * Exact ordered segment IDs per track. The per-segment loop below only
 * checks that each ID is corpus-served, so without this a truncated file 1
 * (e.g. part 3 deleted) or a file 2 retargeted at a 660 Hz part still prints
 * OK — both mutants exited 0 before this pin existed. Each file's IDs must
 * equal its entry here, otherwise the failure names the broken track.
 */
const EXPECTED_TRACK_SEGMENTS = [
  [
    '<multi-660-part1-of-3@nntune.test>',
    '<multi-660-part2-of-3@nntune.test>',
    '<multi-660-part3-of-3@nntune.test>',
  ],
  ['<single-yenc-001@nntune.test>'],
];

const CORPUS_GROUP = 'alt.binaries.nntune.test';

/** Single-track match query the first file's subject must still derive. */
const SINGLE_TRACK_QUERY = 'NNTune-Fixture sine-660';

function unescape(s) {
  return s
    .replaceAll('&lt;', '<')
    .replaceAll('&gt;', '>')
    .replaceAll('&quot;', '"')
    .replaceAll('&apos;', "'")
    .replaceAll('&amp;', '&');
}

/** Minimal port of `derive_upload_query` (workers/ingest/src/matching.rs). */
function deriveUploadQuery(subjects) {
  const first = subjects[0];
  if (first === undefined) return null;
  const beforeQuote = first.split('"')[0] ?? first;
  const spaced = [...beforeQuote].map((c) => (c === '.' || c === '_' ? ' ' : c)).join('');
  let candidate = spaced.trim().replace(/-+$/, '').trim();
  for (;;) {
    const trimmed = candidate.trimEnd();
    const last = trimmed.slice(-1);
    let head = null;
    if (last === ']') {
      const i = trimmed.lastIndexOf('[');
      head = i >= 0 ? trimmed.slice(0, i) : null;
    } else if (last === ')') {
      const i = trimmed.lastIndexOf('(');
      head = i >= 0 ? trimmed.slice(0, i) : null;
    } else {
      const i = trimmed.search(/\s[^\s]+$/);
      if (i >= 0 && trimmed.slice(i + 1).toLowerCase() === 'yenc') head = trimmed.slice(0, i);
    }
    if (head === null) break;
    const next = head.trim().replace(/-+$/, '').trim();
    if (next === candidate) break;
    candidate = next;
    if (candidate === '') break;
  }
  const FILE_TYPES = new Set(
    ['flac', 'mp3', 'ogg', 'opus', 'm4a', 'wav', 'rar', 'zip', '7z', 'par2', 'nfo'],
  );
  const m = candidate.match(/^(.*)\s([^\s]+)$/);
  if (m && FILE_TYPES.has((m[2] ?? '').toLowerCase())) candidate = (m[1] ?? '').trim();
  return candidate === '' ? null : candidate;
}

function parseNzb(text) {
  const blocks = [];
  let rest = text;
  for (;;) {
    const i = rest.indexOf('<file');
    if (i < 0) break;
    rest = rest.slice(i);
    const end = rest.indexOf('</file>');
    if (end < 0) break;
    blocks.push(rest.slice(0, end));
    rest = rest.slice(end);
  }
  if (blocks.length === 0) blocks.push(text);
  return blocks.map((block) => {
    const subject = /<file[^>]*subject="([^"]*)"/.exec(block)?.[1] ?? '';
    const groups = [...block.matchAll(/<group>([\s\S]*?)<\/group>/g)].map((m) =>
      unescape((m[1] ?? '').trim()),
    );
    const segments = [...block.matchAll(/<segment\s([^>]*)>([\s\S]*?)<\/segment>/g)].map((m) => {
      const tag = m[1] ?? '';
      const bytes = Number(/bytes="(\d+)"/.exec(tag)?.[1] ?? NaN);
      const number = Number(/number="(\d+)"/.exec(tag)?.[1] ?? NaN);
      const raw = unescape((m[2] ?? '').trim());
      return { bytes, number, id: raw.startsWith('<') ? raw : `<${raw}>` };
    });
    return { subject: unescape(subject), groups, segments };
  });
}

const failures = [];
const fail = (msg) => failures.push(`[two-track-self-check] ${msg}`);

let text;
try {
  text = readFileSync(NZB_PATH, 'utf8');
} catch {
  fail(
    `fixture missing at e2e/fixtures/synthetic-two-track.nzb — ` +
      `the staging leg falls back to the single-track NZB and fails with the single-track precondition`,
  );
  console.error(failures.join('\n'));
  process.exit(1);
}

if (text.includes('<!DOCTYPE')) {
  fail('fixture carries a DOCTYPE — the production NZB parser refuses every DOCTYPE (XXE hardening)');
}

const files = parseNzb(text);
if (files.length !== 2) {
  fail(`fixture carries ${files.length} <file> entr(y/ies), expected exactly 2 distinct tracks`);
}

const subjects = [];
files.forEach((file, fi) => {
  const label = TRACK_LABELS[fi] ?? `track ${fi + 1} (unexpected extra file)`;
  subjects.push(file.subject);
  if (file.segments.length === 0) {
    fail(`${label} declares no <segment>s — ingest would reject the file`);
    return;
  }
  const numbers = file.segments.map((s) => s.number);
  const expected = file.segments.map((_, i) => i + 1);
  if (JSON.stringify(numbers) !== JSON.stringify(expected)) {
    fail(`${label} segment numbers [${numbers}] are not contiguous 1..=N in order`);
  }
  const expectedIds = EXPECTED_TRACK_SEGMENTS[fi];
  const actualIds = file.segments.map((s) => s.id);
  if (expectedIds !== undefined && JSON.stringify(actualIds) !== JSON.stringify(expectedIds)) {
    fail(
      `${label} carries segment IDs [${actualIds.join(', ') || '(none)'}], expected ` +
        `[${expectedIds.join(', ')}] for this track — truncated or cross-track IDs fail here by track name`,
    );
  }
  for (const seg of file.segments) {
    const corpusBytes = CORPUS_SEGMENTS.get(seg.id);
    if (corpusBytes === undefined) {
      fail(
        `${label} references ${seg.id} which the default synthetic corpus does not serve — ` +
          `staging ingest would 430 on this part (see default_corpus() in crates/nntune-test-nntp/src/fixtures.rs)`,
      );
      continue;
    }
    if (seg.bytes !== corpusBytes) {
      fail(`${label} segment ${seg.id} declares bytes=${seg.bytes} but the corpus payload is ${corpusBytes} bytes`);
    }
    if (!seg.id.endsWith('@nntune.test>')) {
      fail(`${label} segment ${seg.id} is not synthetic @nntune.test data`);
    }
  }
  if (!file.groups.includes(CORPUS_GROUP)) {
    fail(`${label} does not list group ${CORPUS_GROUP} (groups: ${file.groups.join(', ') || '(none)'})`);
  }
});

if (files.length === 2 && files[0].subject === files[1].subject) {
  fail('both files carry the same subject — the two tracks are not distinct');
}

// File 1 must keep deriving the single-track query: both fixtures must
// present the same match query so any future mirror seeding applies alike.
const query = deriveUploadQuery(subjects);
if (query !== SINGLE_TRACK_QUERY) {
  fail(
    `first-file subject derives query ${JSON.stringify(query)}, expected single-track query ` +
      `${JSON.stringify(SINGLE_TRACK_QUERY)} — meter-fixture query parity breaks`,
  );
}

if (failures.length > 0) {
  console.error(failures.join('\n'));
  process.exit(1);
}

console.log(
  `[two-track-self-check] OK: 2 tracks staged through the ingest path\n` +
    `- ${TRACK_LABELS[0]}: 3 segments, query ${JSON.stringify(query)}\n` +
    `- ${TRACK_LABELS[1]}: 1 segment, distinct subject`,
);
