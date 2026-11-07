import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { lstat, mkdtemp, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout } from 'node:timers/promises';
import test from 'node:test';
import { Miniflare, convertV4MiniflareOptions } from 'miniflare';
import { database, seed, sql, state } from './database.mjs';
import { flacFixture, metadataBlocks } from './flac-fixture.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const api = 'api/';
const ingest = 'ingest/';

async function generatedModules(prefix, folder = 'build') {
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
  await visit(`${prefix}${folder}`);
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

async function compositeDelivery(bucket) {
  for (let i = 0; i < 400; i++) {
    assert.equal(await bucket.get('qualification-refused'), null, 'replay must not reach DLQ');
    const marker = await bucket.get('qualification-composite-delivered');
    if (marker) return marker.json();
    await setTimeout(50);
  }
  assert.fail('new Queue callback completion marker required');
}

test('generated release artifacts in real workerd', { timeout: 120_000 }, async t => {
  const apiModules = [
    await sourceModule(`${api}qualification/api-probe.mjs`),
    await sourceModule(`${api}src/runtime.mjs`),
    ...await generatedModules(api),
  ];
  const ingestModules = [
    await sourceModule(`${api}qualification/ingest-probe.mjs`),
    ...await generatedModules(ingest, 'build-ordinary'),
  ];
  const compositeModules = [
    await sourceModule(`${api}qualification/composite-probe.mjs`),
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
  const persistence = await mkdtemp(path.join(tmpdir(), 'preparation-qualification-'));
  let mf;
  t.after(async () => {
    if (mf) await mf.dispose();
    assert.ok((await lstat(persistence)).isDirectory());
    await rm(persistence, { recursive: true });
  });
  const options = convertV4MiniflareOptions({
    rootPath: root,
    resourcePersistencePath: persistence,
    host: '127.0.0.1',
    port: 0,
    workers: [
      worker('api', apiModules, { bindings: mode }),
      worker('api-configured', apiModules, { bindings: configured }),
      worker('api-disabled', apiModules, { bindings: { GENERATION_PREPARATION_MODE: 'disabled' } }),
      worker('api-database', [
        await sourceModule(`${api}src/runtime.mjs`), ...await generatedModules(api),
      ], { bindings: configured, hyperdrives: { HYPERDRIVE: database } }),
      worker('composite', compositeModules, {
        bindings: { ...mode, HYPERDRIVE_CACHING_DISABLED: '1' },
        hyperdrives: { LIBRARY_DB: database },
        serviceBindings: { API_PREPARATION_PLANS: { name: 'api-database', entrypoint: 'IngestPreparationPlans' } },
        r2Buckets: { INGEST_SCRATCH: 'qualification-composite' },
        queueProducers: { INGEST_QUEUE: 'qualification-composite-queue' },
        queueConsumers: { 'qualification-composite-queue': {
          maxBatchSize: 1, maxBatchTimeout: 0, maxRetries: 0,
          deadLetterQueue: 'qualification-composite-dead-letter',
        } },
      }),
      worker('composite-dead-letter', [await sourceModule(`${api}qualification/dead-letter-probe.mjs`)], {
        r2Buckets: { INGEST_SCRATCH: 'qualification-composite' },
        queueConsumers: { 'qualification-composite-dead-letter': { maxBatchSize: 1, maxBatchTimeout: 0 } },
      }),
      worker('composite-flac', compositeModules, {
        bindings: { ...mode, HYPERDRIVE_CACHING_DISABLED: '1' },
        hyperdrives: { LIBRARY_DB: database },
        serviceBindings: { API_PREPARATION_PLANS: { name: 'api-database', entrypoint: 'IngestPreparationPlans' } },
        r2Buckets: { INGEST_SCRATCH: 'qualification-flac' },
        queueProducers: { INGEST_QUEUE: 'qualification-flac-queue' },
        queueConsumers: { 'qualification-flac-queue': {
          maxBatchSize: 1, maxBatchTimeout: 0, maxRetries: 0,
          deadLetterQueue: 'qualification-flac-dead-letter',
        } },
      }),
      worker('flac-dead-letter', [await sourceModule(`${api}qualification/dead-letter-probe.mjs`)], {
        r2Buckets: { INGEST_SCRATCH: 'qualification-flac' },
        queueConsumers: { 'qualification-flac-dead-letter': { maxBatchSize: 1, maxBatchTimeout: 0 } },
      }),
      worker('ingest', ingestModules, {
        bindings: mode,
        serviceBindings: {
          API_PREPARATION_PLANS: { name: 'api', entrypoint: 'IngestPreparationPlans' },
          DISABLED_PLANS: { name: 'api-disabled', entrypoint: 'IngestPreparationPlans' },
          PROBE: { name: 'api', entrypoint: 'QualificationProbe' },
          CONFIGURED_PROBE: { name: 'api-configured', entrypoint: 'QualificationProbe' },
          CONFIGURED_PLANS: { name: 'api-configured', entrypoint: 'IngestPreparationPlans' },
          CONFIGURED_PUBLIC: { name: 'api-configured' },
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
  assert.equal(options.resourcePersistencePath, persistence, 'converter must retain explicit persistence');
  mf = new Miniflare(options);
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
    const configuredProbe = () => entry.fetch('https://qualification.invalid/configured-probe').then(r => r.json());
    // Construct Origin inside workerd: the Node binding proxy's own host guard
    // otherwise rejects it before the application sees any matrix request.
    const publicConfigured = (route, init) => entry.fetch(`https://qualification.invalid/configured-public${route}`, init);
    const health = async () => {
      const response = await publicConfigured('/healthz');
      assert.equal(response.status, 200, 'missing config must not short-circuit the route matrix');
      assert.equal(response.headers.get('access-control-allow-origin'), configured.RP_ORIGIN,
        'the real Rust router must receive the configured application Origin');
      assert.deepEqual(await response.json(), { ok: true, build_sha: 'unknown' });
    };
    await health();
    assert.equal((await configuredProbe()).bridgeCalls, 0, 'configured isolate must be independent');
    assert.equal((await entry.fetch('https://qualification.invalid/configured-fault')).status, 204);
    const before = (await configuredProbe()).bridgeCalls;
    for (const route of ['/', '/ingest_preparation_plan', '/preparation', '/v1/ingest/preparation']) {
      for (const method of ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS']) {
        const response = await publicConfigured(`${route}?entrypoint=IngestPreparationPlans`, {
          method,
          headers: {
            'x-preparation-mode': 'preparation_only',
            'content-type': 'application/octet-stream',
          },
          ...(['GET', 'HEAD'].includes(method) ? {} : { body: new Uint8Array([255]) }),
        });
        // The existing scope map covers POST /v1/ingest/*, so its generic
        // browser preflight is 204 even though this path has no handler.
        // That empty response must not be mistaken for a private bridge call.
        const preflight = method === 'OPTIONS' && route === '/v1/ingest/preparation';
        assert.equal(response.status, preflight ? 204 : 404,
          `configured public ${method} ${route} must keep its router posture`);
        assert.equal(response.headers.get('access-control-allow-origin'), configured.RP_ORIGIN);
        assert.match(response.headers.get('x-trace-id') ?? '', /^[a-f0-9]{32}$/,
          'every matrix response must come from the Rust router, not a harness/proxy');
        if (preflight) {
          assert.equal(response.headers.get('access-control-allow-methods'),
            'GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS');
          assert.equal(response.headers.get('access-control-allow-headers'), 'content-type, authorization');
          assert.equal((await response.arrayBuffer()).byteLength, 0);
        } else {
          assert.equal(response.headers.get('access-control-allow-methods'), null);
          assert.equal(response.headers.get('access-control-allow-headers'), null);
          await response.arrayBuffer();
        }
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

  await t.test('Rust producer freezes real R2 and sealed DB plan, sends Queue and recovers read-only', async () => {
    seed();
    const composite = await mf.getWorker('composite');
    const bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite');
    assert.deepEqual(await composite.fetch('https://qualification.invalid/abi').then(r => r.json()),
      ['constructor', 'fetch', 'qualification_prepare', 'qualification_recover', 'queue']);
    // Fixed synthetic fixture only. No caller identity or startup is selected by
    // the ordinary fetch router (whose generated prototype was pinned above).
    const prepare = () => composite.fetch('https://qualification.invalid/prepare', { method: 'POST' });
    const initial = state();
    for (const fault of ['offset', 'length', 'string', 'suffix', 'null', 'version', 'size', 'short', 'long']) {
      const response = await composite.fetch(`https://qualification.invalid/prepare-range-fault/${fault}`, {
        method: 'POST',
      });
      assert.equal(response.status, 503, `malformed R2 range response must refuse: ${fault}`);
      assert.equal(response.headers.get('x-qualification-range-reads'), '1', 'fault must reach the real R2 GET');
      assert.equal(await response.text(), 'qualification unavailable');
      assert.equal(state(), initial, `range refusal must precede DB dispatch: ${fault}`);
      assert.equal((await bucket.list()).objects.length, 1, 'refusal retains the tagged R2 object');
    }
    assert.equal((await prepare()).status, 202, 'actual Rust producer must acknowledge its Queue send');
    const row = JSON.parse(sql(`SELECT row_to_json(q) FROM (
      SELECT id,object_id,object_version,plan_id,plaintext_bytes,plaintext_sha256,send_acknowledged
      FROM ingest_file_dispatches WHERE job_id = 'qualification-job') q;`));
    assert.equal(row.send_acknowledged, true);
    assert.equal(row.plaintext_bytes, 65_537);
    assert.equal(sql(`SELECT count(*) FROM ingest_preparation_plans
      WHERE job_id = 'qualification-job' AND octet_length(encrypted) = plaintext_bytes + 16;`), '1');
    assert.equal(sql(`SELECT count(*) FROM posted_file_job_attempts
      WHERE job_id = 'qualification-job' AND attempt = 1;`), '1');
    assert.equal(sql(`SELECT count(*) FROM posted_file_attempt_receipts
      WHERE job_id = 'qualification-job';`), '1');
    const expected = Uint8Array.from({ length: 65_537 }, (_, i) => i % 251);
    assert.equal(row.plaintext_sha256, createHash('sha256').update(expected).digest('hex'));
    const objectKey = `prepared-tagged/${row.object_id}`;
    const object = await bucket.get(objectKey);
    assert.ok(object, 'only the Rust producer wrote the tagged object');
    assert.equal(object.version, row.object_version);
    assert.deepEqual(new Uint8Array(await object.arrayBuffer()), expected);
    const persisted = state();
    let delivered;
    for (let i = 0; i < 400; i++) {
      assert.equal(await bucket.get('qualification-refused'), null, 'successful recovery must not reach DLQ');
      const marker = await bucket.get('qualification-composite-delivered');
      if (marker) { delivered = await marker.json(); break; }
      await setTimeout(50);
    }
    assert.deepEqual(delivered, { count: 1 }, 'real Queue callback must positively observe Rust recovery');
    assert.equal(state(), persisted, 'Queue recovery must not start or renew attempts');
    const pointer = { version: 1, id: row.id };
    const recover = body => composite.fetch('https://qualification.invalid/recover', {
      method: 'POST', body: JSON.stringify(body),
    });
    assert.equal((await recover(pointer)).status, 204);
    assert.equal(state(), persisted, 'explicit duplicate recovery is read-only');
    assert.equal((await prepare()).status, 202, 'exact producer replay reuses the same custody');
    assert.equal(state(), persisted, 'producer replay must not remint or renew');
    assert.equal((await bucket.get(objectKey)).version, row.object_version, 'create-only replay preserves R2 version');
    assert.equal((await recover({ version: 1, id: 'unknown-qualification-pointer' })).status, 503);
    assert.equal(state(), persisted, 'unknown pointer cannot borrow current authority');
    const keys = (await bucket.list()).objects.map(o => o.key).sort();
    assert.deepEqual(keys, [objectKey, 'qualification-composite-delivered'].sort(),
      'no plaintext plan object may be stored in R2');
    // Test-only replacement gives a new version even at the same key. Recovery
    // must refuse, leave exact DB custody untouched and never delete the object.
    await bucket.put(objectKey, new Uint8Array([9]));
    assert.equal((await recover(pointer)).status, 503);
    assert.equal(state(), persisted);
    assert.ok(await bucket.get(objectKey), 'unknown/version mismatch retains R2');
  });

  await t.test('real FLAC tag rewrite feeds Rust R2, sealed plan and Queue recovery', async t => {
    seed(true);
    assert.equal(sql(`SELECT count(DISTINCT content_sha256) FROM ingest_jobs
      WHERE id IN ('qualification-job','qualification-flac-job');`), '2',
      'both fixture jobs must coexist with globally distinct upload identities');
    assert.equal(sql(`SELECT count(*) FROM ingest_jobs j JOIN ingest_originals o
      ON o.job_id = j.id AND o.content_sha256 = j.content_sha256
      WHERE j.id IN ('qualification-job','qualification-flac-job')
        AND j.idempotency_key = 'admin:' || j.content_sha256;`), '2',
      'each fixture must keep its admission key and original custody bound to its upload');
    let composite = await mf.getWorker('composite-flac');
    let bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite-flac');
    const { input, audio, samples } = flacFixture();
    const prepare = body => composite.fetch('https://qualification.invalid/prepare-flac', {
      method: 'POST', body,
    });
    const badCrc = Buffer.from(input);
    badCrc[badCrc.length - 1] ^= 1;
    const badTotal = Buffer.from(input);
    badTotal[25] ^= 1;
    const initial = state();
    for (const body of [Buffer.from('invalid'), badCrc, badTotal, Buffer.alloc(128 * 1024 + 1)]) {
      const response = await prepare(body);
      assert.equal(response.status, 503, 'invalid structural FLAC must fail before preparation');
      assert.equal(await response.text(), 'qualification unavailable');
      assert.equal(state(), initial);
      assert.deepEqual((await bucket.list()).objects, [], 'parser refusal must not write R2');
    }
    let pending;
    let custody;
    let pendingVersion;
    for (const fault of ['before', 'after']) {
      const response = await composite.fetch(`https://qualification.invalid/prepare-flac?queue-fault=${fault}`, {
        method: 'POST', body: input,
      });
      assert.equal(response.status, 503, `unacknowledged Queue send must remain pending: ${fault}`);
      assert.equal(await response.text(), 'qualification unavailable');
      assert.equal(response.headers.get('x-qualification-send-attempts'), '1');
      assert.equal(response.headers.get('x-qualification-send-accepted'), fault === 'after' ? '1' : '0');
      assert.equal(sql(`SELECT send_acknowledged FROM ingest_file_dispatches
        WHERE job_id = 'qualification-flac-job';`), 'f');
      if (fault === 'before') {
        pending = state();
        custody = state(true);
        const dispatch = JSON.parse(sql(`SELECT row_to_json(q) FROM (
          SELECT id,object_id,object_version FROM ingest_file_dispatches
          WHERE job_id = 'qualification-flac-job') q;`));
        pendingVersion = dispatch;
        assert.equal(await bucket.get('qualification-composite-delivered'), null,
          'pre-send refusal must not invoke the native consumer');
        assert.equal((await composite.fetch('https://qualification.invalid/recover', {
          method: 'POST', body: JSON.stringify({ version: 1, id: dispatch.id }),
        })).status, 204, 'persisted preparation is recoverable even before Queue acceptance');
        await t.test('fresh workerd recovers an unsent dispatch before retrying Queue', async () => {
          const key = `prepared-tagged/${dispatch.object_id}`;
          const object = await bucket.get(key);
          assert.ok(object);
          const bytes = Buffer.from(await object.arrayBuffer());
          const objects = (await bucket.list()).objects.map(o => ({ key: o.key, version: o.version }));
          assert.equal(object.version, dispatch.object_version);
          assert.equal(state(), pending, 'read-only recovery before restart must preserve all rows');
          // Only committed DB/R2 custody and the same ephemeral keyring survive.
          // The failed send accepted no Queue message, so none can supply a marker.
          await mf.dispose();
          mf = undefined;
          mf = new Miniflare(options);
          composite = await mf.getWorker('composite-flac');
          bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite-flac');
          assert.equal((await composite.fetch('https://qualification.invalid/healthz')).status, 200);
          assert.equal(state(), pending, 'runtime startup must not acknowledge or renew the dispatch');
          assert.equal(await bucket.get('qualification-composite-delivered'), null);
          const recover = pointer => composite.fetch('https://qualification.invalid/recover', {
            method: 'POST', body: JSON.stringify(pointer),
          });
          assert.equal((await recover({ version: 1, id: dispatch.id })).status, 204,
            'fresh Rust and private API isolates must open unacknowledged custody');
          assert.equal((await recover({ version: 1, id: dispatch.id })).status, 204);
          assert.equal((await recover({ version: 1, id: 'unknown-unsent-pointer' })).status, 503);
          assert.equal(state(), pending, 'fresh recovery/replay must preserve full pending authority');
          assert.equal(sql(`SELECT send_acknowledged FROM ingest_file_dispatches
            WHERE job_id = 'qualification-flac-job';`), 'f');
          const retained = await bucket.get(key);
          assert.ok(retained, 'restart must not reseed or replace the prepared object');
          assert.equal(retained.version, dispatch.object_version);
          assert.deepEqual(Buffer.from(await retained.arrayBuffer()), bytes);
          assert.deepEqual((await bucket.list()).objects.map(o => ({ key: o.key, version: o.version })), objects);
          assert.equal(await bucket.get('qualification-composite-delivered'), null,
            'direct restart recovery must not send Queue work');
          assert.equal(await bucket.get('qualification-refused'), null);
        });
      } else {
        assert.deepEqual(await compositeDelivery(bucket), { count: 1 },
          'lost acknowledgement must follow a real send and successful Rust Queue recovery');
      }
      assert.equal(state(), pending, 'failed send/recovery must preserve full persisted rows');
      const retained = await bucket.get(`prepared-tagged/${pendingVersion.object_id}`);
      assert.ok(retained, 'unacknowledged send retains the prepared object');
      assert.equal(retained.version, pendingVersion.object_version);
      assert.equal(await bucket.get('qualification-refused'), null);
    }
    // A fresh marker must come from the ordinary retry, not the lost-reply send.
    await bucket.delete('qualification-composite-delivered');
    assert.equal((await prepare(input)).status, 202);
    assert.deepEqual(await compositeDelivery(bucket), { count: 1 });
    assert.equal(state(true), custody, 'successful retry changes only send acknowledgement');
    assert.equal((await bucket.get(`prepared-tagged/${pendingVersion.object_id}`)).version,
      pendingVersion.object_version, 'retry must not rewrite the prepared R2 object');
    const row = JSON.parse(sql(`SELECT row_to_json(q) FROM (
      SELECT id,object_id,object_version,plaintext_bytes,plaintext_sha256,send_acknowledged
      FROM ingest_file_dispatches WHERE job_id = 'qualification-flac-job') q;`));
    const objectKey = `prepared-tagged/${row.object_id}`;
    const object = await bucket.get(objectKey);
    assert.ok(object);
    assert.equal(object.version, row.object_version);
    const tagged = Buffer.from(await object.arrayBuffer());
    assert.ok(tagged.length > 65_536, 'tagged fixture must cross the real range-reader boundary');
    assert.equal(tagged.length, row.plaintext_bytes);
    assert.equal(row.plaintext_sha256, createHash('sha256').update(tagged).digest('hex'));
    assert.notEqual(row.plaintext_sha256, createHash('sha256').update(input).digest('hex'),
      'dispatch must freeze tagged bytes, not the original upload');
    const rewritten = metadataBlocks(tagged);
    assert.deepEqual(rewritten.blocks.map(b => b.type), [0, 4], 'tagger strips padding and adds comments');
    assert.equal(Number(rewritten.blocks[0].body.readBigUInt64BE(10) & 0xfffffffffn), samples);
    assert.ok(rewritten.blocks[1].body.includes(Buffer.from('TITLE=Qualification title')));
    assert.deepEqual(rewritten.audio, audio, 'tag rewrite must preserve every structural audio frame');
    assert.equal(row.send_acknowledged, true);
    assert.equal(sql(`SELECT count(*) FROM ingest_preparation_plans
      WHERE job_id = 'qualification-flac-job' AND octet_length(encrypted) = plaintext_bytes + 16;`), '1');
    const persisted = state();
    let delivered;
    for (let i = 0; i < 400; i++) {
      assert.equal(await bucket.get('qualification-refused'), null);
      const marker = await bucket.get('qualification-composite-delivered');
      if (marker) { delivered = await marker.json(); break; }
      await setTimeout(50);
    }
    assert.deepEqual(delivered, { count: 1 });
    assert.equal(state(), persisted, 'Queue recovery does not mutate startup authority');
    // Consume the replay before disposing this runtime; an old pending callback
    // must not supply the post-restart replay's completion marker.
    await bucket.delete('qualification-composite-delivered');
    assert.equal((await prepare(input)).status, 202);
    assert.deepEqual(await compositeDelivery(bucket), { count: 1 });
    assert.equal(await bucket.get('qualification-refused'), null);
    assert.equal(state(), persisted, 'tagging and producer replay preserve exact custody');
    assert.equal((await bucket.get(objectKey)).version, row.object_version);
    assert.deepEqual((await bucket.list()).objects.map(o => o.key).sort(),
      [objectKey, 'qualification-composite-delivered'].sort(), 'R2 stores no plaintext plan');
  });

  await t.test('fresh workerd recovers persisted FLAC preparation without restarting authority', async () => {
    const row = JSON.parse(sql(`SELECT row_to_json(q) FROM (
      SELECT id,object_id,object_version,plaintext_bytes,plaintext_sha256
      FROM ingest_file_dispatches WHERE job_id = 'qualification-flac-job') q;`));
    const pointer = { version: 1, id: row.id };
    const objectKey = `prepared-tagged/${row.object_id}`;
    const persisted = state();
    // Shut down the runtime, not just a Rust client or handle. Only the external
    // DB, explicit R2 persistence and the same ephemeral API keyring survive.
    // Graceful disposal is not abrupt-process/ambiguous-commit fault injection.
    await mf.dispose();
    mf = undefined;
    mf = new Miniflare(options);
    const composite = await mf.getWorker('composite-flac');
    assert.equal((await composite.fetch('https://qualification.invalid/healthz')).status, 200);
    const recover = body => composite.fetch('https://qualification.invalid/recover', {
      method: 'POST', body: JSON.stringify(body),
    });
    assert.equal((await recover(pointer)).status, 204, 'fresh Rust/service isolates must open persisted custody');
    assert.equal(state(), persisted, 'restart recovery must not acquire, remint or renew');
    const bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite-flac');
    const object = await bucket.get(objectKey);
    assert.ok(object, 'R2 must survive runtime replacement, not be reseeded by the harness');
    assert.equal(object.version, row.object_version);
    const tagged = Buffer.from(await object.arrayBuffer());
    assert.equal(tagged.length, row.plaintext_bytes);
    assert.equal(createHash('sha256').update(tagged).digest('hex'), row.plaintext_sha256);
    assert.equal((await recover(pointer)).status, 204, 'duplicate restart delivery remains read-only');
    assert.equal((await recover({ version: 1, id: 'unknown-restart-pointer' })).status, 503);
    assert.equal(state(), persisted, 'unknown restart pointer must not borrow persisted authority');
    await bucket.delete('qualification-composite-delivered');
    assert.equal((await composite.fetch('https://qualification.invalid/prepare-flac', {
      method: 'POST', body: flacFixture().input,
    })).status, 202, 'fresh producer reuses the exact stored preparation');
    assert.deepEqual(await compositeDelivery(bucket), { count: 1 }, 'new replay callback must complete');
    assert.equal(state(), persisted, 'producer replay after restart must not rotate authority or custody');
    assert.equal((await bucket.get(objectKey)).version, row.object_version);
    assert.deepEqual((await bucket.list()).objects.map(o => o.key).sort(),
      [objectKey, 'qualification-composite-delivered'].sort(), 'restart must not add plaintext plans');
    assert.equal(await bucket.get('qualification-refused'), null, 'restart replay must not reach DLQ');
  });

  await t.test('R2 stream bounds and deadline refuse without changing recovered custody', { timeout: 40_000 }, async () => {
    const composite = await mf.getWorker('composite-flac');
    const bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite-flac');
    const row = JSON.parse(sql(`SELECT row_to_json(q) FROM (
      SELECT id,object_id,object_version,plaintext_bytes,plaintext_sha256
      FROM ingest_file_dispatches WHERE job_id = 'qualification-flac-job') q;`));
    const pointer = JSON.stringify({ version: 1, id: row.id });
    const recover = route => composite.fetch(`https://qualification.invalid${route}`, {
      method: 'POST', body: pointer,
    });
    const persisted = state();
    const objects = () => bucket.list().then(result => result.objects
      .map(object => ({ key: object.key, version: object.version })).sort((a, b) => a.key.localeCompare(b.key)));
    const custody = await objects();
    assert.equal((await recover('/recover')).status, 204, 'genuine R2 recovery must succeed before faults');
    for (const fault of ['oversized', 'chunked-long', 'stream-error', 'stalled']) {
      const start = performance.now();
      const response = await recover(`/recover-range-fault/${fault}`);
      const elapsed = performance.now() - start;
      assert.equal(response.status, 503, `R2 stream must refuse: ${fault}`);
      assert.equal(response.headers.get('x-qualification-range-reads'), '1', 'fault must reach actual R2 GET');
      assert.equal(await response.text(), 'qualification unavailable', 'R2 stream details must remain redacted');
      if (fault === 'stalled') {
        assert.ok(elapsed >= 9_000 && elapsed < 25_000, 'stalled R2 body must reach the existing deadline');
      } else {
        assert.ok(elapsed < 8_000, 'byte-bound and stream-error refusal must not depend on the deadline');
      }
      assert.equal(state(), persisted, `failed R2 read must not renew, rotate or remint: ${fault}`);
      const object = await bucket.get(`prepared-tagged/${row.object_id}`);
      assert.ok(object, 'failed read must retain exact R2 custody');
      assert.equal(object.version, row.object_version);
      const bytes = Buffer.from(await object.arrayBuffer());
      assert.equal(bytes.length, row.plaintext_bytes);
      assert.equal(createHash('sha256').update(bytes).digest('hex'), row.plaintext_sha256);
      assert.deepEqual(await objects(), custody, 'direct refusal must neither write objects nor invoke Queue');
      assert.equal(await bucket.get('qualification-refused'), null);
      assert.equal((await recover('/recover')).status, 204, 'fault must not poison genuine recovery');
      assert.equal(state(), persisted);
    }
  });

  await t.test('private plan response bounds and deadline refuse without changing recovered custody', { timeout: 40_000 }, async () => {
    const composite = await mf.getWorker('composite-flac');
    const bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite-flac');
    const row = JSON.parse(sql(`SELECT row_to_json(q) FROM (
      SELECT id,object_id,object_version,plaintext_bytes,plaintext_sha256
      FROM ingest_file_dispatches WHERE job_id = 'qualification-flac-job') q;`));
    const pointer = JSON.stringify({ version: 1, id: row.id });
    const recover = route => composite.fetch(`https://qualification.invalid${route}`, {
      method: 'POST', body: pointer,
    });
    const persisted = state();
    const keys = (await bucket.list()).objects.map(o => o.key).sort();
    assert.equal((await recover('/recover')).status, 204, 'real private plan must open before fault injection');
    for (const fault of ['status', 'redirect', 'type', 'digest', 'short', 'stream-error', 'oversized', 'stalled']) {
      const start = performance.now();
      const response = await recover(`/recover-plan-fault/${fault}`);
      const elapsed = performance.now() - start;
      assert.equal(response.status, 503, `private plan response must fail closed: ${fault}`);
      assert.equal(response.headers.get('x-qualification-plan-calls'), '1', 'real plan open must precede each response fault');
      assert.equal(await response.text(), 'qualification unavailable', 'private body/error detail must not escape');
      if (fault === 'oversized') {
        assert.ok(elapsed < 8_000, 'open oversize stream must refuse before the 10-second deadline');
      } else if (fault === 'stalled') {
        assert.ok(elapsed >= 9_000 && elapsed < 25_000, 'stalled read must reach the existing bounded deadline');
      }
      assert.equal(state(), persisted, `failed recovery must not renew, rotate or remint: ${fault}`);
      const object = await bucket.get(`prepared-tagged/${row.object_id}`);
      assert.ok(object, `failed recovery retains R2: ${fault}`);
      assert.equal(object.version, row.object_version);
      const bytes = Buffer.from(await object.arrayBuffer());
      assert.equal(bytes.length, row.plaintext_bytes);
      assert.equal(createHash('sha256').update(bytes).digest('hex'), row.plaintext_sha256);
      assert.deepEqual((await bucket.list()).objects.map(o => o.key).sort(), keys);
      assert.equal(await bucket.get('qualification-refused'), null, 'direct refusal must not create Queue work');
      assert.equal((await recover('/recover')).status, 204, 'fault must not poison later genuine recovery');
      assert.equal(state(), persisted);
    }
  });
});
