import { test, expect } from '@playwright/test';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { API, TESTIDS, extractLibraryItems } from '../helpers/contract.js';
import { readApiBody, stagingBaseUrl } from '../helpers/access.js';
import {
  accountHeaders,
  annotateSignupPath,
  apiGet,
  applyAccountToPage,
  completeByopWizard,
  requireApiAccount,
  submitPublicUiSignup,
  tryMintTestAccount,
  waitForHydration,
} from '../helpers/accounts.js';
import {
  SECOND_SINGLE_TRACK_FIXTURE_FILE,
  SINGLE_TRACK_FIXTURE_FILE,
  resolveTwoTrackFixture,
  twoTrackPreconditionFailure,
} from '../helpers/two-track-fixture.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURES_DIR = path.resolve(here, '../fixtures');

// Test-first acceptance for the two-track synthetic fixture (roadmap M1/M3):
// after running the fixture, the staging library must list ≥2 synthetic
// tracks, and a missing second track must fail BY NAME (the precondition
// message), not by timeout. The leg stages two single-file uploads
// (synthetic.nzb plus synthetic-second-track.nzb) — two uploads are two
// ingest jobs and two library rows per idempotency key, on current ingest
// and under the future per-file manifest (ADR 0002) alike. If the second
// upload is missing, the leg stages the single-track fallback and FAILS
// with the documented single-track signature — that red IS the signal.
// The two-file synthetic-two-track.nzb is never staged (one upload is one
// row on current ingest); it stays pinned by the offline self-check gate
// as the reference two-file shape.
//
// Ingest path only (/library/import → /api/library, fresh test account per
// run): this spec never touches the no-auth demo-audio route
// (/api/demo/audio) or its `second-signal`/`third-light` track ids — those
// route checks live in demo-audio-contract.spec.ts.
//
// NO file-level skip here by design: the offline describe below pins the
// fixture contract on every PR (via `npm run test:unit`) with no staging,
// while the live leg skips green when STAGING_BASE_URL is unset and fails
// closed once it is configured. The live leg needs a real browser (BYOP
// wizard plus import UI), so unconfigured runs launch chromium to reach the
// skip — CI installs it before `test:unit` (see the e2e-lint job).

test.describe('two-track fixture precondition (offline fixtures)', () => {
  const candidates = {
    singleTrack: '/fixtures/synthetic.nzb',
    secondSingle: '/fixtures/synthetic-second-track.nzb',
  };

  test('both single-file NZBs present stages both in order', () => {
    const staging = resolveTwoTrackFixture(
      candidates,
      (p) => p === candidates.singleTrack || p === candidates.secondSingle,
    );
    expect(staging.staged).toEqual([candidates.singleTrack, candidates.secondSingle]);
    expect(staging.isTwoTrack).toBe(true);
  });

  test('second single-file NZB absent falls back to the single-track NZB', () => {
    const staging = resolveTwoTrackFixture(candidates, (p) => p === candidates.singleTrack);
    expect(staging.staged).toEqual([candidates.singleTrack]);
    expect(staging.isTwoTrack).toBe(false);
  });

  test('neither NZB present fails naming both expected paths', () => {
    expect(() => resolveTwoTrackFixture(candidates, () => false)).toThrow(
      /synthetic-second-track\.nzb.*synthetic\.nzb|synthetic\.nzb.*synthetic-second-track\.nzb/,
    );
  });

  test('two distinct staged tracks pass', () => {
    expect(twoTrackPreconditionFailure(['track-a', 'track-b'])).toBeNull();
  });

  test('empty library fails naming the ingest precondition', () => {
    const failure = twoTrackPreconditionFailure([]);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/ingest|library is empty/i);
  });

  test('single-track library fails naming the missing second track', () => {
    const failure = twoTrackPreconditionFailure(['only-track']);
    expect(failure).not.toBeNull();
    // Names the staged track AND the missing second one — never a timeout.
    expect(failure).toContain('only-track');
    expect(failure).toMatch(/second synthetic track is missing|only 1 track/i);
    expect(failure).toMatch(/≥2/);
  });

  test('duplicate ids fail (one distinct track is not two)', () => {
    const failure = twoTrackPreconditionFailure(['track-a', 'track-a']);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/only 1 track|second synthetic track is missing/i);
  });
});

