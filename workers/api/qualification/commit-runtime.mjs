import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { setTimeout } from 'node:timers/promises';
import { Miniflare, convertV4MiniflareOptions } from 'miniflare';
import { commitProxy } from './commit-proxy.mjs';
import { seed, sql, state } from './database.mjs';
import { flacWorkers, root } from './runtime-fixture.mjs';

export async function committedAcknowledgements(t, configured) {
  const proxy = await commitProxy();
  let persistence;
  let mf;
  let composite;
  let bucket;
  t.after(async () => {
    try { if (mf) await mf.dispose(); }
    finally {
      await proxy.close();
      if (persistence) await rm(persistence, { recursive: true });
    }
  });
  persistence = await mkdtemp(path.join(tmpdir(), 'preparation-commit-'));
  const options = convertV4MiniflareOptions({
    rootPath: root, resourcePersistencePath: persistence, host: '127.0.0.1', port: 0,
    workers: await flacWorkers(configured, proxy.database),
  });
  assert.equal(options.resourcePersistencePath, persistence);
  async function start() {
    mf = new Miniflare(options);
    composite = await mf.getWorker('composite-flac');
    bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite-flac');
    assert.equal((await composite.fetch('https://qualification.invalid/healthz')).status, 200);
  }
  const prepare = () => composite.fetch('https://qualification.invalid/prepare-commit', { method: 'POST' });
  const recover = id => composite.fetch('https://qualification.invalid/recover', {
    method: 'POST', body: JSON.stringify({ version: 1, id }),
  });
  const count = table => sql(`SELECT count(*) FROM ${table} WHERE job_id = 'qualification-commit-job';`);
  const planState = () => sql(`SELECT md5(jsonb_agg(to_jsonb(p) ORDER BY id)::text)
    FROM ingest_preparation_plans p WHERE job_id = 'qualification-commit-job';`);
  const dispatch = () => JSON.parse(sql(`SELECT row_to_json(q) FROM (
    SELECT id,object_id,object_version,plaintext_sha256,plaintext_bytes,send_acknowledged
    FROM ingest_file_dispatches WHERE job_id = 'qualification-commit-job') q;`));
  seed('commit');
  await start();
  let frozenPlan;
  let frozenObject;
  let startedCustody;
  let pointer;
  for (const [index, fault] of ['plan', 'dispatch', 'startup', 'acknowledgement'].entries()) {
    await t.test(`lost PostgreSQL COMMIT acknowledgement after ${fault}`, async () => {
      assert.equal(await bucket.get('qualification-composite-delivered'), null,
        'no earlier callback may supply the selected commit phase marker');
      const before = proxy.arm(fault);
      const response = await prepare();
      assert.equal(response.headers.get('x-qualification-send-attempts'), index === 3 ? '1' : '0',
        'instrumented native send must not precede an observed startup commit');
      assert.equal(response.status, 503, 'committed-but-unobserved success must remain a redacted refusal');
      assert.equal(await response.text(), 'qualification unavailable');
      proxy.assertDropped(before);
      // Independent direct DB reads prove the selected transaction committed.
      // Wire injection waits for COMMIT completion but never forwards it to Rust.
      assert.equal(count('ingest_preparation_plans'), '1');
      if (index === 0) frozenPlan = planState();
      assert.equal(planState(), frozenPlan, 'later retries must reuse the exact stored envelope and version');
      assert.equal(count('ingest_file_dispatches'), index === 0 ? '0' : '1');
      assert.equal(count('posted_file_job_attempts'), index < 2 ? '0' : '1');
      assert.equal(count('posted_file_attempt_receipts'), index < 2 ? '0' : '1');
      assert.equal(sql(`SELECT status FROM ingest_jobs WHERE id = 'qualification-commit-job';`),
        index < 2 ? 'queued' : 'running');
      if (index > 0) {
        const row = dispatch();
        if (index === 1) pointer = row.id;
        assert.equal(row.id, pointer, 'unknown commits must not allocate another outbox identity');
        assert.equal(row.send_acknowledged, index === 3);
      }
      if (index === 2) startedCustody = state(true);
      if (index === 3) assert.equal(state(true), startedCustody, 'ack commit changes only its monotonic bit');
      const keys = (await bucket.list()).objects.filter(o => o.key.startsWith('prepared-tagged/'));
      assert.equal(keys.length, 1, 'every retry keeps the original create-only R2 object');
      const object = await bucket.get(keys[0].key);
      const bytes = Buffer.from(await object.arrayBuffer());
      const fingerprint = {
        key: keys[0].key, version: object.version, size: bytes.length,
        hash: createHash('sha256').update(bytes).digest('hex'),
      };
      if (index === 0) frozenObject = fingerprint;
      assert.deepEqual(fingerprint, frozenObject);
      if (index > 0) {
        const row = dispatch();
        assert.equal(fingerprint.key, `prepared-tagged/${row.object_id}`);
        assert.equal(fingerprint.version, row.object_version);
        assert.equal(fingerprint.size, row.plaintext_bytes);
        assert.equal(fingerprint.hash, row.plaintext_sha256);
      }
      if (index < 3) {
        assert.equal(await bucket.get('qualification-composite-delivered'), null,
          'no send may precede successful startup and its observed commit');
      } else {
        // Complete the accepted callback before replacement; this control does
        // not rely on emulator persistence of an accepted pending Queue message.
        let delivered;
        for (let i = 0; i < 400; i++) {
          assert.equal(await bucket.get('qualification-refused'), null);
          const marker = await bucket.get('qualification-composite-delivered');
          if (marker) { delivered = await marker.json(); break; }
          await setTimeout(50);
        }
        assert.deepEqual(delivered, { count: 1 });
      }
      const persisted = state();
      await mf.dispose();
      mf = undefined;
      await start();
      assert.equal(state(), persisted, 'fresh runtime must not repair or renew authority on startup');
      assert.equal((await recover(pointer ?? 'unknown-commit-pointer')).status, index < 2 ? 503 : 204,
        'Queue-style recovery cannot start an absent/unacquired intent');
      assert.equal((await recover('unknown-commit-pointer')).status, 503);
      assert.equal(state(), persisted, 'direct recovery remains read-only after ambiguous commit');
      const retained = await bucket.get(frozenObject.key);
      assert.equal(retained.version, frozenObject.version);
      assert.deepEqual(Buffer.from(await retained.arrayBuffer()), bytes);
      assert.equal(await bucket.get('qualification-refused'), null);
      if (index < 3) assert.equal(await bucket.get('qualification-composite-delivered'), null,
        'fresh runtime must not replay any premature send');
    });
  }
  const persisted = state();
  await bucket.delete('qualification-composite-delivered');
  assert.equal(await bucket.get('qualification-composite-delivered'), null);
  const replay = await prepare();
  assert.equal(replay.status, 202, 'ordinary exact retry must reconcile all committed outcomes');
  assert.equal(replay.headers.get('x-qualification-send-attempts'), '1');
  let delivered;
  for (let i = 0; i < 400; i++) {
    assert.equal(await bucket.get('qualification-refused'), null);
    const marker = await bucket.get('qualification-composite-delivered');
    if (marker) { delivered = await marker.json(); break; }
    await setTimeout(50);
  }
  assert.deepEqual(delivered, { count: 1 }, 'a fresh native Queue delivery must positively recover');
  assert.equal(state(), persisted, 'final retry cannot remint, rotate tokens or renew leases');
  assert.equal(planState(), frozenPlan);
  assert.equal((await bucket.get(frozenObject.key)).version, frozenObject.version);
  assert.deepEqual((await bucket.list()).objects.map(o => o.key).sort(),
    [frozenObject.key, 'qualification-composite-delivered'].sort(), 'no plaintext plan in R2');
}
