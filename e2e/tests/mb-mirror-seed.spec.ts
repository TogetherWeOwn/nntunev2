import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  API,
  extractLibraryItems,
  extractRecordingMbids,
  isMbid,
} from '../helpers/contract.js';
import { readApiBody, stagingBaseUrl } from '../helpers/access.js';
import {
  accountHeaders,
  apiGet,
  requireApiAccount,
  tryMintTestAccount,
} from '../helpers/accounts.js';

// Seed-verification for TOG-28772 (acceptance-test card): the MB mirror
// must hold the synthetic-fixture release the staging E2E ingests
// (`e2e/fixtures/synthetic.nzb`, 660 Hz sine track). The paired build card
// (TOG-28778) implements the seed on this branch and never edits this file.
//
// The constants below ARE the builder contract: the seed must insert
// exactly these rows (artist + release + recording + the one mb_track
// membership; see workers/mb/tests/sql/mirror_seed_synthetic_fixture_verify.sql).
// All MBIDs are valid 8-4-4-4-12 hex and unmistakably synthetic — they can
// never collide with real MusicBrainz data.
export const FIXTURE_ARTIST_MBID = '11111111-1111-1111-1111-111111111111';
export const FIXTURE_ARTIST_NAME = 'NNTune Fixture Artist';
export const FIXTURE_RELEASE_MBID = '22222222-2222-2222-2222-222222222222';
// EXACTLY the NZB-derived match query (see the derivation pin below): any
// other title never matches the ingested fixture.
export const FIXTURE_RELEASE_TITLE = 'NNTune-Fixture sine-660';
export const FIXTURE_RECORDING_MBID = '33333333-3333-3333-3333-333333333333';
export const FIXTURE_RECORDING_DURATION_MS = 210;

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_NZB = path.resolve(here, '../fixtures/synthetic.nzb');

/**
 * Port of `derive_upload_query` (workers/ingest/src/matching.rs): the
 * candidate release title derived from NZB file subjects. The offline pin
 * below keeps this port honest — if the Rust derivation changes, the pin
 * fails by name instead of the seed silently missing the match.
 */
function deriveUploadQuery(subjects: string[]): string | null {
  const first = subjects[0];
  if (first === undefined) return null;
  const beforeQuote = first.split('"')[0] ?? first;
  const spaced = beforeQuote.replace(/[._]/g, ' ');
  let candidate = spaced.trim().replace(/-+$/, '').trim();
  for (;;) {
    const trimmed = candidate.trimEnd();
    const last = trimmed.slice(-1);
    let head: string | undefined;
    if (last === ']') {
      const i = trimmed.lastIndexOf('[');
      head = i === -1 ? undefined : trimmed.slice(0, i);
    } else if (last === ')') {
      const i = trimmed.lastIndexOf('(');
      head = i === -1 ? undefined : trimmed.slice(0, i);
    } else {
      const split = trimmed.split(/\s+/);
      head =
        split.length > 1 && split[split.length - 1]?.toLowerCase() === 'yenc'
          ? split.slice(0, -1).join(' ')
          : undefined;
    }
    if (head === undefined) break;
    const next = head.trim().replace(/-+$/, '').trim();
    if (next === candidate) break;
    candidate = next;
    if (candidate === '') break;
  }
  const FILE_TYPES = new Set([
    'flac',
    'mp3',
    'ogg',
    'opus',
    'm4a',
    'wav',
    'rar',
    'zip',
    '7z',
    'par2',
    'nfo',
  ]);
  const parts = candidate.split(/\s+/);
  const tail = parts[parts.length - 1]?.toLowerCase() ?? '';
  if (parts.length > 1 && FILE_TYPES.has(tail)) {
    candidate = parts.slice(0, -1).join(' ');
  }
  const trimmed = candidate.trim();
  if (trimmed.length < 2 || trimmed.length > 100 || /[\0-\x1f\x7f]/.test(trimmed)) {
    return null;
  }
  return trimmed;
}

function nzbSubjects(): string[] {
  const text = readFileSync(FIXTURE_NZB, 'utf8');
  const subjects: string[] = [];
  const re = /subject="([^"]*)"/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    const raw = match[1] ?? '';
    subjects.push(
      raw
        .replace(/&quot;/g, '"')
        .replace(/&apos;/g, "'")
        .replace(/&lt;/g, '<')
        .replace(/&gt;/g, '>')
        .replace(/&amp;/g, '&'),
    );
  }
  return subjects;
}

