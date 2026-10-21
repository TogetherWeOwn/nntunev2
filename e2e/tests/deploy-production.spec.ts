import { test, expect } from '@playwright/test';
import {
  readFileSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
  chmodSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import {
  createServer,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from 'node:http';
import { fileURLToPath } from 'node:url';

const workflowDir = new URL('../../.github/workflows/', import.meta.url);
const productionWorkflow = readFileSync(
  new URL('deploy-production.yml', workflowDir),
  'utf8',
);
const repoRoot = fileURLToPath(new URL('../..', import.meta.url));
const smokeCli = join(repoRoot, 'bin', 'smoke.mjs');

// Execute the exact checked-in shell blocks, not a copy of their logic. Only
// dummy inputs enter the child; no ambient credentials or network requests.
function runShell(script: string, env: Record<string, string> = {}) {
  const scratch = mkdtempSync(
    join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'prod-pipe-'),
  );
  const outputFile = join(scratch, 'github-output');
  try {
    const result = spawnSync(
      '/bin/bash',
      ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script],
      {
        encoding: 'utf8',
        timeout: 5_000,
        env: { PATH: process.env.PATH, GITHUB_OUTPUT: outputFile, ...env },
      },
    );
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    let output = result.stdout + result.stderr;
    // Failing preflights intentionally never create a configured output.
    try {
      output += readFileSync(outputFile, 'utf8');
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    return { status: result.status, output };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

const VALID_SHA = 'a'.repeat(40);
const OTHER_SHA = 'b'.repeat(40);
const VERSION_UUID = '12345678-1234-1234-1234-123456789abc';

// Async child runner for cases where the child talks to an in-test HTTP
// stub: spawnSync would block the worker event loop, so the stub could never
// accept the child's connections. Async spawn keeps the loop (and the stub)
// alive while the child runs.
function runChild(
  cmd: string,
  args: string[],
  extraEnv: Record<string, string>,
  timeoutMs: number,
): Promise<{ status: number | null; output: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {
      env: { ...process.env, ...extraEnv },
    });
    let output = '';
    child.stdout.on('data', (chunk) => {
      output += chunk;
    });
    child.stderr.on('data', (chunk) => {
      output += chunk;
    });
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`child timed out after ${timeoutMs}ms: ${output}`));
    }, timeoutMs);
    child.on('error', (err) => {
      clearTimeout(timer);
      reject(err);
    });
    child.on('close', (code) => {
      clearTimeout(timer);
      resolve({ status: code, output });
    });
  });
}

test.describe('production preflight (exact checked-in block, offline)', () => {
  const match = productionWorkflow.match(
    /      - name: Resolve and validate the promotion target\n(?:        [^\n]*\n)*?        run: \|\n((?:          [^\n]*\n)+)/,
  );
  const script = match?.[1]?.replace(/^ {10}/gm, '');

  // The step block reads PRODUCTION_DOMAIN_DEFAULT from the workflow-level
  // env (GitHub injects it); the offline runner binds the same value so the
  // exact block executes with the same inputs it gets in CI.
  const STEP_ENV = {
    PRODUCTION_DOMAIN_DEFAULT: 'production-placeholder.example.invalid',
  };

  test('the preflight block is present and extractable', () => {
    expect(script, 'preflight run block missing').toBeDefined();
    expect(script).toContain('PRODUCTION_DEPLOY_ENABLED');
  });

  test('the workflow default hostname is a non-routable placeholder', () => {
    const envBlock = productionWorkflow.split('\njobs:\n')[0]!;
    expect(envBlock).toContain(
      'PRODUCTION_DOMAIN_DEFAULT: "production-placeholder.example.invalid"',
    );
  });

  test('disabled lane refuses without the enabled flag', () => {
    const result = runShell(script!, { ...STEP_ENV, INPUT_SHA: VALID_SHA });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('Production lane disabled');
  });

  test('refuses a non-40-hex sha', () => {
    const result = runShell(script!, {
      ...STEP_ENV,
      INPUT_SHA: 'abc',
      PRODUCTION_DEPLOY_ENABLED: 'true',
      PRODUCTION_DOMAIN: 'example.com',
    });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('sha input must be');
  });

  test('refuses the placeholder hostname', () => {
    const result = runShell(script!, {
      ...STEP_ENV,
      INPUT_SHA: VALID_SHA,
      PRODUCTION_DEPLOY_ENABLED: 'true',
    });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('Production hostname not decided');
  });

  test('refuses a non-bare hostname', () => {
    const result = runShell(script!, {
      ...STEP_ENV,
      INPUT_SHA: VALID_SHA,
      PRODUCTION_DEPLOY_ENABLED: 'true',
      PRODUCTION_DOMAIN: 'https://example.com/app',
    });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('bare hostname');
  });

  test('accepts an enabled lane with a valid sha and domain', () => {
    const result = runShell(script!, {
      ...STEP_ENV,
      INPUT_SHA: VALID_SHA,
      PRODUCTION_DEPLOY_ENABLED: 'true',
      PRODUCTION_DOMAIN: 'example.com',
    });
    expect(result.status).toBe(0);
    expect(result.output).toContain(`sha=${VALID_SHA}`);
    expect(result.output).toContain('domain=example.com');
  });
});

