import { test, expect } from '@playwright/test';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const workflow = readFileSync(new URL('../../.github/workflows/deploy.yml', import.meta.url), 'utf8');
const api = workflow.split('\n  staging-api:\n')[1]!.split('\n  post-deploy-e2e:\n')[0]!
  .replace(/^\s*#.*$/gm, '');

function step(name: string, source = api): string {
  const section = source.split(`      - name: ${name}\n`)[1];
  expect(section, `missing step: ${name}`).toBeDefined();
  return section!.split(/\n      - /)[0]! + '\n';
}

function shell(section: string): string {
  const block = section.match(/        run: \|\n((?:(?: {10}[^\n]*)?\n)+)/)?.[1];
  expect(block, 'missing shell block').toBeDefined();
  return block!.replace(/^ {10}/gm, '');
}

// Run only extracted preflight/report shell. No Actions, provider calls,
// ambient credentials, generated Wrangler config or deployment execution.
function runShell(script: string, env: Record<string, string>) {
  const scratch = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'api-outcome-'));
  const outputFile = join(scratch, 'output');
  const summaryFile = join(scratch, 'summary');
  try {
    const result = spawnSync('/bin/bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
      cwd: scratch,
      encoding: 'utf8',
      timeout: 5_000,
      env: { PATH: process.env.PATH, GITHUB_OUTPUT: outputFile, GITHUB_STEP_SUMMARY: summaryFile, ...env },
    });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    expect(result.status, result.stdout + result.stderr).toBe(0);
    const read = (path: string): string => {
      try { return readFileSync(path, 'utf8'); } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
        return '';
      }
    };
    return { output: read(outputFile), summary: read(summaryFile), log: result.stdout + result.stderr };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

const sourceSha = '0123456789abcdef0123456789abcdef01234567';
const dummyVars = {
  HYPERDRIVE_ID_STAGING: 'dummy-hyperdrive-value',
  API_RP_ID_STAGING: 'dummy-rp-id-value',
  API_RP_ORIGIN_STAGING: 'https://dummy-origin.invalid',
};
const reporter = step('Record API deployment outcome');
const positive = {
  CONFIGURED: 'true', HYPERDRIVE_OUTCOME: 'success', DEPLOY_OUTCOME: 'success',
  JOB_STATUS: 'success', SOURCE_SHA: sourceSha,
};

function assertWiring(source: string) {
  for (const name of ['deployed', 'outcome', 'source_sha']) {
    expect(source).toContain(`      ${name}: \${{ steps.deployment.outputs.${name} }}`);
  }
  const report = step('Record API deployment outcome', source);
  expect(report).toContain('        id: deployment\n');
  expect(report).toContain('        if: always()\n');
  // Reporting must work even when checkout/setup failed before workers/api exists.
  expect(source).not.toContain('    defaults:');
  expect(report).not.toContain('working-directory:');
  for (const [name, expression] of [
    ['CONFIGURED', 'steps.config.outputs.configured'],
    ['HYPERDRIVE_OUTCOME', 'steps.hyperdrive.outcome'],
    ['DEPLOY_OUTCOME', 'steps.deploy.outcome'],
    ['JOB_STATUS', 'job.status'],
    ['SOURCE_SHA', 'github.sha'],
  ]) {
    expect(report).toContain(`          ${name}: \${{ ${expression} }}`);
  }
  for (const [name, id] of [
    ['Hyperdrive cache-disabled gate', 'hyperdrive'], ['Deploy api to staging', 'deploy'],
  ]) {
    const section = step(name!, source);
    expect(section).toContain(`        id: ${id}\n`);
    expect(section).toContain("        if: steps.config.outputs.configured == 'true'\n");
    expect(section).not.toMatch(/always\(\)|failure\(\)|cancelled\(\)/);
  }
  expect(step('Generate staging wrangler.toml', source)).toContain("        if: steps.config.outputs.configured == 'true'\n");
  expect(source).not.toContain('continue-on-error:');
  expect(step('Hyperdrive cache-disabled gate', source)).toContain('working-directory: workers/api');
  expect(step('Generate staging wrangler.toml', source)).toContain('working-directory: workers/api');
  expect(step('Hyperdrive cache-disabled gate', source)).not.toContain('--allow-skip');
  expect(source.indexOf('- name: Record API deployment outcome')).toBeGreaterThan(source.indexOf('- name: Deploy api to staging'));
  expect(step('Deploy api to staging', source)).toContain('command: deploy --env staging --var BUILD_SHA:${{ github.sha }}');
}

