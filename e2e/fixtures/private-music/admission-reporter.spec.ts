import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from '@playwright/test';

test('private staging reporter and receipt wiring with disposable evidence', () => {
  test.setTimeout(60_000);
  execFileSync('python3', [fileURLToPath(new URL('./test_admission_wiring.py', import.meta.url)), process.execPath], {
    timeout: 55_000,
  });
});