type RunRow = {
  id: number;
  head_sha: string;
  status: string;
  conclusion: string;
  event: string;
};

type JobRow = { name: string; conclusion: string };

// Stubbed GitHub API for the staging-evidence script. The exact checked-in
// Python block runs unmodified except for the API base URL, which points at
// this stub instead of api.github.com (test-only substitution). The stub
// emulates GitHub's server-side event filter: when the script requests
// event=push, dispatch runs are withheld, mirroring production behavior.
const evidenceStub = {
  runs: [] as RunRow[],
  jobsByRun: new Map<number, JobRow[]>(),
  ignoreEventFilter: false,
  lastRunsUrl: '',
};

function startEvidenceStub(): Promise<{ server: Server; base: string }> {
  const server = createServer((req: IncomingMessage, res: ServerResponse) => {
    const url = new URL(req.url ?? '/', 'http://stub');
    if (url.pathname.endsWith('/actions/workflows/deploy.yml/runs')) {
      evidenceStub.lastRunsUrl = req.url ?? '';
      const event = url.searchParams.get('event');
      const rows =
        event === 'push' && !evidenceStub.ignoreEventFilter
          ? evidenceStub.runs.filter((r) => r.event === 'push')
          : [...evidenceStub.runs];
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ workflow_runs: rows }));
      return;
    }
    const jobsMatch = url.pathname.match(/\/actions\/runs\/(\d+)\/jobs/);
    if (jobsMatch) {
      const jobs = evidenceStub.jobsByRun.get(Number(jobsMatch[1])) ?? [];
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ jobs }));
      return;
    }
    res.writeHead(404, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ message: 'not found' }));
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port =
        typeof address === 'object' && address ? address.port : 0;
      resolve({ server, base: `http://127.0.0.1:${port}` });
    });
  });
}

