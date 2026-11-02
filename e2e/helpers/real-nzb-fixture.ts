/**
 * Staging real-NZB fixture resolution for the post-deploy journey.
 *
 * The staging provider is a real NNTP service, so the checked-in synthetic
 * NZBs (`e2e/fixtures/synthetic*.nzb`, served only by the local Rust fixture
 * server) answer `430` there. The staging fixture is instead a real,
 * secret-held NZB rebuilt in CI from the `NNTUNE_TEST_NZB_XZ_B64_1` / `_2`
 * repo secrets into `RUNNER_TEMP` only (see `.github/workflows/e2e-staging.yml`).
 * This helper resolves that runtime path: when the `TEST_NZB_PATH`
 * environment variable names an existing file, staging legs ingest it;
 * otherwise (local runs, offline unit tests) they keep the synthetic files.
 *
 * Owner rule (public repo): no NZB bytes, message-IDs, subjects, file names
 * or segment lists in the repo or public logs. This file carries none of
 * those — only the environment-variable name and static skip reasons. Legs
 * must never log the resolved path's contents, only its presence.
 *
 * Honest scope: the secret NZB is a RAR/PAR2 set, and the staging stream
 * chain serves transcode artifacts of audio inputs only
 * (`workers/ingest/src/transcode.rs` skips non-audio files), so the real NZB
 * proves ingest + matching (import completes, library row appears) but no
 * streamable track until RAR read lands. Legs that need decoded audio or
 * ≥2 tracks skip by name in real-NZB mode (see `REAL_NZB_RAR_SKIP` and the
 * journey evidence gate's real-NZB allow-list); the skip message says so.
 */

import { existsSync } from 'node:fs';

/** Environment variable carrying the runtime real-NZB path (CI `RUNNER_TEMP` only). */
export const REAL_NZB_PATH_ENV = 'TEST_NZB_PATH';

/**
 * Static skip reason for legs the RAR-set real NZB cannot feed (streaming,
 * playback, meter, queue order): ingest + matching run, decoded-audio legs
 * wait for RAR read. Fixed text — carries no NZB content and no values.
 */
export const REAL_NZB_RAR_SKIP =
  'real-NZB fixture mode (secret-held RAR set): ingest + matching run, but no ' +
  'streamable track exists until RAR read lands — decoded-audio legs skip by name ' +
  '(see e2e/README.md).';

/**
 * Resolve the runtime real-NZB path: the `envPath` value when set, nonblank
 * and existing, else null (caller falls back to the synthetic fixtures).
 * `exists` is injected so offline self-tests pin this without touching the
 * filesystem. Never logs the path's contents — presence only.
 */
export function resolveRealNzbFixture(
  envPath: string | undefined,
  exists: (candidate: string) => boolean,
): string | null {
  if (typeof envPath !== 'string') return null;
  const trimmed = envPath.trim();
  if (trimmed === '') return null;
  return exists(trimmed) ? trimmed : null;
}

/** True when a runtime real-NZB fixture is staged for this run. */
export function realNzbActive(): boolean {
  return resolveRealNzbFixture(process.env[REAL_NZB_PATH_ENV], existsSync) !== null;
}

/**
 * Resolve the NZB a leg uploads: the runtime real NZB when staged, else the
 * checked-in synthetic fallback. Keeps local/offline runs on the fixture
 * server path with no environment set.
 */
export function fixtureNzbOr(syntheticFallback: string): string {
  return resolveRealNzbFixture(process.env[REAL_NZB_PATH_ENV], existsSync) ?? syntheticFallback;
}
