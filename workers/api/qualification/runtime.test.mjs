import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout } from 'node:timers/promises';
import test from 'node:test';
import { Miniflare, convertV4MiniflareOptions } from 'miniflare';

const root = fileURLToPath(new URL('../../', import.meta.url));
const api = 'api/';
const ingest = 'ingest/';

async function generatedModules(prefix) {
  const modules = [];
  async function visit(relative) {
    for (const item of await readdir(path.join(root, relative), { withFileTypes: true })) {
      const name = `${relative}/${item.name}`;
      if (item.isDirectory()) await visit(name);
      else if (/\.(mjs|js|wasm)$/.test(name)) {
        modules.push({
          type: name.endsWith('.wasm') ? 'CompiledWasm' : 'ESModule',
          path: name,
          contents: await readFile(path.join(root, name)),
        });
      }
    }
  }
  await visit(`${prefix}build`);
  assert.ok(modules.some(m => m.type === 'CompiledWasm'), 'actual Wasm artifact required');
  return modules;
}

async function sourceModule(relative) {
  return { type: 'ESModule', path: relative, contents: await readFile(path.join(root, relative), 'utf8') };
}

function worker(name, modules, options = {}) {
  return {
    name, modules, modulesRoot: root,
    compatibilityDate: '2026-10-23',
    // An accidental external fetch must fail, never contact a real service.
    outboundService: () => new Response(null, { status: 502 }),
    ...options,
  };
}