async function runEvidence(base: string) {
  const match = productionWorkflow.match(
    /python3 - <<'PY'\n([\s\S]*?)\n          PY/,
  );
  expect(match?.[1], 'evidence python block missing').toBeDefined();
  // Test-only substitution: the exact block text runs, with only the API
  // host redirected at the local stub (no ambient network).
  const script = match![1]!
    .replace(/^ {10}/gm, '')
    .split('https://api.github.com')
    .join(base);
  const scratch = mkdtempSync(
    join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'evidence-'),
  );
  try {
    const file = join(scratch, 'evidence.py');
    writeFileSync(file, script);
    return await runChild(
      'python3',
      [file],
      {
        REPO: 'o/r',
        DEPLOY_SHA: VALID_SHA,
        GITHUB_TOKEN: 'dummy-token',
      },
      60_000,
    );
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

const PUSH_RUN: RunRow = {
  id: 100,
  head_sha: VALID_SHA,
  status: 'completed',
  conclusion: 'success',
  event: 'push',
};

// A later manual dispatch on the same SHA: staging jobs skip on dispatch, so
// it carries no evidence and must never shadow the push run above.
const DISPATCH_RUN: RunRow = {
  id: 101,
  head_sha: VALID_SHA,
  status: 'completed',
  conclusion: 'success',
  event: 'workflow_dispatch',
};

const HEALTHY_JOBS: JobRow[] = [
  { name: 'staging-web', conclusion: 'success' },
  { name: 'post-deploy-e2e / synthetic smoke (health)', conclusion: 'success' },
  {
    name: 'post-deploy-e2e / full journey (signup → byop → ingest → meter → limits)',
    conclusion: 'success',
  },
];

test.describe('staging evidence (exact checked-in block, stubbed API)', () => {
  let server: Server;
  let base: string;

  test.beforeAll(async () => {
    ({ server, base } = await startEvidenceStub());
  });

  test.afterAll(async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  test.beforeEach(() => {
    evidenceStub.runs = [];
    evidenceStub.jobsByRun = new Map();
    evidenceStub.ignoreEventFilter = false;
    evidenceStub.lastRunsUrl = '';
  });

  test('accepts prefixed called-workflow e2e job names on a push run', async () => {
    evidenceStub.runs = [PUSH_RUN];
    evidenceStub.jobsByRun.set(100, HEALTHY_JOBS);
    const result = await runEvidence(base);
    expect(result.status).toBe(0);
    expect(result.output).toContain('staging evidence ok');
    expect(result.output).toContain('deploy run 100');
  });

  test('accepts the bare post-deploy-e2e name', async () => {
    evidenceStub.runs = [PUSH_RUN];
    evidenceStub.jobsByRun.set(100, [
      { name: 'staging-web', conclusion: 'success' },
      { name: 'post-deploy-e2e', conclusion: 'success' },
    ]);
    expect((await runEvidence(base)).status).toBe(0);
  });

  test('requests only push-triggered deploy runs', async () => {
    evidenceStub.runs = [PUSH_RUN];
    evidenceStub.jobsByRun.set(100, HEALTHY_JOBS);
    expect((await runEvidence(base)).status).toBe(0);
    expect(evidenceStub.lastRunsUrl).toContain('event=push');
  });

  test('a later manual dispatch never shadows push evidence', async () => {
    // Without the event=push filter the script would select run 101 (the
    // newest id) and refuse on its skipped staging jobs. The filter keeps
    // run 100 selectable however many dispatches follow it.
    evidenceStub.runs = [PUSH_RUN, DISPATCH_RUN];
    evidenceStub.jobsByRun.set(100, HEALTHY_JOBS);
    evidenceStub.jobsByRun.set(101, [
      { name: 'staging-web', conclusion: 'skipped' },
    ]);
    const result = await runEvidence(base);
    expect(result.status).toBe(0);
    expect(result.output).toContain('deploy run 100');
  });

  test('refuses when no e2e job matches', async () => {
    evidenceStub.runs = [PUSH_RUN];
    evidenceStub.jobsByRun.set(100, [
      { name: 'staging-web', conclusion: 'success' },
    ]);
    const result = await runEvidence(base);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('post-deploy-e2e');
  });

  test('refuses when a prefixed e2e job is not success', async () => {
    evidenceStub.runs = [PUSH_RUN];
    evidenceStub.jobsByRun.set(100, [
      { name: 'staging-web', conclusion: 'success' },
      {
        name: 'post-deploy-e2e / synthetic smoke (health)',
        conclusion: 'success',
      },
      {
        name: 'post-deploy-e2e / full journey (signup → byop → ingest → meter → limits)',
        conclusion: 'failure',
      },
    ]);
    const result = await runEvidence(base);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('not all success');
  });

  test('refuses a non-push run even if the listing returns one', async () => {
    // Defense in depth for the server-side filter: a listing that hands
    // back a dispatch run must still never count as evidence.
    evidenceStub.ignoreEventFilter = true;
    evidenceStub.runs = [DISPATCH_RUN];
    evidenceStub.jobsByRun.set(101, [
      { name: 'staging-web', conclusion: 'success' },
      { name: 'post-deploy-e2e', conclusion: 'success' },
    ]);
    const result = await runEvidence(base);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('not push-triggered');
  });
});

test.describe('production seed gate (exact checked-in block, stubbed wrangler)', () => {
  const match = productionWorkflow.match(
    /      - name: Require one-time production seed deployment\n(?:        [^\n]*\n)*?        run: \|\n((?:          [^\n]*\n)+)/,
  );
  const script = match?.[1]?.replace(/^ {10}/gm, '');

  // The step shells out to `npx wrangler ... --json`; the offline runner puts
  // a stub npx first on PATH that replays fixture JSON. Two test-only
  // substitutions apply, both mirroring what GitHub does in CI: the stub for
  // npx (no network, no wrangler install), and the `${{ env.* }}`
  // expression evaluated to the workflow-pinned value (Actions expands it
  // before bash ever sees it; bash alone would reject the `${{ }}` shape).
  // The pin test below keeps both substitutions honest.
  async function runSeed(rawOutput: string) {
    const bin = mkdtempSync(
      join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'seed-bin-'),
    );
    const scratch = mkdtempSync(
      join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'seed-'),
    );
    // NOTE: the stub dir is exported inside the script, not via spawn env:
    // some sandboxes reset PATH on spawn, while an in-script export always
    // applies. The exact checked-in block below the export line is untouched.
    const runnable =
      `export PATH="${bin}:$PATH"\n` +
      script!.split('${{ env.WRANGLER_VERSION }}').join('4.150.0');
    try {
      const fixture = join(scratch, 'deployments.json');
      writeFileSync(fixture, rawOutput);
      const argvFile = join(scratch, 'npx-argv');
      const stub = join(bin, 'npx');
      // The stub records its argv so the tests can see exactly which worker
      // the gate queries; a gate repointed at staging must fail, not replay
      // the production fixture silently.
      writeFileSync(
        stub,
        '#!/bin/bash\nprintf \'%s\\n\' "$@" > "$SEED_ARGV"\ncat "$SEED_FIXTURE"\n',
      );
      chmodSync(stub, 0o755);
      const result = await runChild(
        '/bin/bash',
        ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', runnable],
        {
          SEED_FIXTURE: fixture,
          SEED_ARGV: argvFile,
          WORKER_NAME: 'nntune-web',
          CLOUDFLARE_API_TOKEN: 'dummy-token',
          CLOUDFLARE_ACCOUNT_ID: 'dummy-account',
        },
        60_000,
      );
      let argv = '';
      try {
        argv = readFileSync(argvFile, 'utf8');
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      }
      return { ...result, argv };
    } finally {
      rmSync(bin, { recursive: true, force: true });
      rmSync(scratch, { recursive: true, force: true });
    }
  }

  const singleAt = (createdOn: string) => ({
    created_on: createdOn,
    versions: [{ percentage: 100, version_id: VERSION_UUID }],
  });

  test('the seed block is present and runs before the version upload', () => {
    expect(script, 'seed run block missing').toBeDefined();
    expect(script).toContain('deployments list');
    // Pin the exact deployments-list target so a repoint at staging fails
    // without executing anything: production env, bound worker name, --json.
    expect(script).toContain('--env production');
    expect(script).toContain('--name "$WORKER_NAME"');
    expect(script).toContain('--json');
    // The offline runner evaluates this one Actions expression to the pinned
    // value; pin both sides so the substitution cannot drift.
    expect(script).toContain('wrangler@${{ env.WRANGLER_VERSION }}');
    expect(productionWorkflow.split('\njobs:\n')[0]!).toContain(
      'WRANGLER_VERSION: 4.150.0',
    );
    // Pin the WORKER_NAME the seed step binds, the same way WRANGLER_VERSION
    // is pinned: the step must wire the production worker name, and the
    // workflow-level default must be the production worker.
    const seedStep = productionWorkflow.match(
      /- name: Require one-time production seed deployment\n([\s\S]*?)        run: \|\n/,
    )?.[1];
    expect(seedStep, 'seed step env block missing').toBeDefined();
    expect(seedStep).toContain('WORKER_NAME: ${{ env.PRODUCTION_WORKER_NAME }}');
    expect(productionWorkflow.split('\njobs:\n')[0]!).toContain(
      'PRODUCTION_WORKER_NAME: "nntune-web"',
    );
    const seedAt = productionWorkflow.indexOf(
      'Require one-time production seed deployment',
    );
    const uploadAt = productionWorkflow.indexOf(
      'Upload reviewed production version',
    );
    expect(seedAt).toBeGreaterThan(-1);
    expect(seedAt).toBeLessThan(uploadAt);
  });

  test('seed queries the production worker deployments list', async () => {
    // The stub records the exact argv the gate passes to npx; assert the full
    // deployments-list invocation so a gate repointed at staging (or at a
    // hardcoded staging worker name) fails instead of replaying the fixture.
    const result = await runSeed(
      JSON.stringify([singleAt('2026-10-18T00:00:00Z')]),
    );
    expect(result.status).toBe(0);
    const args = result.argv.trim().split('\n');
    expect(args).toEqual([
      '--yes',
      'wrangler@4.150.0',
      'deployments',
      'list',
      '--env',
      'production',
      '--name',
      'nntune-web',
      '--json',
    ]);
  });

  test('seed accepts a single-version 100% deployment', async () => {
    const result = await runSeed(JSON.stringify([singleAt('2026-10-18T00:00:00Z')]));
    expect(result.status).toBe(0);
    expect(result.output).toContain('production seed ok');
  });

  test('seed refuses an empty deployments list', async () => {
    const result = await runSeed(JSON.stringify([]));
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('seed');
  });

  test('seed refuses when the newest deployment is split', async () => {
    // Newest is picked by created_on, never by array position (mirrors the
    // shared gradual-promote rollback target): an older single-version
    // deployment does not satisfy the gate while a split is live.
    const result = await runSeed(
      JSON.stringify([
        singleAt('2026-10-18T00:00:00Z'),
        {
          created_on: '2026-10-19T00:00:00Z',
          versions: [
            { percentage: 50, version_id: VERSION_UUID },
            {
              percentage: 50,
              version_id: '87654321-4321-4321-4321-cba987654321',
            },
          ],
        },
      ]),
    );
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('not a single-version 100% deployment');
  });

  test('seed refuses unparseable wrangler output', async () => {
    const result = await runSeed('not json{{{');
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('could not parse');
  });
});

