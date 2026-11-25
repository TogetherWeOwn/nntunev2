'use strict';

const assert = require('node:assert/strict');
const { execFile, spawnSync } = require('node:child_process');
const { EventEmitter, once } = require('node:events');
const { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join, resolve } = require('node:path');
const { test } = require('node:test');
const { createServer } = require('node:tls');
const { promisify } = require('node:util');
const { qualify } = require('../../../../.github/scripts/private_trial_provider_preflight.cjs');
const { authorization, mediaKey, sha256 } = require('./s3_fixture_protocol.cjs');

const endpoint = 'https://fixture.test:8443';
const region = 'fixture-region';
const bucket = 'nntunev2-private-fixtures-staging';
const generation = 'cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd';
const version = 'disposable+/=?';
const sibling = 'other-version';
const key = `media/00000000000040008000000000000000/${generation}`;
const path = `/${bucket}/${key}`;
const queryFor = pin => `versionId=${encodeURIComponent(pin)}`;
const urlFor = (origin, pin) => `${origin}${path}?${queryFor(pin)}`;
const credential = { access_key: 'FIXTUREACCESS', secret_key: 'fixture-secret' };
const launcher = require.resolve('../../../../.github/scripts/private_trial_provider_preflight.cjs');
const plan = [['HEAD', version], ['HEAD', sibling], ['DELETE', version], ['HEAD', version],
  ['DELETE', version], ['HEAD', version], ['HEAD', sibling]];

async function exercise(scenario = {}) {
  const scope = { bucket, endpoint, region, generation, version, sibling_version: sibling, ...scenario.scope };
  const inventory = new Map([[key, new Set([version, sibling])], [mediaKey, new Set([version])]]);
  const requests = [];
  let deleted = false;
  const send = async (request) => {
    const step = requests.push(request) - 1;
    const [method, pin] = plan[step] ?? [];
    const url = urlFor(endpoint, pin);
    if (request.url !== url || request.method !== method) throw new Error('outside synthetic selection');
    const selected = inventory.get(key);
    const present = selected.has(pin);
    const reply = {
      status: method === 'DELETE' ? 204 : present ? 200 : 404,
      headers: method === 'DELETE' || present ? { 'x-amz-version-id': pin } : {},
      url,
      ...scenario.at?.[step],
    };
    if (method === 'DELETE' && reply.status === 204) {
      if (scenario.wipeHistory) selected.clear();
      else selected.delete(pin);
      deleted = true;
    }
    // Fetch disallows constructing a 204 with a body. Inject that malformed reply for refusal coverage.
    const response = new Response(reply.body, {
      status: reply.body && reply.status === 204 ? 200 : reply.status,
      headers: reply.headers,
    });
    Object.defineProperties(response, { url: { value: reply.url }, status: { value: reply.status } });
    return response;
  };
  const result = await qualify(process.env.INGEST_WASM_JS, endpoint, region,
    JSON.stringify(scope), JSON.stringify(credential), send);
  assert.equal(result, scenario.result ?? 'qualified');
  const expected = plan.slice(0, scenario.steps ?? plan.length);
  assert.deepEqual(requests.map(request => [request.method, request.url]),
    expected.map(([method, pin]) => [method, urlFor(endpoint, pin)]));
  for (const [index, request] of requests.entries()) {
    const [, pin] = expected[index];
    assert.equal(request.redirect, 'manual');
    assert.equal(request.cache, 'no-store');
    assert.equal((await request.arrayBuffer()).byteLength, 0);
    const fields = {
      host: 'fixture.test:8443',
      'x-amz-content-sha256': sha256(Buffer.alloc(0)),
      'x-amz-date': request.headers.get('x-amz-date'),
    };
    assert.equal(request.headers.get('authorization'),
      authorization(request.method, path, queryFor(pin), fields, credential.access_key, credential.secret_key, region));
  }
  assert.deepEqual([...inventory.get(key)], deleted ? (scenario.wipeHistory ? [] : [sibling]) : [version, sibling]);
  assert.deepEqual([...inventory.get(mediaKey)], [version]);
}

