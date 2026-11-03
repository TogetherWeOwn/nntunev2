/**
 * Offline unit tests for the CC0 fixture helper (`helpers/cc0-fixture.ts`).
 *
 * These never touch R2: with the fetcher's config env cleared (and an
 * empty cache dir, so a previous real fetch cannot leak in) the helper
 * must report a skip with a reason — that is the contract every spec
 * needing real audio relies on in CI. Manifest errors stay errors.
 */
import { test, expect } from '@playwright/test';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { cc0Track, FIXTURE_CONFIG_ENV } from '../helpers/cc0-fixture.js';

function unconfiguredEnv(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env };
  for (const name of FIXTURE_CONFIG_ENV) delete env[name];
  env.NNTUNE_FIXTURE_CACHE_DIR = mkdtempSync(join(tmpdir(), 'cc0-fixture-spec-'));
  return env;
}

test('unconfigured environment skips with a reason, never fails', () => {
  const fixture = cc0Track(1, unconfiguredEnv());
  expect(fixture.status).toBe('skipped');
  if (fixture.status === 'skipped') {
    expect(fixture.reason).toContain('no R2 read access configured');
  }
});

test('unknown track position is an error, not a skip', () => {
  expect(() => cc0Track(99, unconfiguredEnv())).toThrow(/position 99/);
});