test.describe('production secret-shape gate (workflow wiring pin)', () => {
  test('the secret-shape step runs after the dry-run and before the version upload', () => {
    // The checker itself is covered by ci/test-check-production-secrets-shape.mjs
    // (ci-ok job); this pins the wiring: offline shape proof on the validated
    // SHA after wrangler accepts the config, before any version upload.
    const stepAt = productionWorkflow.indexOf(
      'node ci/check-production-secrets-shape.mjs',
    );
    expect(stepAt).toBeGreaterThan(-1);
    const dryRunAt = productionWorkflow.indexOf(
      'Build production target without deploying',
    );
    const uploadAt = productionWorkflow.indexOf(
      'Upload reviewed production version',
    );
    expect(dryRunAt).toBeGreaterThan(-1);
    expect(uploadAt).toBeGreaterThan(-1);
    expect(stepAt).toBeGreaterThan(dryRunAt);
    expect(stepAt).toBeLessThan(uploadAt);
    // A position check alone cannot prove the step blocks the job (`|| true`,
    // continue-on-error, if: false, or the command demoted to a comment all
    // keep it green), so extract the exact step block and require the run
    // line to be exactly the checker with no neutralizer.
    const stepBlock = productionWorkflow.match(
      /      - name: Require production Worker secret shape before upload\n((?:(?!      - ).*\n)+)/,
    )?.[1];
    expect(stepBlock, 'secret-shape step block missing').toBeDefined();
    const runLine = stepBlock!.match(/^\s*run:\s*(.+?)\s*$/m)?.[1];
    expect(runLine).toBe('node ci/check-production-secrets-shape.mjs');
    const codeLines = stepBlock!.split('\n').filter(
      (line) => !/^\s*#/.test(line),
    );
    expect(
      codeLines.some((line) => /^\s*continue-on-error\s*:/.test(line)),
      'secret-shape step must not set continue-on-error',
    ).toBe(false);
    expect(
      codeLines.some((line) => /^\s*if\s*:/.test(line)),
      'secret-shape step must not set if:',
    ).toBe(false);
  });
});