test('synthetic provider proof reads, deletes and reconfirms only the selected disposable version', async () => {
  await exercise({ at: { 3: { headers: { 'x-amz-version-id': version } }, 4: { headers: {} } } });
  await exercise({ wipeHistory: true, result: 'unconfirmed' });
});

test('synthetic provider proof stops on invalid selection or ambiguous replies', async (t) => {
  const wrongVersion = { 'x-amz-version-id': 'other-version' };
  const cases = [
    ['scope origin differs', { scope: { endpoint: 'https://foreign.test' }, steps: 0, result: 'refused' }],
    ['scope region differs', { scope: { region: 'foreign-region' }, steps: 0, result: 'refused' }],
    ['scope bucket differs', { scope: { bucket: 'foreign-bucket' }, steps: 0, result: 'refused' }],
    ['key injection', { scope: { generation: '../' + mediaKey }, steps: 0, result: 'refused' }],
    ['unversioned selection', { scope: { version: 'null' }, steps: 0, result: 'refused' }],
    ['sibling aliases the selected version', { scope: { sibling_version: version }, steps: 0, result: 'refused' }],
    ['HEAD denied', 0, { status: 403 }],
    ['selected version absent at entry', 0, { status: 404 }],
    ['missing present version', 0, { headers: {} }],
    ['wrong present version', 0, { headers: wrongVersion }],
    ['duplicate present version', 0, { headers: [['x-amz-version-id', version], ['x-amz-version-id', version]] }],
    ['wrong response origin', 0, { url: urlFor('https://foreign.test', version) }],
    ['redirect', 0, { status: 307 }],
    ['sibling missing before delete', 1, { status: 404 }],
    ['DELETE denied', 2, { status: 403 }],
    ['wrong DELETE version', 2, { headers: wrongVersion }],
    ['nonempty DELETE', 2, { body: 'unexpected' }],
    ['ambiguous 204 framing', 2, { headers: { 'content-length': '1' } }],
    ['post-delete HEAD denied', 3, { status: 403 }],
    ['nonempty HEAD 404', 3, { body: 'unexpected' }],
    ['wrong absent version', 3, { headers: wrongVersion }],
    ['repeat DELETE denied', 4, { status: 403 }],
    ['repeat HEAD reports present', 5, { status: 200 }],
  ];
  for (const [name, step, reply] of cases) {
    await t.test(name, () => exercise(reply
      ? { at: { [step]: reply }, steps: step + 1, result: 'unconfirmed' }
      : step));
  }
});

