import { test, expect } from '@playwright/test';
import { readFileSync, readdirSync, mkdtempSync, rmSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
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
});

// Execute the exact checked-in sanitize blocks: Playwright 1.55 writes
// test-results/**/error-context.md (ARIA snapshots rendering filled input
// values, including the BYOP password) and embeds the same text in
// playwright-report/data/*.md. Both directories are uploaded on failure, so
// the sanitize step must strip a marker password from every uploaded path.
const sanitizeBlocks = [...stagingWorkflow.matchAll(
  /      - name: Sanitize failure artifacts[\s\S]*?run: \|\n((?:          [^\n]*\n)+)/g,
)];

function listFilesRecursive(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listFilesRecursive(full));
    else out.push(full);
  }
  return out;
}

test.describe('failure-artifact sanitization', () => {
  test('both jobs sanitize before uploading', () => {
    expect(sanitizeBlocks).toHaveLength(2);
    for (const block of sanitizeBlocks) {
      const script = block[1]!.replace(/^ {10}/gm, '');
      expect(script).toContain('error-context.md');
      expect(script).toContain('playwright-report/data');
    }
    // Each sanitize precedes its job's upload: nothing uploads raw first.
    for (const artifact of ['e2e-synthetic-report', 'e2e-journey-report']) {
      const uploadAt = stagingWorkflow.indexOf(artifact);
      const sanitizeAt = stagingWorkflow.lastIndexOf('Sanitize failure artifacts', uploadAt);
      expect(sanitizeAt, `${artifact} uploads before any sanitize`).toBeGreaterThan(-1);
    }
  });

  for (const [index, block] of sanitizeBlocks.entries()) {
    test(`sanitize script ${index} strips a marker password from every uploaded path`, () => {
      const script = block[1]!.replace(/^ {10}/gm, '');
      const artifacts = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'sanitize-test-'));
      try {
        mkdirSync(join(artifacts, 'test-results', 'spec-chunk'), { recursive: true });
        mkdirSync(join(artifacts, 'playwright-report', 'data'), { recursive: true });
        const marker = 'MARKER-NNTP-PASS-9f8e7d6c5b4a';
        writeFileSync(
          join(artifacts, 'test-results', 'spec-chunk', 'error-context.md'),
          `- textbox [ref=e5]: ${marker}\n`,
        );
        writeFileSync(
          join(artifacts, 'playwright-report', 'data', 'abc123.md'),
          `snapshot with ${marker}\n`,
        );
        writeFileSync(join(artifacts, 'test-results', 'spec-chunk', 'junit.xml'), '<xml>safe</xml>');
        writeFileSync(join(artifacts, 'playwright-report', 'index.html'), '<html>safe</html>');
        const result = runShell(script, {}, artifacts);
        expect(result.status).toBe(0);
        expect(existsSync(join(artifacts, 'test-results', 'spec-chunk', 'error-context.md'))).toBe(false);
        expect(existsSync(join(artifacts, 'playwright-report', 'data'))).toBe(false);
        // Marker absent from every remaining path the workflow uploads.
        for (const file of listFilesRecursive(artifacts)) {
          expect(readFileSync(file, 'utf8'), file).not.toContain(marker);
        }
        // Non-credential evidence survives sanitization.
        expect(existsSync(join(artifacts, 'test-results', 'spec-chunk', 'junit.xml'))).toBe(true);
        expect(existsSync(join(artifacts, 'playwright-report', 'index.html'))).toBe(true);
      } finally {
        rmSync(artifacts, { recursive: true, force: true });
      }
    });
  }
});