test.describe('API deployment outcome (offline workflow contract)', () => {
  test('outputs bind to executed steps and reporting cannot bypass deploy gates', () => {
    assertWiring(api);
    for (const [from, to] of [
      ['if: always()', "if: steps.config.outputs.configured == 'true'"],
      ['steps.deploy.outcome', 'steps.deploy.conclusion'],
      ['steps.deployment.outputs.deployed', 'steps.config.outputs.configured'],
      ["if: steps.config.outputs.configured == 'true'", 'if: always()'],
      ['id: deploy\n', 'id: renamed\n'],
    ]) {
      expect(api).toContain(from!);
      expect(() => assertWiring(api.replace(from!, to!))).toThrow();
    }
  });

  // All seven incomplete combinations preserve safe skip, even though the
  // preflight and job may succeed. Values must never enter the summary.
  for (let mask = 0; mask < 7; mask++) {
    test(`missing public vars (mask ${mask}) cannot report deployed`, () => {
      const vars = Object.fromEntries(Object.entries(dummyVars).filter((_, index) => mask & (1 << index)));
      const config = runShell(shell(step('Check api staging configuration')), vars);
      expect(config.output).toBe('configured=false\n');
      for (const name of Object.keys(dummyVars).filter(name => !(name in vars))) {
        expect(config.summary).toContain(name);
      }
      const report = runShell(shell(reporter), {
        ...positive, CONFIGURED: 'false', HYPERDRIVE_OUTCOME: 'skipped', DEPLOY_OUTCOME: 'skipped', ...dummyVars,
      });
      expect(report.output).toBe('deployed=false\noutcome=unconfigured\nsource_sha=\n');
      expect(report.summary).toContain('deployment outcome: unconfigured');
      expect(report.summary).not.toContain(sourceSha);
      for (const value of Object.values(dummyVars)) {
        expect(config.summary + config.log + report.summary + report.log).not.toContain(value);
      }
    });
  }

  for (const [name, overrides, outcome] of [
    ['setup failure', { CONFIGURED: '', HYPERDRIVE_OUTCOME: 'skipped', DEPLOY_OUTCOME: 'skipped', JOB_STATUS: 'failure' }, 'failed'],
    ['Hyperdrive failure', { HYPERDRIVE_OUTCOME: 'failure', DEPLOY_OUTCOME: 'skipped', JOB_STATUS: 'failure' }, 'failed'],
    ['config generation failure', { DEPLOY_OUTCOME: 'skipped', JOB_STATUS: 'failure' }, 'failed'],
    ['deploy failure', { DEPLOY_OUTCOME: 'failure', JOB_STATUS: 'failure' }, 'failed'],
    ['skipped deploy with green job', { DEPLOY_OUTCOME: 'skipped' }, 'failed'],
    ['skipped Hyperdrive with green job', { HYPERDRIVE_OUTCOME: 'skipped' }, 'failed'],
    ['cancelled deploy', { DEPLOY_OUTCOME: 'cancelled', JOB_STATUS: 'cancelled' }, 'cancelled'],
    ['missing outcome', { DEPLOY_OUTCOME: '' }, 'failed'],
    ['failed job despite successful deploy', { JOB_STATUS: 'failure' }, 'failed'],
  ] as const) {
    test(`${name} has no positive outcome or deployed SHA`, () => {
      const env = { ...positive, ...overrides };
      const result = runShell(shell(reporter), env);
      expect(result.output).toBe(`deployed=false\noutcome=${outcome}\nsource_sha=\n`);
      expect(result.summary).toContain(`deployment outcome: ${outcome}`);
      expect(result.summary).toContain(`Deploy api to staging: ${env.DEPLOY_OUTCOME || 'not-run'}`);
      expect(result.summary).not.toContain('Source SHA:');
      expect(result.summary).not.toContain(sourceSha);
    });
  }

  test('complete vars and successful executed steps report the deployed source only', () => {
    const config = runShell(shell(step('Check api staging configuration')), dummyVars);
    expect(config.output).toBe('configured=true\n');
    const result = runShell(shell(reporter), positive);
    expect(result.output).toBe(`deployed=true\noutcome=deployed\nsource_sha=${sourceSha}\n`);
    expect(result.summary).toContain('deployment outcome: deployed');
    expect(result.summary).toContain('Hyperdrive cache-disabled gate: success');
    expect(result.summary).toContain('Deploy api to staging: success');
    expect(result.summary).toContain(`Source SHA: ${sourceSha}`);
    expect(result.summary).toContain(`BUILD_SHA var: ${sourceSha}`);
    expect(result.summary).toContain('not health, rollback or full staging acceptance evidence');
  });
});
