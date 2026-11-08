import { test, expect } from '@playwright/test';
import { readFileSync, mkdtempSync, mkdirSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const workflow = readFileSync(new URL('../../.github/workflows/deploy.yml', import.meta.url), 'utf8');
// Scoped to the staging-api job only: the staging-mb and staging-ingest lanes
// sit between staging-api and post-deploy-e2e and carry their own same-shaped
// outcome outputs, which would otherwise satisfy the api-job pins (and mute
// the wiring mutants below). Their evidence lives in workflows.spec.ts.
const api = workflow.split('\n  staging-api:\n')[1]!.split('\n  staging-mb:\n')[0]!
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
function runShell(script: string, env: Record<string, string>, expectStatus: number | 'nonzero' = 0, cwdDir?: string) {
  const scratch = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'api-outcome-'));
  const outputFile = join(scratch, 'output');
  const summaryFile = join(scratch, 'summary');
  try {
    const result = spawnSync('/bin/bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
      cwd: cwdDir ?? scratch,
      encoding: 'utf8',
      timeout: 5_000,
      env: { PATH: process.env.PATH, GITHUB_OUTPUT: outputFile, GITHUB_STEP_SUMMARY: summaryFile, ...env },
    });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    if (expectStatus === 'nonzero') expect(result.status, result.stdout + result.stderr).not.toBe(0);
    else expect(result.status, result.stdout + result.stderr).toBe(expectStatus);
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
// Required staging-api vars. HYPERDRIVE_ID_MB is deliberately absent: the
// mirror is optional (absent or empty means no-enrichment mode).
const dummyVars = {
  HYPERDRIVE_ID_STAGING: 'dummy-hyperdrive-value',
  API_RP_ID_STAGING: 'dummy-rp-id-value',
  API_RP_ORIGIN_STAGING: 'https://dummy-origin.invalid',
};
const reporter = step('Record API deployment outcome');
const positive = {
  CONFIGURED: 'true', HYPERDRIVE_OUTCOME: 'success', SCHEMA_OUTCOME: 'success', DEPLOY_OUTCOME: 'success',
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
    ['SCHEMA_OUTCOME', 'steps.schema.outcome'],
    ['DEPLOY_OUTCOME', 'steps.deploy.outcome'],
    ['JOB_STATUS', 'job.status'],
    ['SOURCE_SHA', 'github.sha'],
  ]) {
    expect(report).toContain(`          ${name}: \${{ ${expression} }}`);
  }
  for (const [name, id, condition] of [
    ['Hyperdrive cache-disabled gate', 'hyperdrive', "steps.config.outputs.configured == 'true'"],
    ['Verify actual staging migration schema (read-only)', 'schema', "steps.config.outputs.configured == 'true'"],
    ['Deploy api to staging', 'deploy', "steps.config.outputs.configured == 'true' && steps.schema.outcome == 'success'"],
  ]) {
    const section = step(name!, source);
    expect(section).toContain(`        id: ${id}\n`);
    expect(section).toContain(`        if: ${condition}\n`);
    expect(section).not.toMatch(/always\(\)|failure\(\)|cancelled\(\)/);
  }
  expect(step('Verify actual staging migration schema (read-only)', source))
    .toContain('run: python3 .github/scripts/staging_api_schema_gate.py');
  expect(source.indexOf('- name: Verify actual staging migration schema (read-only)'))
    .toBeLessThan(source.indexOf('- name: Deploy api to staging'));
  expect(step('Generate staging wrangler.toml', source)).toContain("        if: steps.config.outputs.configured == 'true'\n");
  expect(source).not.toContain('continue-on-error:');
  expect(step('Hyperdrive cache-disabled gate', source)).toContain('working-directory: workers/api');
  expect(step('Generate staging wrangler.toml', source)).toContain('working-directory: workers/api');
  expect(step('Hyperdrive cache-disabled gate', source)).not.toContain('--allow-skip');
  expect(source.indexOf('- name: Record API deployment outcome')).toBeGreaterThan(source.indexOf('- name: Deploy api to staging'));
  expect(step('Deploy api to staging', source)).toContain('command: deploy --no-build --env staging --var BUILD_SHA:${{ github.sha }}');
}

