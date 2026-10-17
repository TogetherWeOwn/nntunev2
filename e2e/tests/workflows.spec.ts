import { test, expect } from '@playwright/test';
import { readFileSync, readdirSync, mkdtempSync, rmSync, writeFileSync, mkdirSync, existsSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import config from '../playwright.config.js';

const workflowDir = new URL('../../.github/workflows/', import.meta.url);
const stagingWorkflow = readFileSync(new URL('e2e-staging.yml', workflowDir), 'utf8');

// Execute the exact checked-in shell blocks, not a copy of their logic. Only
// dummy inputs enter the child; no ambient credentials or network requests.
function runShell(script: string, env: Record<string, string> = {}, cwd?: string) {
  const scratch = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'workflow-test-'));
  const outputFile = join(scratch, 'github-output');
  try {
    const result = spawnSync('/bin/bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
      encoding: 'utf8',
      timeout: 5_000,
      ...(cwd ? { cwd } : {}),
      env: { PATH: process.env.PATH, GITHUB_OUTPUT: outputFile, ...env },
    });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    let output = result.stdout + result.stderr;
    // Failing preflights intentionally never create a configured output.
    try { output += readFileSync(outputFile, 'utf8'); } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    return { status: result.status, output };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

const preflights = [...stagingWorkflow.matchAll(
  /      - name: Check staging configuration\n([\s\S]*?)        run: \|\n((?:          [^\n]*\n)+)/g,
)];

const dummyPair = {
  CF_ACCESS_CLIENT_ID: 'dummy-client-id',
  CF_ACCESS_CLIENT_SECRET: 'dummy-client-secret',
};

test('both staging jobs have tested preflights', () => {
  expect(preflights).toHaveLength(2);
});

for (const [index, preflight] of preflights.entries()) {
  const script = preflight[2]!.replace(/^ {10}/gm, '');
  test.describe(index === 0 ? 'synthetic preflight' : 'journey preflight', () => {
    test('binds both Access secrets into the preflight environment', () => {
      expect(preflight[1]).toContain('CF_ACCESS_CLIENT_ID: ${{ secrets.CF_ACCESS_CLIENT_ID }}');
      expect(preflight[1]).toContain('CF_ACCESS_CLIENT_SECRET: ${{ secrets.CF_ACCESS_CLIENT_SECRET }}');
    });

    test('unconfigured staging skips offline', () => {
      const result = runShell(script);
      expect(result.status).toBe(0);
      expect(result.output).toContain('configured=false');
      expect(result.output).not.toContain('configured=true');
    });

    for (const [name, pair] of [
      ['both missing', {}],
      ['ID only', { CF_ACCESS_CLIENT_ID: dummyPair.CF_ACCESS_CLIENT_ID }],
      ['secret only', { CF_ACCESS_CLIENT_SECRET: dummyPair.CF_ACCESS_CLIENT_SECRET }],
    ] as const) {
      test(`configured staging rejects ${name} with names-only output`, () => {
        const result = runShell(script, { STAGING_BASE_URL: 'https://staging.example', ...pair });
        expect(result.status).not.toBe(0);
        expect(result.output).toContain('CF_ACCESS_CLIENT_ID');
        expect(result.output).toContain('CF_ACCESS_CLIENT_SECRET');
        expect(result.output).not.toContain('configured=true');
        expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_ID);
        expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_SECRET);
      });
    }

    test('configured staging with complete dummy pair proceeds', () => {
      const result = runShell(script, { STAGING_BASE_URL: 'https://staging.example', ...dummyPair });
      expect(result.status).toBe(0);
      expect(result.output).toContain('configured=true');
      expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_ID);
      expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_SECRET);
    });
  });
}

const ciWorkflow = readFileSync(new URL('ci.yml', workflowDir), 'utf8');
const aggregate = ciWorkflow.split('  ci-ok:\n')[1]?.match(/      - run: \|\n((?:          [^\n]*\n)+)/)?.[1];

test.describe('ci-ok aggregate', () => {
  for (const result of ['success', 'failure', 'cancelled', 'skipped']) {
    test(`e2e-lint ${result} ${result === 'success' ? 'passes' : 'fails'} the aggregate`, () => {
      expect(aggregate).toBeDefined();
      const needs = JSON.stringify({ placeholder: { result: 'success' }, 'e2e-lint': { result } }, null, 2);
      const script = aggregate!.replace(/^ {10}/gm, '');
      const actual = runShell(script, { NEEDS_JSON: needs });
      expect(actual.status).toBe(result === 'success' ? 0 : 1);
    });
  }
});

test('credential-bearing contexts do not record raw traces or screenshots', () => {
  expect(config.use?.trace).toBe('off');
  expect(config.use?.screenshot).toBe('off');
});

