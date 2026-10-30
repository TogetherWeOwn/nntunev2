import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

// Imported by the already-required workflows.spec.ts. Keeping this support
// bank outside *.spec.ts avoids adding an unrelated browser journey/matrix
// row: every offline assertion still runs through test:unit -> e2e-lint -> ci-ok.
// The Python file's header is the executable CLI/fixture contract for the two
// stdlib-only seams. No product implementation is replaced by a test double.
const root = fileURLToPath(new URL('../..', import.meta.url));
const read = (path: string) => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const code = (text: string) => text.split('\n').filter(line => !/^\s*#/.test(line)).join('\n')
  .replace(/\\\r?\n\s*/g, ' ');
const deploy = code(read('.github/workflows/deploy.yml'));
const production = code(read('.github/workflows/deploy-production.yml'));
const e2e = code(read('.github/workflows/e2e-staging.yml'));
const sharedAction = 'TogetherWeOwn/.github/actions/calver@1508f2c10cb83bc0677e8e559ff796a55c66e737';

function job(source: string, name: string): string {
  const body = source.split('\njobs:\n')[1] ?? source;
  const parts = [...body.matchAll(/^  ([a-zA-Z0-9_-]+):\n/gm)];
  const index = parts.findIndex(match => match[1] === name);
  expect(index, `required job ${name}`).toBeGreaterThan(-1);
  return body.slice(parts[index]!.index, parts[index + 1]?.index ?? body.length);
}

function steps(body: string): string[] {
  return body.split(/^      - /m).slice(1);
}

function calver(body: string): string[] {
  return steps(body).filter(step => /uses: TogetherWeOwn\/\.github\/actions\/calver@/.test(step));
}

function runOfflineClass(name: string, file = 'ci/test-web-release-acceptance.py') {
  const result = spawnSync('python3', [file, name, '-v'], {
    cwd: root, encoding: 'utf8', timeout: 120_000, maxBuffer: 4 * 1024 * 1024,
    env: { PATH: process.env.PATH, PYTHONDONTWRITEBYTECODE: '1', PYTHONIOENCODING: 'utf-8' },
  });
  expect(result.error).toBeUndefined();
  expect(result.signal).toBeNull();
  expect(result.status, result.stdout + result.stderr).toBe(0);
}

// Evaluate only the small Actions concurrency-expression subset we use, not
// arbitrary JS or workflow scripts. A run-id-only main lock fails the second
// main push; a nested reused group fails the child-lock comparison.
function groupValue(template: string, context: Record<string, string | number | boolean>): string {
  const atom = (input: string): string | number | boolean => {
    const text = input.trim();
    if (/^'[^']*'$/.test(text)) return text.slice(1, -1);
    if (text === 'true' || text === 'false') return text === 'true';
    expect(Object.hasOwn(context, text), `unsupported concurrency atom ${text}`).toBe(true);
    return context[text]!;
  };
  const condition = (input: string): string | number | boolean => {
    const compare = input.match(/^(.*?)\s+(==|!=)\s+(.*?)$/);
    if (!compare) return atom(input);
    const equal = atom(compare[1]!) === atom(compare[3]!);
    return compare[2] === '==' ? equal : !equal;
  };
  return template.replace(/\$\{\{\s*(.*?)\s*\}\}/g, (_match, expression: string) => {
    let value: string | number | boolean = false;
    for (const branch of expression.split('||')) {
      value = true;
      for (const part of branch.split('&&')) {
        value = condition(part.trim());
        if (!value) break;
      }
      if (value) break;
    }
    return String(value);
  });
}

function concurrency(source: string, indent: number): { group: string; cancel: string } {
  const space = ' '.repeat(indent);
  const match = source.match(new RegExp(`^${space}concurrency:\\n${space}  group: (.+)\\n${space}  cancel-in-progress: (.+)$`, 'm'));
  expect(match, 'transaction concurrency boundary missing').not.toBeNull();
  return { group: match![1]!, cancel: match![2]! };
}

const mainContext = {
  'github.workflow': 'deploy', 'github.ref': 'refs/heads/main',
  'github.event_name': 'push', 'github.event.pull_request.number': false,
  'github.run_id': 4242,
};

test.describe('complete immutable web artifact and honest executed acceptance', () => {
  test('offline package, inventory, safe extraction and metadata-only rendering', () => {
    test.setTimeout(150_000);
    runOfflineClass('ArtifactTests');
  });

  test('offline exact-run/attempt jobs, steps, pagination and artifact certification', () => {
    test.setTimeout(150_000);
    runOfflineClass('EvidenceTests');
  });

  test('producer bundles retain the complete sibling-server closure without a consumer build', () => {
    test.setTimeout(150_000);
    runOfflineClass('ProducerModuleClosureTests', 'ci/test-web-release-boundaries.py');
  });

  test('malformed nested inventory and evidence refuse structurally without usable output', () => {
    test.setTimeout(150_000);
    runOfflineClass('MalformedBoundaryTests', 'ci/test-web-release-boundaries.py');
  });

  test('GitHub fetch and canary resolve enforce exact immutable selection at the fake HTTP boundary', () => {
    test.setTimeout(150_000);
    runOfflineClass('GitHubTransportTests', 'ci/test-web-release-boundaries.py');
  });

  test('main writer lock spans build, deploy, reusable E2E and publication without a nested same-group deadlock', () => {
    const outer = concurrency(deploy.split('\njobs:\n')[0]!, 0);
    const first = groupValue(outer.group, mainContext);
    expect(groupValue(outer.group, { ...mainContext, 'github.run_id': 4243 })).toBe(first);
    expect(groupValue(outer.cancel, mainContext)).toBe('false');
    expect(groupValue(outer.group, {
      ...mainContext, 'github.event_name': 'pull_request', 'github.ref': 'refs/pull/12/merge',
      'github.event.pull_request.number': 12,
    })).not.toBe(first);
    const nested = concurrency(e2e.split('\njobs:\n')[0]!, 0);
    expect(groupValue(nested.group, mainContext)).not.toBe(first);
    for (const name of ['staging-web', 'staging-api', 'preview-web', 'post-deploy-e2e', 'publish-web-canary']) {
      const body = job(deploy, name);
      if (/^    concurrency:/m.test(body)) {
        expect(groupValue(concurrency(body, 4).group, mainContext), `nested lock ${name}`).not.toBe(first);
      }
    }
  });

  test('the staging build uses a single deferred-source plan and retains that exact plan with the full tree', () => {
    const staging = job(deploy, 'staging-web');
    const planner = calver(staging);
    expect(planner).toHaveLength(1);
    expect(planner[0]).toContain(sharedAction);
    expect(planner[0]).toMatch(/"phase"\s*:\s*"plan"/);
    expect(planner[0]).toMatch(/"channel"\s*:\s*"canary"/);
    expect(planner[0]).toMatch(/"policy"\s*:\s*"staged-promotion"/);
    expect(planner[0]).toMatch(/"source"\s*:/);
    expect(planner[0]).not.toMatch(/"evidence"\s*:/);
    expect(staging).toContain('github.run_id');
    expect(staging).toContain('github.run_attempt');
    expect(staging).toContain('outputs.plan');
    expect(staging).toContain('outputs.version');
    expect(staging).toContain('--release-version');
    expect(staging).toMatch(/web-release-artifact\.py[\s\S]*?\bpackage\b/);
    const buildAt = staging.search(/(?:pnpm (?:run )?|npm run )build\b/);
    expect(buildAt).toBeGreaterThan(staging.indexOf(sharedAction));
    expect(staging.match(/(?:pnpm (?:run )?|npm run )build\b/g)).toHaveLength(1);
    expect(staging.indexOf('web-release-artifact.py')).toBeGreaterThan(buildAt);
    expect(staging.indexOf('Deploy to staging')).toBeGreaterThan(staging.indexOf('web-release-artifact.py'));
    expect(staging).toMatch(/uses: actions\/upload-artifact@[0-9a-f]{40}\b/);
    expect(staging).toMatch(/if-no-files-found: ['"]?error['"]?/);
    expect(staging).toMatch(/path: [^\n]*\.tar\.gz/);
    expect(staging).toContain('artifact-id');
    expect(staging).toContain('artifact-digest');
    expect(staging, 'current same-origin API/CSP policy stays portable').not.toMatch(/PUBLIC_API_BASE_URL:\s*https?:/);
  });

  test('canary publication needs real deployment and E2E, then the stronger certificate, with frozen plan and upload identity', () => {
    const publisher = job(deploy, 'publish-web-canary');
    const needs = publisher.match(/^    needs: \[([^\]]+)\]/m)?.[1]?.split(',').map(value => value.trim()).sort();
    expect(needs).toEqual(['post-deploy-e2e', 'staging-web']);
    expect(publisher).not.toMatch(/always\(\)|continue-on-error:\s*true|\|\|\s*true/);
    expect(publisher).toContain('web-release-evidence.py');
    expect(publisher).toContain('certify');
    const publish = calver(publisher);
    expect(publish).toHaveLength(1);
    expect(publish[0]).toContain(sharedAction);
    expect(publish[0]).toMatch(/"phase"\s*:\s*"publish"/);
    expect(publish[0]).toMatch(/"plan"\s*:/);
    expect(publish[0]).toMatch(/"evidence"\s*:/);
    expect(publish[0]).not.toMatch(/"phase"\s*:\s*"plan"/);
    expect(publisher.indexOf(sharedAction)).toBeGreaterThan(publisher.indexOf('certify'));
    expect(publisher).toContain('contents: write');
    expect(publisher).toContain('actions: read');
    expect(publisher).not.toContain('CLOUDFLARE_API_TOKEN');
    expect(publisher).toContain('needs.staging-web.outputs.plan');
  });

  test('production is still explicit-SHA manual web-only and never rebuilds or substitutes source output', () => {
    const triggers = production.split('\npermissions:\n')[0]!;
    expect(triggers).toContain('workflow_dispatch:');
    expect(triggers).not.toMatch(/^  (push|schedule|workflow_run|workflow_call|pull_request):/m);
    expect(production).toContain('PRODUCTION_DEPLOY_ENABLED: ${{ vars.PRODUCTION_DEPLOY_ENABLED }}');
    expect(production).not.toMatch(/PRODUCTION_DEPLOY_ENABLED:\s*['"]?true['"]?\s*$/m);
    expect(production).not.toMatch(/(?:pnpm (?:run )?|npm run |yarn )build\b/);
    expect(production).not.toMatch(/wrangler[^\n]*\bdeploy\s+--dry-run/);
    expect(production).not.toMatch(/entry\.worker|workers\/(?:api|ingest|mb|stream|repair)/);
    expect(production).not.toMatch(/--var(?:[=\s]|$)/m);
    const consumer = job(production, 'deploy-production');
    expect(consumer).toContain('web-release-artifact.py');
    expect(consumer).toContain('--require-portable');
    expect(consumer).toContain('--plan-id');
    expect(consumer).toContain('render-config');
    expect(consumer).toMatch(/versions upload[^\n]*--no-bundle/);
    expect(consumer).toMatch(/versions upload[^\n]*--config/);
    // The exact rendered config, not the otherwise-clean source default,
    // must be the one checked AND uploaded. Use one shared absolute-path
    // token (literal or job env) across steps with different working dirs.
    const renderTarget = consumer.slice(consumer.indexOf('render-config'))
      .match(/--output\s+("[^"\n]+"|'[^'\n]+'|[^\s\\]+)/)?.[1];
    const checkedTarget = consumer.match(/check-production-secrets-shape\.mjs --config\s+("[^"\n]+"|'[^'\n]+'|[^\s\\]+)/)?.[1];
    const uploadedTarget = consumer.slice(consumer.indexOf('versions upload'))
      .match(/--config\s+("[^"\n]+"|'[^'\n]+'|[^\s\\]+)/)?.[1];
    const unquote = (value: string | undefined) => value?.replace(/^['"]|['"]$/g, '');
    expect(renderTarget, 'render-config --output is required').toBeDefined();
    expect(unquote(checkedTarget)).toBe(unquote(renderTarget));
    expect(unquote(uploadedTarget)).toBe(unquote(renderTarget));
    expect(consumer.indexOf('render-config')).toBeLessThan(consumer.indexOf('Require production-only Cloudflare credentials'));
    expect(consumer.indexOf('Require production Worker secret shape before upload')).toBeLessThan(consumer.indexOf('versions upload'));
    expect(consumer).toContain('Require one-time production seed deployment');
    expect(consumer).toContain('gradual-promote@');
    expect(consumer).toContain('PRODUCTION_CLOUDFLARE_API_TOKEN');
    expect(consumer).toMatch(/environment:\s*\n\s+name: production/);
    const releaseSteps = calver(consumer);
    expect(releaseSteps).toHaveLength(2);
    expect(releaseSteps[0]).toContain(sharedAction);
    expect(releaseSteps[0]).toMatch(/"phase"\s*:\s*"plan"/);
    expect(releaseSteps[0]).toMatch(/"channel"\s*:\s*"stable"/);
    expect(releaseSteps[0]).toMatch(/"sourceCanary"\s*:/);
    expect(consumer).toContain('outputs.plan');
    expect(consumer).toContain('outputs.version');
    expect(consumer).toContain('--release-version');
    expect(releaseSteps[1]).toMatch(/"phase"\s*:\s*"publish"/);
    expect(consumer.indexOf('Verify production serves the promoted build')).toBeGreaterThan(-1);
    expect(consumer.indexOf(releaseSteps[1]!)).toBeGreaterThan(consumer.indexOf('Verify production serves the promoted build'));
    for (const action of releaseSteps) {
      expect(action).toContain(sharedAction);
      expect(action).toContain('runs-on-json:');
    }
  });
});