test.describe('API deployment outcome (offline workflow contract)', () => {
  test('outputs bind to executed steps and reporting cannot bypass deploy gates', () => {
    assertWiring(api);
    for (const [from, to] of [
      ['if: always()', "if: steps.config.outputs.configured == 'true'"],
      ['steps.deploy.outcome', 'steps.deploy.conclusion'],
      ['steps.schema.outcome', 'steps.schema.conclusion'],
      [" && steps.schema.outcome == 'success'", ''],
      ['id: schema\n', 'id: renamed\n'],
      ['steps.deployment.outputs.deployed', 'steps.config.outputs.configured'],
      ["if: steps.config.outputs.configured == 'true'", 'if: always()'],
      ['id: deploy\n', 'id: renamed\n'],
    ]) {
      expect(api).toContain(from!);
      expect(() => assertWiring(api.replace(from!, to!))).toThrow();
    }
  });

  // All seven incomplete combinations of the three required vars preserve safe
  // skip, with or without the optional mirror var. Values never enter the summary.
  for (let mask = 0; mask < 7; mask++) {
    test(`missing public vars (mask ${mask}) cannot report deployed`, () => {
      const vars = Object.fromEntries(Object.entries(dummyVars).filter((_, index) => mask & (1 << index)));
      const config = runShell(shell(step('Check api staging configuration')), { ...vars, HYPERDRIVE_ID_MB: 'dummy-mirror-value' });
      expect(runShell(shell(step('Check api staging configuration')), vars).output).toBe('configured=false\n');
      expect(config.summary).not.toContain('HYPERDRIVE_ID_MB');
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
      for (const value of [...Object.values(dummyVars), 'dummy-mirror-value']) {
        expect(config.summary + config.log + report.summary + report.log).not.toContain(value);
      }
    });
  }

  for (const [name, overrides, outcome] of [
    ['setup failure', { CONFIGURED: '', HYPERDRIVE_OUTCOME: 'skipped', DEPLOY_OUTCOME: 'skipped', JOB_STATUS: 'failure' }, 'failed'],
    ['Hyperdrive failure', { HYPERDRIVE_OUTCOME: 'failure', DEPLOY_OUTCOME: 'skipped', JOB_STATUS: 'failure' }, 'failed'],
    ['schema failure', { SCHEMA_OUTCOME: 'failure', DEPLOY_OUTCOME: 'skipped', JOB_STATUS: 'failure' }, 'failed'],
    ['failed schema with green job and deploy', { SCHEMA_OUTCOME: 'failure' }, 'failed'],
    ['skipped schema with green job and deploy', { SCHEMA_OUTCOME: 'skipped' }, 'failed'],
    ['cancelled schema with green job and deploy', { SCHEMA_OUTCOME: 'cancelled' }, 'failed'],
    ['missing schema outcome with green job and deploy', { SCHEMA_OUTCOME: '' }, 'failed'],
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

  test('absent schema environment fails closed without losing the summary', () => {
    const { SCHEMA_OUTCOME: _schemaOutcome, ...env } = positive;
    const result = runShell(shell(reporter), env);
    expect(result.output).toBe('deployed=false\noutcome=failed\nsource_sha=\n');
    expect(result.summary).toContain('Read-only migration schema gate: not-run');
    expect(result.summary).not.toContain(sourceSha);
  });

  test('complete vars and successful executed steps report the deployed source only', () => {
    const config = runShell(shell(step('Check api staging configuration')), dummyVars);
    expect(config.output).toBe('configured=true\n');
    const mirrorModes: Record<string, string>[] = [{}, { HYPERDRIVE_ID_MB: '' }, { HYPERDRIVE_ID_MB: 'dummy-mirror-value' }];
    for (const mirror of mirrorModes) {
      const withMirror = runShell(shell(step('Check api staging configuration')), { ...dummyVars, ...mirror });
      expect(withMirror.output).toBe('configured=true\n');
    }
    const result = runShell(shell(reporter), positive);
    expect(result.output).toBe(`deployed=true\noutcome=deployed\nsource_sha=${sourceSha}\n`);
    expect(result.summary).toContain('deployment outcome: deployed');
    expect(result.summary).toContain('Hyperdrive cache-disabled gate: success');
    expect(result.summary).toContain('Read-only migration schema gate: success');
    expect(result.summary).toContain('Deploy api to staging: success');
    expect(result.summary).toContain(`Source SHA: ${sourceSha}`);
    expect(result.summary).toContain(`BUILD_SHA var: ${sourceSha}`);
    expect(result.summary).toContain('not health, rollback or full staging acceptance evidence');
  });

  test('config step no longer requires or reads HYPERDRIVE_ID_MB', () => {
    const section = step('Check api staging configuration');
    expect(section).not.toContain('HYPERDRIVE_ID_MB');
  });

  // Extracted generation shell, run offline against a dummy manifest. No
  // provider calls, credentials or deploy execution.
  const generator = step('Generate staging wrangler.toml');
  function generate(mirror: Record<string, string>) {
    const probe = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'api-generate-'));
    try {
      const apiDir = join(probe, 'workers/api');
      mkdirSync(apiDir, { recursive: true });
      mkdirSync(join(probe, '.github/scripts'), { recursive: true });
      writeFileSync(join(probe, '.github/scripts/binding_manifest.toml'), `
[[worker]]
name = "api"
[[worker.binding]]
binding = "HYPERDRIVE"
staging = "dummy-hyperdrive-staging"
[[worker.binding]]
binding = "MB_DB"
staging = "dummy-mb-mirror"
prod = "dummy-mb-mirror"
`);
      const env = {
        API_RP_ID_STAGING: 'staging.nntune.com',
        API_RP_ORIGIN_STAGING: 'https://staging.nntune.com',
        HYPERDRIVE_ID_STAGING: 'dummy-hyperdrive-staging',
        ...mirror,
      };
      const run = (expected: number | 'nonzero') => runShell(shell(generator), env, expected, apiDir);
      return { run, apiDir, configPath: join(apiDir, 'wrangler.toml'), read: (path: string) => (existsSync(path) ? readFileSync(path, 'utf8') : undefined) };
    } catch (error) {
      rmSync(probe, { recursive: true, force: true });
      throw error;
    }
  }
  // runShell removes only its own scratch; the probe tree is removed here.
  function withGenerated(mirror: Record<string, string>, expected: number | 'nonzero') {
    const g = generate(mirror);
    try {
      const result = g.run(expected);
      return { ...result, toml: g.read(g.configPath) };
    } finally {
      rmSync(join(g.apiDir, '../..'), { recursive: true, force: true });
    }
  }

  for (const [name, mirror] of [['unset', {}], ['empty', { HYPERDRIVE_ID_MB: '' }]] as const) {
    test(`${name} HYPERDRIVE_ID_MB omits MB_DB and reports no-enrichment mode`, () => {
      const result = withGenerated(mirror, 0);
      expect(result.toml).toBeDefined();
      expect(result.toml).toContain('binding = "HYPERDRIVE"');
      expect(result.toml).toContain('id = "dummy-hyperdrive-staging"');
      expect(result.toml).not.toContain('MB_DB');
      expect(result.toml).not.toContain('HYPERDRIVE_ID_MB');
      expect(result.summary).toContain('mirror: absent (no-enrichment mode)');
    });
  }

  test('matching HYPERDRIVE_ID_MB binds MB_DB after the primary Hyperdrive', () => {
    const result = withGenerated({ HYPERDRIVE_ID_MB: 'dummy-mb-mirror' }, 0);
    expect(result.toml).toContain('[[env.staging.hyperdrive]]\nbinding = "MB_DB"\nid = "dummy-mb-mirror"\n');
    expect(result.toml!.indexOf('binding = "HYPERDRIVE"')).toBeLessThan(result.toml!.indexOf('binding = "MB_DB"'));
    expect(result.summary).not.toContain('no-enrichment');
  });

  test('mismatched HYPERDRIVE_ID_MB is refused before any config is written', () => {
    const result = withGenerated({ HYPERDRIVE_ID_MB: 'dummy-other-mirror' }, 'nonzero');
    expect(result.log).toContain('HYPERDRIVE_ID_MB must equal the api MB_DB inventory id');
    expect(result.toml).toBeUndefined();
    expect(result.summary).not.toContain('mirror:');
  });
});