test('synthetic TLS origin checks signed headers and refuses ambiguous wire replies', async (t) => {
  let requests = [];
  let fault;
  const server = createServer({ key: readFileSync(process.env.S3_FIXTURE_KEY),
    cert: readFileSync(process.env.NODE_EXTRA_CA_CERTS) }, (socket) => {
    socket.on('error', () => {});
    let input = '';
    socket.on('data', (chunk) => {
      input += chunk;
      if (!input.endsWith('\r\n\r\n')) return;
      const step = requests.push(input) - 1;
      const [, pin] = plan[step];
      const status = ['200 OK', '200 OK', '204 No Content', '404 Not Found',
        '204 No Content', '404 Not Found', '200 OK'][step];
      const length = status.startsWith('204') ? '' : 'Content-Length: 7\r\n';
      const extra = step === fault.step && fault.conflictingFraming ? 'Transfer-Encoding: chunked\r\n' : '';
      socket.write(`HTTP/1.1 ${status}\r\nConnection: close\r\n${length}${extra}x-amz-version-id: ${pin}\r\n\r\n`);
      socket.end(step === fault.step && !fault.conflictingFraming ? 'payload' : undefined);
    });
  });
  server.listen(0, '::1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const host = `[::1]:${server.address().port}`;
  const origin = `https://${host}`;
  const scope = { bucket, endpoint: origin, region, generation, version, sibling_version: sibling };
  for (const scenario of [
    { step: -1, mode: '--provider-dispatch' }, { step: -1, mode: '--provider' },
    { step: 2 }, { step: 5 }, { step: 6, conflictingFraming: true },
  ]) {
    requests = [];
    fault = scenario;
    const qualified = fault.step < 0;
    const scopeJson = JSON.stringify(scope);
    if (qualified) {
      const child = await promisify(execFile)(process.execPath,
        [launcher, scenario.mode, process.env.INGEST_WASM_JS], {
          env: {
            NODE_EXTRA_CA_CERTS: process.env.NODE_EXTRA_CA_CERTS,
            PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT: origin,
            PRIVATE_PLAYBACK_STAGING_S3_REGION: region,
            PRIVATE_TRIAL_PREFLIGHT_SCOPE: scopeJson,
            ...(scenario.mode === '--provider-dispatch' ? {
              PRIVATE_TRIAL_PREFLIGHT_PROVIDER_MATCH: 'true',
              PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256: sha256(scopeJson),
            } : {}),
            PRIVATE_TRIAL_PREFLIGHT_S3: JSON.stringify(credential),
          },
        });
      assert.equal(child.stdout, 'Private staging fixture provider preflight: qualified.'
        + ' Matched present HEAD and preserved sibling HEAD. Twice observed empty DELETE 204 and HEAD 404'
        + ' with optional version headers absent or matching.\n');
      assert.equal(child.stderr, '');
    } else {
      assert.equal(await qualify(process.env.INGEST_WASM_JS, origin, region,
        scopeJson, JSON.stringify(credential)), 'unconfirmed');
    }
    assert.equal(requests.length, qualified ? plan.length : fault.step + 1);
    for (const [step, request] of requests.entries()) {
      const [method, pin] = plan[step];
      const [line, ...fields] = request.slice(0, -4).split('\r\n');
      const headers = new Headers(fields.map(field => {
        const colon = field.indexOf(':');
        return [field.slice(0, colon), field.slice(colon + 1)];
      }));
      assert.equal(line, `${method} ${path}?${queryFor(pin)} HTTP/1.1`);
      assert.equal(headers.get('host'), host);
      assert.equal(headers.get('cache-control'), 'no-cache, no-store');
      assert.equal(headers.get('pragma'), 'no-cache');
      const signed = { host, 'x-amz-date': headers.get('x-amz-date'),
        'x-amz-content-sha256': sha256(Buffer.alloc(0)) };
      assert.equal(headers.get('x-amz-content-sha256'), signed['x-amz-content-sha256']);
      assert.equal(headers.get('authorization'),
        authorization(method, path, queryFor(pin), signed, credential.access_key, credential.secret_key, region));
    }
  }
});

test('the operator entry requires explicit provider invocation and prints a fixed verdict', () => {
  const child = spawnSync(process.execPath, [launcher, process.env.INGEST_WASM_JS], { encoding: 'utf8', env: {} });
  assert.equal(child.status, 1);
  assert.equal(child.stdout, 'Private staging fixture provider preflight: refused.\n');
  assert.equal(child.stderr, '');
});

test('the protected summary records an interrupted read and a successful fresh dispatch', async (t) => {
  const root = resolve(__dirname, '../../../..');
  const workflow = readFileSync(join(root, '.github/workflows/staging-trial-provider-preflight.yml'), 'utf8');
  const shell = (name, env) => {
    const step = workflow.split(`      - name: ${name}\n`)[1]?.split('      - ')[0];
    const script = step?.split('        run: |\n')[1];
    assert.ok(script, `missing shell step: ${name}`);
    return promisify(execFile)('bash', ['--noprofile', '--norc', '-eo', 'pipefail', '-c', script], {
      cwd: root, env: { PATH: process.env.PATH, ...env }, timeout: 35_000,
    });
  };
  const scratch = mkdtempSync(join(tmpdir(), 'provider-preflight-'));
  t.after(() => rmSync(scratch, { recursive: true }));
  const bundleDir = join(scratch, 'trial-provider-preflight');
  mkdirSync(bundleDir);
  // Stand in for the WASM verdict at its existing export. The real launcher
  // sends the initial version-pinned read to the generated loopback fixture.
  writeFileSync(join(bundleDir, 'nntune_ingest.js'), `
    exports.qualify_disposable_version = async (endpoint, region, scopeJson) => {
      const scope = JSON.parse(scopeJson);
      const url = new URL('/' + scope.bucket + '/media/00000000000040008000000000000000/'
        + scope.generation + '?versionId=' + encodeURIComponent(scope.version), endpoint);
      const response = await fetch(new Request(url, { method: 'HEAD' }));
      return response.ok ? 'qualified' : 'unconfirmed';
    };
  `);
  const reads = new EventEmitter();
  const server = createServer({ key: readFileSync(process.env.S3_FIXTURE_KEY),
    cert: readFileSync(process.env.NODE_EXTRA_CA_CERTS) }, (socket) => {
    socket.on('error', () => {});
    let request = '';
    socket.on('data', (chunk) => {
      request += chunk;
      if (request.endsWith('\r\n\r\n')) reads.emit('head', request.split('\r\n')[0], socket);
    });
  });
  server.listen(0, '::1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const origin = `https://[::1]:${server.address().port}`;
  const scopeFor = (generation) => JSON.stringify({
    bucket, endpoint: origin, region, generation, version, sibling_version: sibling,
  });
  const probe = (scope) => shell('Run the bound disposable provider probe', {
    RUNNER_TEMP: scratch, NODE_EXTRA_CA_CERTS: process.env.NODE_EXTRA_CA_CERTS,
    PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT: origin,
    PRIVATE_PLAYBACK_STAGING_S3_REGION: region,
    PRIVATE_TRIAL_PREFLIGHT_PROVIDER_MATCH: 'true',
    PRIVATE_TRIAL_PREFLIGHT_SCOPE: scope,
    PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256: sha256(scope),
    PRIVATE_TRIAL_PREFLIGHT_S3: JSON.stringify(credential),
  });
  const summary = async (outcome) => {
    const summaryPath = join(scratch, `summary-${outcome}`);
    await shell('Report provider preflight outcome', { PROBE_OUTCOME: outcome, GITHUB_STEP_SUMMARY: summaryPath });
    return readFileSync(summaryPath, 'utf8');
  };

  const interrupted = probe(scopeFor(generation));
  const checkedInterruption = assert.rejects(interrupted, (error) => {
    // GNU timeout can propagate SIGTERM or return the shell status 128 + 15.
    const termination = error.signal ?? error.code;
    assert.ok(['SIGTERM', 143].includes(termination), `unexpected probe termination: ${termination}`);
    assert.equal(error.stdout, '');
    assert.equal(error.stderr, '');
    return true;
  });
  const [firstRead] = await once(reads, 'head', { signal: AbortSignal.timeout(5000) });
  assert.ok(interrupted.child.kill('SIGTERM'));
  await checkedInterruption;
  assert.equal(firstRead, `HEAD ${path}?${queryFor(version)} HTTP/1.1`);
  for (const outcome of ['failure', 'cancelled', 'skipped']) {
    assert.equal(await summary(outcome), 'Private staging fixture provider preflight: incomplete.\n');
  }

  const freshGeneration = 'edededededed40ed80ededededededed';
  const freshScope = scopeFor(freshGeneration);
  const reviewedSha = 'a'.repeat(40);
  await shell('Admit the reviewed dispatch', {
    REVIEWED_REF: reviewedSha, GITHUB_SHA: reviewedSha, SCOPE_SHA256: sha256(freshScope),
  });
  const retry = probe(freshScope);
  const [retryRead, socket] = await once(reads, 'head', { signal: AbortSignal.timeout(5000) });
  socket.end('HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n');
  const { stdout } = await retry;
  assert.equal(retryRead, `HEAD ${path.replace(generation, freshGeneration)}?${queryFor(version)} HTTP/1.1`);
  assert.match(stdout, /^Private staging fixture provider preflight: qualified\./);
  assert.equal(await summary('success'), 'Private staging fixture provider preflight: qualified.\n');
});

test('the hosted entry refuses provider-setting drift and scope mismatches before loading the bundle', () => {
  const scope = JSON.stringify({ bucket, endpoint, region, generation, version, sibling_version: sibling });
  const digest = sha256(scope);
  for (const [input, pin, match = 'true'] of [
    ['', digest], [scope, ''], [scope, 'malformed'], [scope, '0'.repeat(64)],
    [scope + '\n', digest], ['x'.repeat(4097), sha256('x'.repeat(4097))],
    [scope, digest, 'false'], [scope, digest, ''],
  ]) {
    const child = spawnSync(process.execPath, [launcher, '--provider-dispatch', 'no-such-bundle.js'], {
      encoding: 'utf8',
      env: {
        PRIVATE_TRIAL_PREFLIGHT_PROVIDER_MATCH: match,
        PRIVATE_TRIAL_PREFLIGHT_SCOPE: input,
        PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256: pin,
      },
    });
    assert.equal(child.status, 1);
    assert.equal(child.stdout, 'Private staging fixture provider preflight: refused.\n');
    assert.equal(child.stderr, '');
  }
});

test('a hosted retry refuses the earlier cleanup scope before loading the probe', (t) => {
  const earlier = { bucket, endpoint, region, generation, version, sibling_version: sibling };
  const current = {
    ...earlier,
    generation: 'edededededed40ed80ededededededed',
    version: 'retry-selected',
    sibling_version: 'retry-sibling',
  };
  const currentScope = JSON.stringify(current);
  const scratch = mkdtempSync(join(tmpdir(), 'fixture-scope-retry-'));
  t.after(() => rmSync(scratch, { recursive: true }));
  const bundle = join(scratch, 'probe.cjs');
  // Observe entry into the storage probe without making any provider requests.
  writeFileSync(bundle, `
    console.log('Synthetic probe loaded.');
    exports.qualify_disposable_version = (_endpoint, _region, scope) => {
      console.log(scope);
      return 'qualified';
    };
  `);
  const runRetry = (scope) => spawnSync(process.execPath, [launcher, '--provider-dispatch', bundle], {
    encoding: 'utf8',
    timeout: 5_000,
    env: {
      PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT: endpoint,
      PRIVATE_PLAYBACK_STAGING_S3_REGION: region,
      PRIVATE_TRIAL_PREFLIGHT_PROVIDER_MATCH: 'true',
      PRIVATE_TRIAL_PREFLIGHT_SCOPE: scope,
      PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256: sha256(currentScope),
      PRIVATE_TRIAL_PREFLIGHT_S3: JSON.stringify(credential),
    },
  });

  const stale = runRetry(JSON.stringify(earlier));
  assert.equal(stale.status, 1);
  assert.equal(stale.stderr, '');
  assert.equal(stale.stdout, 'Private staging fixture provider preflight: refused.\n');

  const matching = runRetry(currentScope);
  assert.equal(matching.status, 0);
  assert.equal(matching.stderr, '');
  const [loaded, admittedScope, verdict] = matching.stdout.trimEnd().split('\n');
  assert.equal(loaded, 'Synthetic probe loaded.');
  assert.equal(admittedScope, currentScope);
  assert.match(verdict, /^Private staging fixture provider preflight: qualified\./);
});

test('a protected scope match without a disposable credential fails closed', () => {
  const scope = JSON.stringify({ bucket, endpoint, region, generation, version, sibling_version: sibling });
  const child = spawnSync(process.execPath, [launcher, '--provider-dispatch', process.env.INGEST_WASM_JS], {
    encoding: 'utf8',
    env: {
      PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT: endpoint,
      PRIVATE_PLAYBACK_STAGING_S3_REGION: region,
      PRIVATE_TRIAL_PREFLIGHT_PROVIDER_MATCH: 'true',
      PRIVATE_TRIAL_PREFLIGHT_SCOPE: scope,
      PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256: sha256(scope),
    },
  });
  assert.equal(child.status, 1);
  assert.equal(child.stdout, 'Private staging fixture provider preflight: refused.\n');
  assert.equal(child.stderr, '');
});
