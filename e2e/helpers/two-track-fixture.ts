/**
 * Two-track fixture staging contract for the queue-order un-fixme path.
 *
 * The live "player queue advances in library-selection order" spec in
 * player-queue-order.spec.ts needs ≥2 ingested tracks, but the synthetic
 * NZB fixture (e2e/fixtures/synthetic.nzb) ingests exactly one — so its
 * ≥2-track precondition fails before any UI step. This helper is the
 * test-first half of that un-fixme path (second half is the player hooks
 * follow-up): it defines what "the two-track fixture ran" means and turns
 * the staged library id list into either "staged" (null) or a named
 * failure. The paired fixture build fulfills the fixture side on
 * this branch without editing the acceptance files.
 *
 * Fixture resolution: when both single-file NZBs exist (`synthetic.nzb`
 * plus `synthetic-second-track.nzb`) both are staged in order — two uploads
 * are two ingest jobs and two library rows per idempotency key, on current
 * ingest and under the future per-file manifest (ADR 0002) alike. Until the
 * second upload exists the single-track NZB is staged as a fallback so the
 * staging leg fails with the documented single-track signature instead of
 * a missing-file error. A missing second track therefore fails by NAME
 * (the precondition message below), never by timeout.
 *
 * The two-file `synthetic-two-track.nzb` is never staged: one upload is one
 * job and one library row on current ingest. It stays pinned by the offline
 * `e2e/fixtures/two-track-self-check.mjs` gate as the reference two-file
 * shape.
 *
 * This leg drives the ingest path (/library/import → /api/library) with a
 * fresh test account per run. It never touches the no-auth demo-audio
 * route (/api/demo/audio) or its `second-signal`/`third-light` track ids —
 * those checks live in tests/demo-audio-contract.spec.ts. Ids named in
 * messages here are synthetic library ids (never credentials), so they may
 * be named — unlike URLs, tokens and passwords, which stay out of public
 * CI artifacts.
 */

/** First single-track NZB the staging E2E already ingests. */
export const SINGLE_TRACK_FIXTURE_FILE = 'synthetic.nzb';

/**
 * Second single-track NZB: the 440 Hz tone as its own single-file upload
 * (segment byte-identical to file 2 of the two-file NZB, deriving the
 * distinct query `NNTune-Fixture sine-440`). Staged right after the first
 * upload so the pair yields two library rows; see resolveTwoTrackFixture.
 */
export const SECOND_SINGLE_TRACK_FIXTURE_FILE = 'synthetic-second-track.nzb';

export interface FixtureStaging {
  /** Absolute NZB paths to upload at /library/import, in order. */
  staged: string[];
  /** True once both single-file uploads exist (the single-track fallback is gone). */
  isTwoTrack: boolean;
}

/**
 * Resolve which fixture NZBs the staging leg stages. Both single-file NZBs
 * win when present (two uploads through the existing staged loop);
 * otherwise the single-track NZB is staged so the leg fails with the
 * single-track precondition signature. Throws naming both expected paths
 * when neither exists — that is a missing-fixture failure, not a passing
 * skip. `exists` is injected so the offline spec pins this without touching
 * the filesystem.
 */
export function resolveTwoTrackFixture(
  candidates: { singleTrack: string; secondSingle: string },
  exists: (candidate: string) => boolean,
): FixtureStaging {
  if (exists(candidates.singleTrack) && exists(candidates.secondSingle)) {
    return { staged: [candidates.singleTrack, candidates.secondSingle], isTwoTrack: true };
  }
  if (exists(candidates.singleTrack)) {
    return { staged: [candidates.singleTrack], isTwoTrack: false };
  }
  throw new Error(
    '[two-track-fixture] no ingest fixture found: expected ' +
      `${candidates.singleTrack} (first single-track upload) and ` +
      `${candidates.secondSingle} (second single-track upload).`,
  );
}

/**
 * Return null when the staged library holds ≥2 distinct synthetic tracks,
 * otherwise a human-readable failure naming the missing proof. A single
 * staged track names that track plus the missing second one — the
 * documented single-track signature the staging leg must fail with until
 * the two-track fixture lands.
 */
export function twoTrackPreconditionFailure(ids: string[]): string | null {
  const distinct = [...new Set(ids.filter((id) => typeof id === 'string' && id.length > 0))];
  if (distinct.length >= 2) {
    return null;
  }
  if (distinct.length === 0) {
    return (
      '[two-track-fixture] library is empty — ingest spec must run first ' +
      '(two-track fixture must stage ≥2 synthetic tracks through the ingest path)'
    );
  }
  return (
    `[two-track-fixture] library holds only 1 track (${distinct[0] ?? '(unknown)'}) — ` +
    'the second synthetic track is missing: two-track fixture must stage ≥2 distinct ' +
    'synthetic tracks through the ingest path (the synthetic fixture currently ingests one; ' +
    'queue-order needs ≥2 tracks in library-selection order)'
  );
}
