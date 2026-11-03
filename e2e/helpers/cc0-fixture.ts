/**
 * CC0 fixture-audio access for TS tests.
 *
 * All dev/test audio is CC0 1.0 (or a reviewed equal), recorded in a
 * committed manifest under `fixtures/cc0/` — see
 * `docs/fixture-audio-licences.md`. The audio bytes live in the R2
 * staging bucket, not in git. This helper is the TS side of the
 * fetch-by-key helper: it drives `fixtures/cc0/fetch_fixture.py`
 * (stdlib-only Python; the Rust crate `nntune-test-fixtures` drives
 * the same script, so both languages share one implementation, one
 * cache and one verification path — bytes + sha256 are checked
 * against the manifest before a path comes back).
 *
 * Skip semantics: when R2 read access is not configured (the normal
 * case in CI today) the fetcher exits 77 and this helper returns
 * `{ status: 'skipped', reason }`. Specs must `test.skip()` on that,
 * never fail:
 *
 * ```ts
 * const fixture = cc0Track(1);
 * test.skip(fixture.status === 'skipped', fixture.status === 'skipped' ? fixture.reason : '');
 * ```
 */

import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Exit code the Python fetcher uses for "skip: not configured here". */
export const FIXTURE_SKIP_EXIT = 77;

/** Environment variables that configure the fetcher's R2 transport. */
export const FIXTURE_CONFIG_ENV = [
  'NNTUNE_FIXTURE_BASE_URL',
  'NNTUNE_FIXTURE_R2_ACCESS_KEY_ID',
  'NNTUNE_FIXTURE_R2_SECRET_ACCESS_KEY',
  'NNTUNE_FIXTURE_R2_ENDPOINT',
  'NNTUNE_FIXTURE_R2_ACCOUNT_ID',
  'NNTUNE_FIXTURE_R2_BUCKET',
  'NNTUNE_FIXTURE_CACHE_DIR',
] as const;

export type Cc0Fixture =
  | { status: 'ready'; path: string }
  | { status: 'skipped'; reason: string };

function repoRoot(): string {
  // e2e/helpers/cc0-fixture.ts -> repo root is two levels up.
  return join(dirname(fileURLToPath(import.meta.url)), '..', '..');
}

function runFetcher(args: string[], env: NodeJS.ProcessEnv): Cc0Fixture {
  const result = spawnSync(
    'python3',
    [join(repoRoot(), 'fixtures/cc0/fetch_fixture.py'), ...args],
    { encoding: 'utf-8', env },
  );
  if (result.error) {
    const code = (result.error as NodeJS.ErrnoException).code;
    if (code === 'ENOENT') {
      return {
        status: 'skipped',
        reason: 'CC0 fixture fetch skipped: python3 not found on PATH',
      };
    }
    throw result.error;
  }
  const stdout = (result.stdout ?? '').trim();
  const stderr = (result.stderr ?? '').trim();
  if (result.status === 0) {
    if (!stdout) throw new Error('fixture fetcher exited 0 without printing a path');
    return { status: 'ready', path: stdout };
  }
  if (result.status === FIXTURE_SKIP_EXIT) {
    return { status: 'skipped', reason: stderr };
  }
  throw new Error(stderr || `fixture fetcher failed (exit ${result.status})`);
}

/**
 * Fetch track `position` from the default CC0 fixture album
 * (HoliznaCC0, *Busted Guitar (JAZZ)*; see the committed manifest).
 * Throws on real failures (bad position, network, manifest mismatch);
 * "not configured" returns `{ status: 'skipped' }` instead.
 *
 * `env` exists for tests (e.g. clearing the config to observe the
 * skip path); specs should use the default process environment.
 */
export function cc0Track(
  position: number,
  env: NodeJS.ProcessEnv = process.env,
): Cc0Fixture {
  return runFetcher(['--position', String(position)], env);
}

/**
 * Fetch one file from a specific manifest, addressed by the track
 * `file` value exactly as the manifest lists it. `manifest` is
 * repo-relative. The fetcher refuses files the manifest does not list.
 */
export function cc0File(
  manifest: string,
  file: string,
  env: NodeJS.ProcessEnv = process.env,
): Cc0Fixture {
  return runFetcher(['--manifest', manifest, file], env);
}
