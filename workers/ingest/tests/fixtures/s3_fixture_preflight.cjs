'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { once } = require('node:events');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const { createServer } = require('node:tls');
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

test('synthetic TLS origin refuses trailing bytes after an otherwise valid HEAD or DELETE', async (t) => {
  let lines = [];
  let corruptStep = -1;
  const server = createServer({ key: readFileSync(process.env.S3_FIXTURE_KEY),
    cert: readFileSync(process.env.NODE_EXTRA_CA_CERTS) }, (socket) => {
    socket.on('error', () => {});
    let input = '';
    socket.on('data', (chunk) => {
      input += chunk;
      if (!input.endsWith('\r\n\r\n')) return;
      const step = lines.push(input.split('\r\n')[0]) - 1;
      const [, pin] = plan[step];
      const status = ['200 OK', '200 OK', '204 No Content', '404 Not Found',
        '204 No Content', '404 Not Found', '200 OK'][step];
      const length = status.startsWith('204') ? '' : 'Content-Length: 7\r\n';
      socket.write(`HTTP/1.1 ${status}\r\nConnection: close\r\n${length}x-amz-version-id: ${pin}\r\n\r\n`);
      socket.end(step === corruptStep ? 'payload' : undefined);
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const origin = `https://127.0.0.1:${server.address().port}`;
  const scope = { bucket, endpoint: origin, region, generation, version, sibling_version: sibling };
  for (const step of [-1, 2, 5]) {
    lines = [];
    corruptStep = step;
    assert.equal(await qualify(process.env.INGEST_WASM_JS, origin, region,
      JSON.stringify(scope), JSON.stringify(credential)), step < 0 ? 'qualified' : 'unconfirmed');
    assert.deepEqual(lines, plan.slice(0, step < 0 ? plan.length : step + 1)
      .map(([method, pin]) => `${method} ${path}?${queryFor(pin)} HTTP/1.1`));
  }
});

test('the operator entry requires explicit provider invocation and prints a fixed verdict', () => {
  const launcher = require.resolve('../../../../.github/scripts/private_trial_provider_preflight.cjs');
  const child = spawnSync(process.execPath, [launcher, process.env.INGEST_WASM_JS], { encoding: 'utf8', env: {} });
  assert.equal(child.status, 1);
  assert.equal(child.stdout, 'Private staging fixture provider preflight: refused.\n');
  assert.equal(child.stderr, '');
});