test('generated release artifacts in real workerd', { timeout: 120_000 }, async t => {
  const apiModules = [
    await sourceModule(`${api}qualification/api-probe.mjs`),
    await sourceModule(`${api}src/runtime.mjs`),
    ...await generatedModules(api),
  ];
  const ingestModules = [
    await sourceModule(`${api}qualification/ingest-probe.mjs`),
    ...await generatedModules(ingest),
  ];
  const mode = { GENERATION_PREPARATION_MODE: 'preparation_only' };
  // Ephemeral harness-only keyring/session material, never real credentials.
  // Successful health below must prove this isolate passed config_from_env.
  const configured = {
    ...mode,
    HYPERDRIVE_CACHING_DISABLED: '1',
    RP_ID: 'qualification.invalid',
    RP_ORIGIN: 'https://qualification.invalid',
    SESSION_SECRET_BASE64: randomBytes(32).toString('base64url'),
    VAULT_KEK_BASE64: randomBytes(32).toString('base64url'),
    VAULT_KEK_VERSION: '1',
  };
  // The pinned runtime accepts v5 configs; use its shipped v4 option converter.
  // Resolve module paths from the explicit Workers root, not the harness cwd.
  const options = convertV4MiniflareOptions({
    rootPath: root,
    host: '127.0.0.1',
    port: 0,
    workers: [
      worker('api', apiModules, { bindings: mode }),
      worker('api-configured', apiModules, { bindings: configured }),
      worker('api-disabled', apiModules, { bindings: { GENERATION_PREPARATION_MODE: 'disabled' } }),
      worker('ingest', ingestModules, {
        bindings: mode,
        serviceBindings: {
          API_PREPARATION_PLANS: { name: 'api', entrypoint: 'IngestPreparationPlans' },
          DISABLED_PLANS: { name: 'api-disabled', entrypoint: 'IngestPreparationPlans' },
          PROBE: { name: 'api', entrypoint: 'QualificationProbe' },
          CONFIGURED_PROBE: { name: 'api-configured', entrypoint: 'QualificationProbe' },
          CONFIGURED_PLANS: { name: 'api-configured', entrypoint: 'IngestPreparationPlans' },
        },
        r2Buckets: { INGEST_SCRATCH: 'qualification-scratch' },
        queueProducers: { INGEST_QUEUE: 'qualification-queue' },
        queueConsumers: { 'qualification-queue': {
          maxBatchSize: 1, maxBatchTimeout: 0, maxRetries: 0,
          deadLetterQueue: 'qualification-dead-letter',
        } },
      }),
      worker('dead-letter', [await sourceModule(`${api}qualification/dead-letter-probe.mjs`)], {
        r2Buckets: { INGEST_SCRATCH: 'qualification-scratch' },
        queueConsumers: { 'qualification-dead-letter': { maxBatchSize: 1, maxBatchTimeout: 0 } },
      }),
    ],
  });
  const mf = new Miniflare(options);
  t.after(() => mf.dispose());
  const publicApi = await mf.getWorker('api');
  const entry = await mf.getWorker('ingest');
  const probe = () => entry.fetch('https://qualification.invalid/probe').then(r => r.json());

  await t.test('snapshot actual generated exports, prototypes and facade containment', async () => {
    const { bridgeCalls, ...apiSnapshot } = await probe();
    const ingestSnapshot = await entry.fetch('https://qualification.invalid/abi').then(r => r.json());
    // The locked SDK exports these data classes through the builder's class
    // wrappers. They must not leak through the composition-only public facade.
    const sdkExports = ['ContainerStartupOptions', 'MinifyConfig', 'R2Range', 'default'];
    assert.deepEqual({ api: apiSnapshot, ingest: ingestSnapshot }, {
      api: {
        generated: sdkExports,
        facade: ['IngestPreparationPlans', 'default'],
        generatedPrototype: ['constructor', 'fetch', 'ingest_preparation_plan'],
        publicPrototype: ['constructor', 'fetch'],
        namedPrototype: ['constructor', 'fetch'],
        bridgeOnInstance: 'function',
      },
      ingest: {
        generated: sdkExports, prototype: ['constructor', 'fetch', 'queue'],
      },
    });
    assert.equal(bridgeCalls, 0);
  });

  await t.test('real generated fetch lifecycle: API missing-config refusal and ingest health', async () => {
    const apiResponse = await publicApi.fetch('https://qualification.invalid/healthz');
    assert.equal(apiResponse.status, 500);
    const response = await entry.fetch('https://qualification.invalid/healthz');
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { ok: true });
  });

  await t.test('named binding calls real Wasm bridge and fails closed without DB/keyring', async () => {
    const response = await entry.fetch('https://qualification.invalid/private', {
      method: 'POST', body: new Uint8Array([255]),
    });
    assert.equal(response.status, 503);
    assert.equal(await response.text(), 'preparation unavailable');
  });

  await t.test('non-preparation mode is no-store 503 through the named binding', async () => {
    const response = await entry.fetch('https://qualification.invalid/disabled', { method: 'POST' });
    assert.equal(response.status, 503);
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(await response.text(), 'preparation unavailable');
  });

  await t.test('public paths never select bridge; injected lifecycle failure is redacted', async () => {
    assert.equal((await entry.fetch('https://qualification.invalid/fault')).status, 204);
    const before = (await probe()).bridgeCalls;
    for (const route of ['/', '/ingest_preparation_plan', '/preparation', '/v1/ingest/preparation']) {
      for (const method of ['GET', 'POST', 'PUT']) {
        const response = await publicApi.fetch(`https://qualification.invalid${route}`, {
          method, headers: { 'x-preparation-mode': 'preparation_only' },
          ...(method === 'GET' ? {} : { body: 'ingest_preparation_plan' }),
        });
        assert.notEqual(await response.text(), 'qualification fault must not escape');
        assert.equal((await probe()).bridgeCalls, before);
      }
    }
    const response = await entry.fetch('https://qualification.invalid/private', { method: 'POST' });
    assert.equal(response.status, 503);
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(await response.text(), 'preparation unavailable');
    assert.equal((await probe()).bridgeCalls, before + 1);
  });

  await t.test('configured public router stays isolated while named lifecycle failure is reachable', async () => {
    const publicConfigured = await mf.getWorker('api-configured');
    const configuredProbe = () => entry.fetch('https://qualification.invalid/configured-probe').then(r => r.json());
    const health = async () => {
      const response = await publicConfigured.fetch('https://qualification.invalid/healthz');
      assert.equal(response.status, 200, 'missing config must not short-circuit the route matrix');
      assert.deepEqual(await response.json(), { ok: true, build_sha: 'unknown' });
    };
    await health();
    assert.equal((await configuredProbe()).bridgeCalls, 0, 'configured isolate must be independent');
    assert.equal((await entry.fetch('https://qualification.invalid/configured-fault')).status, 204);
    const before = (await configuredProbe()).bridgeCalls;
    for (const route of ['/', '/ingest_preparation_plan', '/preparation', '/v1/ingest/preparation']) {
      for (const method of ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS']) {
        const response = await publicConfigured.fetch(`https://qualification.invalid${route}?entrypoint=IngestPreparationPlans`, {
          method,
          headers: {
            origin: configured.RP_ORIGIN,
            'x-preparation-mode': 'preparation_only',
            'content-type': 'application/octet-stream',
          },
          ...(['GET', 'HEAD'].includes(method) ? {} : { body: new Uint8Array([255]) }),
        });
        assert.equal(response.status, 404, `configured public ${method} ${route} must stay unserved`);
        await response.arrayBuffer();
        assert.equal((await configuredProbe()).bridgeCalls, before);
      }
    }
    // Positive control: the same injected bridge really is reachable through
    // the named binding. Public 404s cannot be explained by a broken injection.
    const response = await entry.fetch('https://qualification.invalid/configured-private', { method: 'POST' });
    assert.equal(response.status, 503);
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(await response.text(), 'preparation unavailable');
    assert.equal((await configuredProbe()).bridgeCalls, before + 1);
    await health();
    assert.equal((await configuredProbe()).bridgeCalls, before + 1);
  });

  await t.test('real R2 and Queue transport reaches generated consumer; unknown retains object', async () => {
    assert.equal((await entry.fetch('https://qualification.invalid/seed')).status, 202);
    let result;
    for (let i = 0; i < 400; i++) {
      result = await entry.fetch('https://qualification.invalid/retained').then(r => r.json());
      if (result.delivered && result.refused) break;
      await setTimeout(50);
    }
    assert.deepEqual(result.delivered, { count: 1 }, 'Queue must actually reach generated consumer');
    assert.deepEqual(result.refused, [{ version: 1, id: 'qualification-pointer' }],
      'native retry must reach the dead-letter observer; callback completion is not settlement');
    assert.deepEqual(result.bytes, [1, 2, 3]);
  });
});