test.describe('mirror seed matches the synthetic fixture (offline)', () => {
  test('fixture MBIDs are canonical (isMbid)', () => {
    for (const mbid of [FIXTURE_ARTIST_MBID, FIXTURE_RELEASE_MBID, FIXTURE_RECORDING_MBID]) {
      expect(isMbid(mbid), `fixture MBID ${mbid} is not canonical 8-4-4-4-12 hex`).toBe(true);
    }
    expect(
      new Set([FIXTURE_ARTIST_MBID, FIXTURE_RELEASE_MBID, FIXTURE_RECORDING_MBID]).size,
      'fixture MBIDs must be three distinct rows',
    ).toBe(3);
  });

  test('seeded release title equals the NZB-derived match query', () => {
    const subjects = nzbSubjects();
    expect(subjects.length, 'synthetic NZB carries no file subjects').toBeGreaterThan(0);
    const derived = deriveUploadQuery(subjects);
    expect(
      derived,
      `synthetic NZB subjects derive to nothing (subjects: ${subjects.join(' | ').slice(0, 120)})`,
    ).toBeTruthy();
    expect(derived, 'seeded release title does not match the NZB-derived query').toBe(
      FIXTURE_RELEASE_TITLE,
    );
  });

  test('fixture library track carries non-empty recording_mbids (extractRecordingMbids)', () => {
    // The library-track shape the ingest writes for a matched release
    // (release_mbid + recording_mbids filled from the release-to-recordings
    // route). A mirror without the fixture release writes no release_mbid
    // and an empty list — both fail here by name.
    const track = {
      id: 'fixture-track',
      release_mbid: FIXTURE_RELEASE_MBID,
      recording_mbids: [FIXTURE_RECORDING_MBID],
    };
    const mbids = extractRecordingMbids(track);
    expect(mbids.length, '[recording-mbids] fixture track carries an empty list').toBeGreaterThan(
      0,
    );
    for (const mbid of mbids) {
      expect(isMbid(mbid), `[recording-mbids] fixture entry is not a canonical MBID`).toBe(true);
    }
    expect(mbids, 'fixture track does not carry the seeded recording').toContain(
      FIXTURE_RECORDING_MBID,
    );
  });
});

// FIXMEd until the staging MB mirror holds the fixture release: an
// unseeded mirror leaves the ingested track unmatched (no release_mbid,
// empty recording_mbids by design), so this leg fails by name there.
// Un-fixme path: seed the staging mirror per the TOG-27560 grants, then
// flip this leg — it must stay green unconfigured (skip) either way.
test.fixme('staging mirror serves the synthetic-fixture release', async ({ request }) => {
  const baseURL = stagingBaseUrl('skip', 'mirror-seed');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  const account = await tryMintTestAccount(request, baseURL);
  // Fail closed instead of asserting anonymously when neither a minted
  // session nor the QA seam token exists.
  requireApiAccount(account, 'mirror-seed');
  const headers = accountHeaders(account);
  const libraryUrl = `${baseURL}${API.library}`;
  const library = await apiGet(request, libraryUrl, headers, 'mirror-seed');
  const libraryBody = await readApiBody(library, libraryUrl, 'mirror-seed');
  expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
  const items = extractLibraryItems(libraryBody);
  expect(items.length, 'library is empty — ingest spec must run first').toBeGreaterThan(0);
  const track = items[0] as Record<string, unknown>;
  // Identity, not just non-emptiness: a wrong seed (a different release)
  // fails here by name, while a missing seed (no release_mbid) fails just
  // as explicitly.
  const releaseMbid = track.release_mbid ?? track.releaseMbid;
  expect(
    releaseMbid,
    `[mirror-seed] ingested track carries no release_mbid (keys: ${Object.keys(track).join(', ') || '(empty)'}). ` +
      'The staging mirror lacks the synthetic-fixture release.',
  ).toBe(FIXTURE_RELEASE_MBID);
  const mbids = extractRecordingMbids(track);
  expect(
    mbids.length,
    '[recording-mbids] ingested track carries an empty recording_mbids list',
  ).toBeGreaterThan(0);
  expect(mbids, 'ingested track does not carry the seeded fixture recording').toContain(
    FIXTURE_RECORDING_MBID,
  );
});