test.describe('smoke checker (fixture server, CLI contract)', () => {
  let server: Server;
  let base: string;
  let servedSha = VALID_SHA;
  let requireOverride = false;
  let overrideHits = 0;
  let totalHits = 0;

  const routes = (): Record<string, { json?: unknown; html?: string }> => ({
    '/api/health': { json: { ok: true, version: '9.9.9' } },
    '/api/ready': { json: { ok: true } },
    '/api/build': { json: { sha: servedSha } },
    '/': { html: '<html><body>NNTune home</body></html>' },
    '/signup': { html: '<html><body>Get started</body></html>' },
    '/pricing': { html: '<html><body>Plans and pricing</body></html>' },
    '/login': { html: '<html><body>Sign in</body></html>' },
    '/onboarding/byop': { html: '<html><body>BYOP server host</body></html>' },
  });

  test.beforeAll(async () => {
    server = createServer((req: IncomingMessage, res: ServerResponse) => {
      const url = new URL(req.url ?? '/', 'http://fixture');
      totalHits += 1;
      const override = req.headers['cloudflare-workers-version-overrides'];
      if (requireOverride && typeof override !== 'string') {
        res.writeHead(500, { 'content-type': 'text/plain' });
        res.end('override header required');
        return;
      }
      if (typeof override === 'string') overrideHits += 1;
      const route = routes()[url.pathname];
      if (!route) {
        res.writeHead(404, { 'content-type': 'text/plain' });
        res.end('not found');
        return;
      }
      if (route.json !== undefined) {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify(route.json));
      } else {
        res.writeHead(200, { 'content-type': 'text/html' });
        res.end(route.html);
      }
    });
    await new Promise<void>((resolve) => {
      server.listen(0, '127.0.0.1', () => resolve());
    });
    const address = server.address();
    const port = typeof address === 'object' && address ? address.port : 0;
    base = `http://127.0.0.1:${port}`;
  });

  test.afterAll(async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  test.beforeEach(() => {
    servedSha = VALID_SHA;
    requireOverride = false;
    overrideHits = 0;
    totalHits = 0;
  });

  // Async spawn: the fixture server lives on this worker's event loop (see
  // runChild above).
  function runCli(...args: string[]) {
    return runChild(process.execPath, [smokeCli, ...args], {}, 60_000);
  }

  test('healthy fixture passes with the version-override header on every probe', async () => {
    // The fixture 500s any probe without the override header, so exit 0
    // proves the header rode on all 8 probes (3 JSON + 5 HTML).
    requireOverride = true;
    const result = await runCli(
      base,
      '--allow-indexable',
      '--version-override',
      `nntune-web="${VERSION_UUID}"`,
      '--expected-sha',
      VALID_SHA,
    );
    expect(result.status).toBe(0);
    expect(result.output).toContain('smoke ok');
    expect(totalHits).toBe(8);
    expect(overrideHits).toBe(8);
  });

  test('wrong expected SHA fails closed', async () => {
    const result = await runCli(
      base,
      '--allow-indexable',
      '--expected-sha',
      OTHER_SHA,
    );
    expect(result.status).toBe(1);
    expect(result.output).toContain('/api/build sha');
  });

  test('missing base URL is a usage error', async () => {
    expect((await runCli()).status).toBe(2);
  });

  test('malformed version-override is a usage error', async () => {
    const result = await runCli(base, '--version-override', 'bogus');
    expect(result.status).toBe(2);
    expect(result.output).toContain('version-override');
  });
});
