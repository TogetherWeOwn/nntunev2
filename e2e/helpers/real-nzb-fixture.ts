/**
 * Staging real-NZB fixture resolution for the post-deploy journey.
 *
 * The staging provider is a real NNTP service, so the journey ingests a
 * real, secret-held NZB rebuilt in CI from the `NNTUNE_TEST_NZB_XZ_B64_1` /
 * `_2` repo secrets into `RUNNER_TEMP` only (see
 * `.github/workflows/e2e-staging.yml`). This helper resolves that runtime
 * path: when the `TEST_NZB_PATH` environment variable names an existing
 * file, staging legs ingest it; otherwise they fail closed — there are no
 * checked-in NZB fixtures to fall back to (TWO-1169 removed them all).
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
 * journey evidence gate's real-NZB allow-list); the skip reason links the
 * RAR-set playback follow-up (TWO-1174). The 430 control leg needs no NZB
 * at all (it uses a random, never-posted message-ID).
 */

import { existsSync } from 'node:fs';

/** Environment variable carrying the runtime real-NZB path (CI `RUNNER_TEMP` only). */
export const REAL_NZB_PATH_ENV = 'TEST_NZB_PATH';

/**
 * Follow-up card for the parked decoded-audio legs: RAR-set playback and
 * multi-track staging land there; until then the legs below skip by name.
 */
export const RAR_PLAYBACK_FOLLOW_UP = 'TWO-1174';

/**
 * Static skip reason for legs the RAR-set real NZB cannot feed (streaming,
 * playback, meter, queue order): ingest + matching run, decoded-audio legs
 * wait for RAR read. Fixed text — carries no NZB content and no values.
 */
export const REAL_NZB_RAR_SKIP =
  'real-NZB fixture mode (secret-held RAR set): ingest + matching run, but no ' +
  'streamable track exists until RAR read lands — decoded-audio legs skip by name ' +
  '(see e2e/README.md); parked legs un-park under TWO-1174.';

/**
 * Resolve the runtime real-NZB path: the `envPath` value when set, nonblank
 * and existing, else null (the caller fails closed — no synthetic fallback).
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
 * Resolve the NZB a leg uploads: the staged runtime real NZB, or throw a
 * fail-closed error naming the missing staging (never a synthetic file —
 * TWO-1169 deleted every checked-in NZB). Staging legs call this after the
 * pre-deploy skip, so a throw here is a red staging run, not a silent pass.
 */
export function requireRealNzbFixture(): string {
  const staged = resolveRealNzbFixture(process.env[REAL_NZB_PATH_ENV], existsSync);
  if (staged === null) {
    throw new Error(
      `[real-nzb] no staged real-NZB fixture (${REAL_NZB_PATH_ENV} unset, blank or missing): ` +
        'the journey ingests only the secret-held real NZB staged by the workflow ' +
        'prepare step into RUNNER_TEMP — there is no synthetic fallback.',
    );
  }
  return staged;
}