test.describe.serial('two-track fixture stages library tracks (staging)', () => {
  // Ingest polling (queue + NNTP fetch + verify) can take minutes on staging.
  test.describe.configure({ timeout: 300_000 });

  test('two-track fixture stages ≥2 synthetic tracks in the library', async (
    { page, request },
    testInfo,
  ) => {
    const baseURL = stagingBaseUrl('skip', 'two-track-fixture');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const staging = resolveTwoTrackFixture(
      {
        singleTrack: path.resolve(FIXTURES_DIR, SINGLE_TRACK_FIXTURE_FILE),
        secondSingle: path.resolve(FIXTURES_DIR, SECOND_SINGLE_TRACK_FIXTURE_FILE),
      },
      existsSync,
    );

    const minted = await tryMintTestAccount(request, baseURL);
    // Evidence provenance: this test duplicates the signup branch, so it
    // records the same bounded signup-path label (no account data).
    annotateSignupPath(testInfo, minted);
    await applyAccountToPage(page, minted, baseURL);
    if (minted?.viaApi) {
      await page.goto(baseURL, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    } else {
      // Real UI signup fallback (not a bare navigation): shared helper with
      // the signup spec. It asserts the emailed-code step — the
      // code-plus-passkey finish needs a staging code-retrieval seam that
      // does not exist yet, so this path cannot enroll an account. API-driven
      // steps below additionally require the seam — see requireApiAccount.
      await submitPublicUiSignup(page, baseURL);
    }

    // The ingest worker probes through the user's authenticated provider
    // connection, so the ingesting identity completes the BYOP wizard first
    // (shared helper with the signup spec). Fails closed without the NNTP
    // password rather than ingesting into a provider-less account.
    await completeByopWizard(page, baseURL);

    // Stage every resolved fixture NZB through the import UI: both
    // single-file uploads in order, so the API gate below sees two rows
    // (two uploads are two jobs and two library rows per idempotency key).
    for (const nzb of staging.staged) {
      await page.goto(`${baseURL}/library/import`, {
        waitUntil: 'domcontentloaded',
        timeout: 30_000,
      });
      await waitForHydration(page, 'two-track-fixture');
      const fileInput = page.getByTestId(TESTIDS.importFile);
      await expect(fileInput, 'NZB import control missing').toBeAttached({ timeout: 15_000 });
      await fileInput.setInputFiles(nzb);
      const status = page.getByTestId(TESTIDS.importStatus);
      await expect(status, 'import gave no status').toBeVisible({ timeout: 60_000 });
      await expect(status, 'NZB ingest did not complete').toContainText(/complete|ready|done/i, {
        timeout: 240_000,
      });
    }

    // Fail-closed identity for the API gate below: the library assertion
    // must run as the ingesting account, never anonymously.
    const account = minted?.viaApi ? minted : await tryMintTestAccount(request, baseURL);
    requireApiAccount(account, 'two-track-fixture');
    const headers = accountHeaders(account);

    // Precondition via the API (no megabyte buffering: library bodies are
    // small JSON). This gate runs BEFORE any UI wait that could time out, so
    // a missing second track fails BY NAME here — never as a timeout below.
    const libraryUrl = `${baseURL}${API.library}`;
    const library = await apiGet(request, libraryUrl, headers, 'two-track-fixture');
    const libraryBody = await readApiBody(library, libraryUrl, 'two-track-fixture');
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    const items = extractLibraryItems(libraryBody);
    const apiIds = items.map((item) => String(item.id));
    const precondition = twoTrackPreconditionFailure(apiIds);
    expect(precondition, precondition ?? 'two-track precondition met').toBeNull();

    // The staged tracks must also render: ≥2 library rows for the ingesting
    // account. The API gate above already failed short when short, so this
    // wait can only time out on a render regression — not on a missing track.
    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    const rows = page.getByTestId(TESTIDS.libraryItem);
    await expect(
      rows.first(),
      '[two-track-fixture] staged tracks missing from library — ingest above must run first',
    ).toBeVisible({ timeout: 60_000 });
    const rendered = await rows.count();
    expect(
      rendered,
      `[two-track-fixture] rendered library holds ${rendered} track(s), expected ≥2 ` +
        'synthetic tracks post-fixture (API gate passed, so this is a render regression)',
    ).toBeGreaterThanOrEqual(2);
  });
});
