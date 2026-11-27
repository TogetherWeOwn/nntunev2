import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from '@playwright/test';

test('release, observability and private trial script boundaries with synthetic evidence', () => {
  const options = { cwd: fileURLToPath(new URL('../../../', import.meta.url)), timeout: 10_000 };
  execFileSync(process.execPath, ['--test', 'ci/test-offline-script-boundaries.mjs'], options);
  execFileSync('python3', ['-I', '-B', '-m', 'unittest', 'discover', '-s', '.github/scripts',
    '-p', 'test_offline_script_boundaries.py'], options);
});