test('every workflow Action ref is a full commit SHA', () => {
  for (const name of readdirSync(fileURLToPath(workflowDir)).filter(name => name.endsWith('.yml'))) {
    const workflow = readFileSync(new URL(name, workflowDir), 'utf8');
    for (const match of workflow.matchAll(/uses:\s+[^\s@]+@([^\s#]+)/g)) {
      expect(match[1], `${name}: ${match[0]}`).toMatch(/^[a-f0-9]{40}$/);
    }
  }
});

test.describe('credential scope (static pins)', () => {
  test('no context-level extraHTTPHeaders in the Playwright config', () => {
    // Usage patterns only (colons/parens): explanatory comments may name the
    // APIs to document why they are banned.
    const configSource = readFileSync(new URL('../playwright.config.ts', import.meta.url), 'utf8');
    expect(configSource).not.toContain('extraHTTPHeaders:');
    expect(configSource).not.toContain('setExtraHTTPHeaders(');
  });

  test('browser auth is origin-scoped through a context route', () => {
    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    expect(accounts).toContain('isStagingOrigin');
    expect(accounts).toContain('context.route');
    expect(accounts).not.toContain('extraHTTPHeaders:');
    expect(accounts).not.toContain('setExtraHTTPHeaders(');
  });

  test('credentialed API calls never follow redirects (maxRedirects: 0)', () => {
    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    expect(accounts).toContain('maxRedirects: 0');
    expect(accounts).toContain('export async function apiGet');
    expect(accounts).toContain('export async function apiPost');
    expect(accounts).toContain('unexpected redirect');
  });

  // Raw API-context calls follow a staging 302 to another origin WITH the
  // Access pair + bearer (Playwright follows up to 20 redirects by default).
  // Live specs must go through apiGet/apiPost; helpers.spec.ts may use the
  // raw fixture inside its own no-follow proof (it asserts the wrapper, not
  // staging). The pin bans every method (get/post/put/patch/delete/head/fetch
  // on request/page.request/context.request) without a headers condition: the
  // old /request\.(get|post)\([^)]*headers/ stopped at the first ')' so a URL
  // containing a nested call (every API.stream() stream URL) never matched,
  // and request.fetch bypassed it entirely.
  const RAW_API_CALL = /\b(?:request|page\.request|context\.request)\.(?:get|post|put|patch|delete|head|fetch)\s*\(/;
  test('live specs use the no-follow API wrapper, not raw credentialed calls', () => {
    // Raw request.get/post with headers would follow a staging 302 to another
    // origin WITH the Access pair + bearer. Live specs must go through
    // apiGet/apiPost; helpers.spec.ts may use the raw fixture inside its own
    // no-follow proof (it asserts the wrapper, not staging).
    for (const name of [
      'health.spec.ts',
      'ingest-play-meter.spec.ts',
      'signup-byop.spec.ts',
      'demo-audio-contract.spec.ts',
      'demo-media-sessionless.spec.ts',
    ]) {
      const source = readFileSync(new URL(`../tests/${name}`, import.meta.url), 'utf8');
      expect(source, `${name} must not carry credentials on a raw request call`).not.toMatch(
        RAW_API_CALL,
      );
    }
    for (const name of [
      'health.spec.ts',
      'ingest-play-meter.spec.ts',
      'demo-audio-contract.spec.ts',
      'demo-media-sessionless.spec.ts',
    ]) {
      const source = readFileSync(new URL(`../tests/${name}`, import.meta.url), 'utf8');
      expect(source, `${name} must use the no-follow wrapper`).toContain('apiGet(');
    }
  });

  test('no-follow pin catches nested-paren and fetch mutants', () => {
    // Regression proof for the pin above: the reviewer's bypass mutants must
    // match (fail the pin), while the approved wrapper call must not.
    const nestedStreamCall =
      "await request.get(`${baseURL}${API.stream(trackId as string)}`, " +
      "{ headers: { ...headers, Range: 'bytes=0-8' }, timeout: 30_000 })";
    const fetchCall = 'await request.fetch(libraryUrl, { headers })';
    const pageScoped = 'await page.request.get(`${baseURL}/api/usage`, { headers })';
    expect(nestedStreamCall).toMatch(RAW_API_CALL);
    expect(fetchCall).toMatch(RAW_API_CALL);
    expect(pageScoped).toMatch(RAW_API_CALL);
    expect("await apiGet(request, url, headers, 'range')").not.toMatch(RAW_API_CALL);
  });
});

test.describe('public UI signup coverage (static pin)', () => {
  // Isolate the dedicated case and the shared form helper by slicing from
  // their markers to the next top-level test/export (or EOF), so a mint call
  // added to either segment (the bypass this coverage exists to prevent)
  // fails here even though other tests in the file legitimately mint.
  test('dedicated UI-signup case submits the form with no account-seam mint', () => {
    const spec = readFileSync(new URL('../tests/signup-byop.spec.ts', import.meta.url), 'utf8');
    const marker = "test('public UI signup succeeds even when the test-account seam is configured'";
    const start = spec.indexOf(marker);
    expect(start, 'dedicated public UI signup case is missing').toBeGreaterThan(-1);
    const rest = spec.slice(start + marker.length);
    const nextTest = rest.search(/\ntest(?:\(|\.describe)/);
    const dedicated = nextTest === -1 ? spec.slice(start) : spec.slice(start, start + marker.length + nextTest);
    // Null identity = Access pair only: the page starts unauthenticated, so
    // the form is the only account-creation path even with QA_AUTH_TOKEN set.
    expect(dedicated).toContain('applyAccountToPage(page, null, baseURL)');
    expect(dedicated).toContain('submitPublicUiSignup(page, baseURL)');
    expect(dedicated).not.toContain('tryMintTestAccount');

    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    const helperStart = accounts.indexOf('export async function submitPublicUiSignup');
    expect(helperStart, 'shared public UI signup helper is missing').toBeGreaterThan(-1);
    const helperRest = accounts.slice(helperStart + 'export async function submitPublicUiSignup'.length);
    const nextExport = helperRest.search(/\nexport /);
    const helper = nextExport === -1 ? accounts.slice(helperStart) : accounts.slice(helperStart, helperStart + 'export async function submitPublicUiSignup'.length + nextExport);
    expect(helper).toContain('uniqueTestEmail(');
    expect(helper).toContain('`${baseURL}/signup`');
    expect(helper).toContain('TESTIDS.signupEmail');
    expect(helper).toContain('TESTIDS.signupSubmit');
    expect(helper).toContain('TESTIDS.signupError');
    // The real page is two-step: the helper asserts the emailed-code step
    // (magic_code form), never wizard arrival — asserting the wizard without
    // the code-retrieval seam reds every configured run.
    expect(helper).toContain('magic_code');
    expect(helper).not.toContain('TESTIDS.byopHost');
    expect(helper).not.toContain("'/onboarding/byop'");
    expect(helper).not.toContain('tryMintTestAccount');
    expect(helper).not.toContain('API.testAccounts');
    expect(helper).not.toContain('qaAuthHeader');
  });
});

// Execute the exact checked-in sanitize blocks: Playwright 1.55 writes
// test-results/**/error-context.md (ARIA snapshots rendering filled input
// values, including the BYOP password) and the junit reporter embeds failure
// messages (e.g. toHaveValue Received strings) carrying the same filled
// secrets. CI uses the junit reporter only and uploads test-results/ alone;
// the sanitize step deletes error-context.md plus the whole
// playwright-report/ dir, then scrubs secret values from test-results/.
const sanitizeSections = [...stagingWorkflow.matchAll(
  /      - name: Sanitize failure artifacts([\s\S]*?)run: \|\n((?:          [^\n]*\n)+)/g,
)];
const sanitizeBlocks = sanitizeSections.map((m) => m[2]!);

function listFilesRecursive(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listFilesRecursive(full));
    else out.push(full);
  }
  return out;
}

// Scan a file for a marker in plain text AND inside base64 blobs (the html
// report embeds its payload as a base64 zip): decode every long base64 run
// and check the decoded bytes too.
function fileContainsMarker(file: string, marker: string): boolean {
  const raw = readFileSync(file, 'utf8');
  if (raw.includes(marker)) return true;
  for (const match of raw.matchAll(/[A-Za-z0-9+/]{200,}={0,2}/g)) {
    try {
      const decoded = Buffer.from(match[0], 'base64').toString('utf8');
      if (decoded.includes(marker)) return true;
    } catch {
      // Not decodable — ignore this run.
    }
  }
  return false;
}

test.describe('failure-artifact sanitization', () => {
  test('both jobs sanitize before uploading', () => {
    expect(sanitizeSections).toHaveLength(2);
    expect(sanitizeBlocks).toHaveLength(2);
    for (const section of sanitizeSections) {
      // The scrub needs the secret VALUES, so each sanitize step must bind
      // them (names only in source; values come from GitHub secrets).
      for (const name of [
        'CF_ACCESS_CLIENT_ID',
        'CF_ACCESS_CLIENT_SECRET',
        'QA_AUTH_TOKEN',
        'SYNTHETIC_NNTP_HOST',
        'SYNTHETIC_NNTP_USER',
        'SYNTHETIC_NNTP_PASS',
      ]) {
        expect(section[1], `sanitize step env binds ${name}`).toContain(name);
      }
    }
    for (const block of sanitizeBlocks) {
      const script = block.replace(/^ {10}/gm, '');
      expect(script).toContain('error-context.md');
      // Whole-dir removal: the old `playwright-report/data`-only strip left
      // index.html (with its embedded base64 zip) behind.
      expect(script).toContain('rm -rf playwright-report');
      expect(script).not.toContain('playwright-report/data');
      // junit.xml embeds failure messages (toHaveValue Received strings) that
      // can carry filled secrets: the block must scrub test-results/.
      expect(script).toContain('***REDACTED***');
      expect(script).toContain('SYNTHETIC_NNTP_PASS');
    }
    // Each sanitize precedes its job's upload: nothing uploads raw first.
    for (const artifact of ['e2e-synthetic-report', 'e2e-journey-report']) {
      const uploadAt = stagingWorkflow.indexOf(artifact);
      const sanitizeAt = stagingWorkflow.lastIndexOf('Sanitize failure artifacts', uploadAt);
      expect(sanitizeAt, `${artifact} uploads before any sanitize`).toBeGreaterThan(-1);
    }
  });

  test('uploads are junit-only (no playwright-report)', () => {
    // Both jobs upload test-results/ only. The html report (index.html with
    // its embedded base64 zip of step fill values) must never be an upload
    // path — the sanitize step may still NAME playwright-report/ to delete it.
    const uploadPaths = [...stagingWorkflow.matchAll(/path: \|\n((?:            [^\n]*\n)+)/g)];
    expect(uploadPaths).toHaveLength(2);
    for (const upload of uploadPaths) {
      expect(upload[1]).toContain('e2e/test-results/');
      expect(upload[1]).not.toContain('playwright-report');
    }
  });

  test('CI reporter is junit-only (html embeds fill values)', () => {
    const configSource = readFileSync(new URL('../playwright.config.ts', import.meta.url), 'utf8');
    expect(configSource).toContain('junit');
    expect(configSource).toContain('test-results/junit.xml');
    expect(configSource).not.toContain("'html'");
    expect(configSource).not.toContain('"html"');
  });

  // Real-browser proof: run a genuinely failing Playwright test that fills a
  // marker password under the CI reporter config (junit, no html,
  // trace/screenshot off). The junit reporter embeds the failure message
  // (toHaveValue Received string), so junit.xml DOES carry the marker before
  // sanitize. Apply the exact checked-in sanitize block with the marker bound
  // as SYNTHETIC_NNTP_PASS (as CI binds the real secret) and assert the
  // marker is absent from every file left under the upload path, including
  // base64-decoded payloads.
  test('real failing fill leaves no marker after the checked-in sanitize', async () => {
    test.setTimeout(120_000);
    const script = sanitizeBlocks[0]!.replace(/^ {10}/gm, '');
    const marker = 'MARKER-NNTP-PASS-9f8e7d6c5b4a';
    // Probe lives under e2e/ so `import '@playwright/test'` in the generated
    // spec resolves via e2e/node_modules (a system-temp dir has no ancestors
    // with the package). Removed in `finally`.
    const e2eDir = fileURLToPath(new URL('..', import.meta.url));
    const probe = mkdtempSync(join(e2eDir, '.real-sanitize-'));
    try {
      writeFileSync(
        join(probe, 'marker.spec.ts'),
        `import { test, expect } from '@playwright/test';\n` +
          `test('fills marker password then fails', async ({ page }) => {\n` +
          `  await page.setContent('<input data-testid="byop-pass" type="password" />');\n` +
          `  await page.getByTestId('byop-pass').fill('${marker}');\n` +
          // Fail on a page assertion so the failure snapshots the filled page.
          `  await expect(page.getByTestId('byop-pass')).toHaveValue('definitely-not-the-marker');\n` +
          `});\n`,
      );
      writeFileSync(
        join(probe, 'playwright.config.ts'),
        `import { defineConfig } from '@playwright/test';\n` +
          `export default defineConfig({\n` +
          `  testDir: '.',\n` +
          `  testMatch: 'marker.spec.ts',\n` +
          `  timeout: 30_000,\n` +
          `  retries: 0,\n` +
          `  reporter: [['list'], ['junit', { outputFile: 'test-results/junit.xml' }]],\n` +
          `  use: { trace: 'off', screenshot: 'off' },\n` +
          `  outputDir: 'test-results',\n` +
          `});\n`,
      );
      const cli = join(e2eDir, 'node_modules', '@playwright', 'test', 'cli.js');
      const run = spawnSync(process.execPath, [cli, 'test', `--config=${join(probe, 'playwright.config.ts')}`], {
        encoding: 'utf8',
        timeout: 90_000,
        cwd: probe,
        env: { ...process.env, CI: '1' },
      });
      const probeOutput = `${run.stdout}\n${run.stderr}`;
      // Minimal containers (no system browser libs) cannot launch Chromium;
      // CI installs --with-deps so the probe runs for real there. Skip here
      // rather than failing the suite on environment, never on assertion.
      if (/cannot open shared object|browser has been closed|browserType\.launch|executable doesn't exist/i.test(probeOutput)) {
        test.skip(true, 'Chromium system libraries unavailable in this container — probe runs in CI with --with-deps.');
        return;
      }
      // The probe must actually fail (otherwise it proved nothing).
      expect(run.status, `probe run output:\n${probeOutput}`).not.toBe(0);
      // The failing run must have produced the leak-prone snapshot with the
      // marker (otherwise the sanitize assertion is vacuous).
      const errorContexts = listFilesRecursive(probe).filter((f) => f.endsWith('error-context.md'));
      expect(errorContexts.length).toBeGreaterThan(0);
      expect(errorContexts.some((f) => readFileSync(f, 'utf8').includes(marker))).toBe(true);
      // junit.xml embeds the failure message (Received string), so it DOES
      // carry the marker before sanitize — this is the leak the scrub fixes.
      const junitBefore = listFilesRecursive(probe).filter((f) => f.endsWith('.xml'));
      expect(junitBefore.length).toBeGreaterThan(0);
      expect(junitBefore.some((f) => readFileSync(f, 'utf8').includes(marker))).toBe(true);

      // CI binds the real secret to the sanitize step; the probe binds the
      // marker the same way so the exact checked-in scrub removes it.
      const sanitized = runShell(script, { SYNTHETIC_NNTP_PASS: marker }, probe);
      expect(sanitized.status).toBe(0);
      expect(listFilesRecursive(probe).filter((f) => f.endsWith('error-context.md'))).toHaveLength(0);
      expect(existsSync(join(probe, 'playwright-report'))).toBe(false);
      // Marker absent from every remaining file under the upload path,
      // including inside base64 payloads.
      for (const file of listFilesRecursive(join(probe, 'test-results'))) {
        expect(fileContainsMarker(file, marker), file).toBe(false);
      }
      // Non-credential evidence survives sanitization.
      expect(existsSync(join(probe, 'test-results', 'junit.xml'))).toBe(true);
    } finally {
      rmSync(probe, { recursive: true, force: true });
    }
  });
});

// Fail-closed publication gate (acceptance contract for the Builder):
// Today both upload steps run on `failure() && configured` alone and the
// sanitize blocks swallow cleanup failures (`|| true`, stderr discarded)
// and per-file read errors (`except OSError: continue`), so a sanitizer
// that failed, was skipped, or silently left a file unscrubbed still
// publishes. The contract pinned below:
//   1. Each sanitize step carries an `id:` and its job's upload step adds
//      `steps.<sanitize-id>.outcome == 'success'` to its existing
//      `failure() && steps.staging.outputs.configured == 'true'`
//      condition. Skipped / failed / cancelled sanitize => no upload.
//   2. The sanitize blocks exit nonzero (names-only reporting, never file
//      contents) on any read or write failure of a file under the upload
//      path, instead of leaving it eligible.
//   3. A sanitize run may only report success when no `error-context.md`
//      path remains under test-results/ and playwright-report/ is gone:
//      a cleanup that could not remove them must fail, not pass silently.
//   4. A legitimately absent test-results/ is handled explicitly: success
//      (nothing is eligible) with an output notice stating it was absent,
//      not a generic "sanitized" claim and not a crash/traceback.
//   5. Uploads stay junit-only test-results/ with 7-day retention.
// The tests execute the exact checked-in sanitize blocks (extracted
// above) against synthetic fixtures. Failure injection is deterministic
// and root-proof (no chmod): /proc/self/mem reads raise EIO and
// /proc/version writes raise EACCES even for root, and a non-empty
// directory named error-context.md cannot be removed by file deletion.
const uploadSections = [...stagingWorkflow.matchAll(
  /      - name: Upload failure artifacts\n        if: ([^\n]+)\n([\s\S]*?)retention-days: (\d+)/g,
)];

function evaluateUploadGate(
  condition: string,
  scenario: { jobFailed: boolean; configured: boolean; sanitizeOutcome: string },
): boolean {
  const expr = condition
    .replace(/failure\(\)/g, String(scenario.jobFailed))
    .replace(
      /steps\.staging\.outputs\.configured\s*==\s*'true'/g,
      String(scenario.configured),
    )
    .replace(
      /steps\.([A-Za-z0-9_-]+)\.outcome\s*==\s*'success'/g,
      String(scenario.sanitizeOutcome === 'success'),
    );
  // Anything the evaluator does not understand stays visible as a failure
  // here rather than being silently treated as either true or false.
  expect(expr, `unmodelled upload-gate syntax in: ${condition}`).toMatch(
    /^[\s()!&|truefals]+$/,
  );
  return Function(`"use strict"; return (${expr});`)() as boolean;
}

function makeProbe(): string {
  return mkdtempSync(
    join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'artifact-gate-'),
  );
}

const GATE_MARKER = 'SENTINEL-SECRET-VALUE-GATE';

test.describe('fail-closed artifact publication gate', () => {
  test('both uploads require a successful sanitizer outcome (evaluated)', () => {
    expect(uploadSections).toHaveLength(2);
    expect(sanitizeSections).toHaveLength(2);
    for (const [index, upload] of uploadSections.entries()) {
      const condition = upload[1]!;
      const sanitizeId = sanitizeSections[index]![1]!.match(/^\s+id:\s*(\S+)/m)?.[1];
      expect(
        sanitizeId,
        `sanitize step ${index} must carry an id so the upload can gate on its outcome`,
      ).toBeDefined();
      expect(condition).toContain('failure()');
      expect(condition).toContain(`steps.staging.outputs.configured == 'true'`);
      expect(condition).toContain(`steps.${sanitizeId!}.outcome == 'success'`);

      const base = { jobFailed: true, configured: true };
      expect(evaluateUploadGate(condition, { ...base, sanitizeOutcome: 'success' })).toBe(true);
      for (const outcome of ['failure', 'skipped', 'cancelled']) {
        expect(
          evaluateUploadGate(condition, { ...base, sanitizeOutcome: outcome }),
          `upload ${index} must not publish when sanitize outcome is ${outcome}`,
        ).toBe(false);
      }
      expect(evaluateUploadGate(condition, { jobFailed: false, configured: true, sanitizeOutcome: 'success' })).toBe(false);
      expect(evaluateUploadGate(condition, { jobFailed: true, configured: false, sanitizeOutcome: 'success' })).toBe(false);
    }
  });

  test('uploads keep junit-only path and 7-day retention', () => {
    expect(uploadSections).toHaveLength(2);
    for (const upload of uploadSections) {
      expect(upload[2]).toContain('e2e/test-results/');
      expect(upload[2]).not.toContain('playwright-report');
      expect(upload[2]).not.toContain('trace');
      expect(upload[2]).not.toContain('screenshot');
      expect(upload[3]).toBe('7');
    }
  });

  for (const [index, block] of sanitizeBlocks.entries()) {
    const script = block.replace(/^ {10}/gm, '');
    const job = index === 0 ? 'synthetic' : 'journey';

    test(`${job} sanitizer: positive control publishes only scrubbed junit`, () => {
      const probe = makeProbe();
      try {
        mkdirSync(join(probe, 'test-results', 'case-1'), { recursive: true });
        mkdirSync(join(probe, 'playwright-report'), { recursive: true });
        writeFileSync(join(probe, 'test-results', 'junit.xml'), `<x>${GATE_MARKER}</x>`);
        writeFileSync(join(probe, 'test-results', 'case-1', 'error-context.md'), GATE_MARKER);
        writeFileSync(join(probe, 'playwright-report', 'index.html'), GATE_MARKER);
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(result.status).toBe(0);
        expect(result.output).not.toContain(GATE_MARKER);
        expect(existsSync(join(probe, 'playwright-report'))).toBe(false);
        const remaining = listFilesRecursive(join(probe, 'test-results'));
        expect(remaining.some((f) => f.endsWith('error-context.md'))).toBe(false);
        for (const file of remaining) {
          expect(fileContainsMarker(file, GATE_MARKER), file).toBe(false);
        }
        expect(readFileSync(join(probe, 'test-results', 'junit.xml'), 'utf8')).toContain('***REDACTED***');
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: unreadable file fails closed without leaking contents`, () => {
      const probe = makeProbe();
      try {
        mkdirSync(join(probe, 'test-results'), { recursive: true });
        writeFileSync(join(probe, 'test-results', 'junit.xml'), `<x>${GATE_MARKER}</x>`);
        // Regular file whose reads deterministically raise EIO (root too).
        symlinkSync('/proc/self/mem', join(probe, 'test-results', 'unreadable.xml'));
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(
          result.status,
          'a file the scrub could not read must fail the sanitizer, not stay upload-eligible',
        ).not.toBe(0);
        expect(result.output).not.toContain(GATE_MARKER);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: unwritable file fails closed without leaking contents`, () => {
      const probe = makeProbe();
      try {
        mkdirSync(join(probe, 'test-results'), { recursive: true });
        // Readable regular file whose writes deterministically raise EACCES.
        symlinkSync('/proc/version', join(probe, 'test-results', 'junit.xml'));
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(result.status).not.toBe(0);
        expect(result.output).not.toContain(GATE_MARKER);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: undeletable error-context cannot report success`, () => {
      const probe = makeProbe();
      try {
        // A non-empty DIRECTORY named error-context.md: file deletion
        // (`find -delete`, unlink) fails deterministically even for root.
        // Success is acceptable only if the path is actually gone
        // afterwards (a recursive remove is a legitimate stronger cleanup).
        const ctxDir = join(probe, 'test-results', 'case-1', 'error-context.md');
        mkdirSync(ctxDir, { recursive: true });
        writeFileSync(join(ctxDir, 'inner.txt'), GATE_MARKER);
        writeFileSync(join(probe, 'test-results', 'junit.xml'), '<x>ok</x>');
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        if (result.status === 0) {
          expect(
            existsSync(ctxDir),
            'sanitizer reported success while error-context.md remains upload-eligible',
          ).toBe(false);
        }
        expect(result.output).not.toContain(GATE_MARKER);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: absent test-results is explicit, not a generic success claim`, () => {
      const probe = makeProbe();
      try {
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(result.status).toBe(0);
        expect(result.output).toMatch(
          /absent|missing|no test-results|nothing to (sanitize|upload)/i,
        );
        expect(result.output).not.toContain(GATE_MARKER);
        expect(existsSync(join(probe, 'test-results'))).toBe(false);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });
  }
});

// Post-deploy sequencing contract: the full journey is promotion evidence,
// so it may run only after the same run's staging-web deploy succeeds.
// e2e-staging.yml must not race deploy.yml on push; instead deploy.yml
// calls it as a reusable workflow from a job that `needs` staging-web,
// and the journey job additionally requires the caller-only
// `inputs.post_deploy`. Scheduled/manual runs stay synthetic monitoring.
const deployWorkflow = readFileSync(new URL('deploy.yml', workflowDir), 'utf8');

function jobSection(workflow: string, job: string): string {
  const marker = `\n  ${job}:\n`;
  const start = workflow.indexOf(marker);
  expect(start, `job ${job} not found`).toBeGreaterThan(-1);
  const bodyStart = start + marker.length;
  const next = /\n  [A-Za-z0-9_-]+:\n/.exec(workflow.slice(bodyStart));
  return workflow.slice(bodyStart, next ? bodyStart + next.index : undefined);
}

function jobIf(workflow: string, job: string): string | undefined {
  // Job-level keys sit at 4-space indent; step conditions are deeper.
  return jobSection(workflow, job).match(/^    if: (.+)$/m)?.[1];
}

const stagingOnBlock = stagingWorkflow
  .split('\non:\n')[1]!
  .split('\npermissions:\n')[0]!;

test.describe('post-deploy sequencing (deploy → full journey)', () => {
  test('e2e-staging does not trigger on push (no deploy race)', () => {
    expect(stagingOnBlock).not.toMatch(/^  push:/m);
    expect(stagingOnBlock).toMatch(/^  workflow_call:/m);
    // Manual runs exist but define no inputs, so a dispatch cannot set
    // post_deploy and mint promotion evidence by hand.
    expect(stagingOnBlock).toContain('  workflow_dispatch:\n  schedule:');
    expect(stagingOnBlock).toContain('- cron: "17 */6 * * *"');
  });

  test('post_deploy is a required boolean workflow_call input', () => {
    const callBlock = stagingOnBlock
      .split('  workflow_call:\n')[1]!
      .split('\n  workflow_dispatch:')[0]!;
    expect(callBlock).toMatch(/ {6}post_deploy:\n(?:.*\n)*? {8}type: boolean\n {8}required: true/);
  });

  test('full journey runs only for the post-deploy call (evaluated)', () => {
    const condition = jobIf(stagingWorkflow, 'e2e');
    expect(condition, 'e2e job must carry a job-level if').toBeDefined();
    expect(condition).not.toContain('always()');
    expect(condition).not.toContain('!cancelled()');
    expect(jobSection(stagingWorkflow, 'e2e')).toContain('needs: [synthetic]');

    const evaluate = (postDeploy: boolean | ''): boolean => {
      const inner = condition!
        .replace(/^\$\{\{\s*/, '')
        .replace(/\s*\}\}$/, '');
      const expr = inner.replace(
        /inputs\.post_deploy/g,
        postDeploy === '' ? 'false' : String(postDeploy),
      );
      expect(expr, `unmodelled journey-gate syntax in: ${condition}`).toMatch(
        /^[\s()!&|truefals]+$/,
      );
      return Function(`"use strict"; return (${expr});`)() as boolean;
    };
    // workflow_call from deploy.yml passes post_deploy: true.
    expect(evaluate(true)).toBe(true);
    // workflow_dispatch/schedule leave the input unset: monitoring only.
    expect(evaluate('')).toBe(false);
    expect(evaluate(false)).toBe(false);
  });

  test('synthetic smoke stays ungated monitoring on every trigger', () => {
    // No job-level if: scheduled and manual runs execute the smoke (and
    // step-level skip while staging is unconfigured), distinct from the
    // gated full journey above.
    expect(jobIf(stagingWorkflow, 'synthetic')).toBeUndefined();
  });

  test('deploy calls e2e-staging only after a successful push staging-web (evaluated)', () => {
    // The deploy being sequenced after is itself push-only and gated on
    // the Cloudflare token; when it skips, dependents must skip too.
    expect(jobIf(deployWorkflow, 'staging-web')).toContain("github.event_name == 'push'");
    expect(jobIf(deployWorkflow, 'staging-web')).toContain("needs.gate.outputs.enabled == 'true'");

    const section = jobSection(deployWorkflow, 'post-deploy-e2e');
    expect(section).toContain('needs: [staging-web]');
    expect(section).toContain('uses: ./.github/workflows/e2e-staging.yml');
    expect(section).toMatch(/post_deploy: true/);

    const condition = jobIf(deployWorkflow, 'post-deploy-e2e');
    expect(condition, 'post-deploy-e2e must carry a job-level if').toBeDefined();
    // No status-check function in the condition: GitHub then applies the
    // implicit success() over `needs`, which is the whole contract — a
    // skipped/failed/cancelled staging-web can never run this job.
    for (const statusFn of ['always()', '!cancelled()', 'success()', 'failure()', 'cancelled()']) {
      expect(condition, `post-deploy-e2e if must not use ${statusFn}`).not.toContain(statusFn);
    }

    const evaluate = (stagingWebResult: string, eventName: string): boolean => {
      const inner = condition!
        .replace(/^\$\{\{\s*/, '')
        .replace(/\s*\}\}$/, '');
      const expr = inner.replace(
        /github\.event_name\s*==\s*'push'/g,
        String(eventName === 'push'),
      );
      expect(expr, `unmodelled post-deploy syntax in: ${condition}`).toMatch(
        /^[\s()!&|truefals]+$/,
      );
      const conditionHolds = Function(`"use strict"; return (${expr});`)() as boolean;
      // Implicit success() over needs.
      return stagingWebResult === 'success' && conditionHolds;
    };
    expect(evaluate('success', 'push')).toBe(true);
    for (const result of ['skipped', 'failure', 'cancelled']) {
      expect(
        evaluate(result, 'push'),
        `staging-web ${result} must not produce a full-journey run`,
      ).toBe(false);
    }
    for (const event of ['pull_request', 'workflow_dispatch', 'schedule']) {
      expect(evaluate('success', event), `${event} must not run post-deploy E2E`).toBe(false);
    }
  });

  test('deploy concurrency is job-level so the E2E tail never holds the deploy group', () => {
    // A workflow-level group is held until every job in the run finishes —
    // now including up to ~40 min of post-deploy E2E — so queued main
    // pushes would be cancelled before ever deploying. Job-level groups
    // release when the deploy finishes; E2E serializes on e2e-staging.
    expect(deployWorkflow.split('\njobs:\n')[0]!).not.toMatch(/^concurrency:/m);
    for (const job of ['staging-web', 'preview-web']) {
      const section = jobSection(deployWorkflow, job);
      expect(section).toMatch(/^    concurrency:\n      group: .+\n      cancel-in-progress: /m);
      expect(section).toContain("cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}");
    }
    // post-deploy-e2e needs no group of its own: the called workflow
    // serializes on its e2e-staging group.
    expect(jobSection(deployWorkflow, 'post-deploy-e2e')).not.toContain('concurrency:');
    expect(stagingWorkflow.split('\njobs:\n')[0]!).toMatch(
      /^concurrency:\n  group: e2e-staging\n  cancel-in-progress: false/m,
    );
  });

  test('deploy.yml is the only reusable-workflow caller of e2e-staging', () => {
    for (const name of readdirSync(fileURLToPath(workflowDir)).filter(n => n.endsWith('.yml'))) {
      const workflow = readFileSync(new URL(name, workflowDir), 'utf8');
      const calls = workflow.split('uses: ./.github/workflows/e2e-staging.yml').length - 1;
      expect(calls, `${name} calls e2e-staging.yml`).toBe(name === 'deploy.yml' ? 1 : 0);
    }
  });

  test('caller passes exactly the secrets e2e-staging reads, explicitly', () => {
    expect(deployWorkflow).not.toContain('secrets: inherit');
    const used = [...new Set(
      [...stagingWorkflow.matchAll(/secrets\.([A-Z0-9_]+)/g)].map(m => m[1]!),
    )].sort();
    const declaredBlock = stagingOnBlock
      .split('    secrets:\n')[1]!
      .split('\n  workflow_dispatch:')[0]!;
    const declared = [...declaredBlock.matchAll(/^      ([A-Z0-9_]+):$/gm)]
      .map(m => m[1]!)
      .sort();
    const passedBlock = jobSection(deployWorkflow, 'post-deploy-e2e').split('    secrets:\n')[1]!;
    const passed = [...passedBlock.matchAll(/^      ([A-Z0-9_]+): \$\{\{ secrets\.([A-Z0-9_]+) \}\}$/gm)];
    expect(declared, 'workflow_call secret declarations').toEqual(used);
    expect(passed.map(m => m[1]!).sort(), 'secrets passed by deploy.yml').toEqual(used);
    for (const m of passed) expect(m[2], 'secret passed under its own name').toBe(m[1]);
    // Every declared secret stays optional so the pre-configuration
    // step-level skip (staging not configured yet) keeps working.
    for (const m of declaredBlock.matchAll(/required: (\w+)/g)) {
      expect(m[1]).toBe('false');
    }
  });

  test('least permissions and no production deploy path are preserved', () => {
    for (const workflow of [stagingWorkflow, deployWorkflow]) {
      expect(workflow).toContain('\npermissions:\n  contents: read\n');
    }
    expect(stagingWorkflow).not.toMatch(/:\s*write\b/);
    const section = jobSection(deployWorkflow, 'post-deploy-e2e');
    expect(section).toContain('    permissions:\n      contents: read\n');
    expect(section).not.toMatch(/:\s*write\b/);
    expect(stagingWorkflow).not.toContain('wrangler');
    expect(stagingWorkflow).not.toContain('--env production');
    expect(section).not.toContain('deploy --env');
  });
});
