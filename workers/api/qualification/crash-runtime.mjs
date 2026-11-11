// Actions-only subprocess. The parent owns SIGKILL and process-group cleanup;
// deliberately never dispose Miniflare or acknowledge the held producer send.
import assert from 'node:assert/strict';
import { setTimeout } from 'node:timers/promises';
import { Miniflare, convertV4MiniflareOptions } from 'miniflare';
import { flacFixture } from './flac-fixture.mjs';
import { flacWorkers, root } from './runtime-fixture.mjs';

assert.equal(process.env.GITHUB_ACTIONS, 'true');
assert.equal(typeof process.send, 'function', 'only the parent harness may start the crash runtime');
process.once('message', async ({ persistence, configured }) => {
  try {
    const options = convertV4MiniflareOptions({
      rootPath: root, resourcePersistencePath: persistence, host: '127.0.0.1', port: 0,
      workers: await flacWorkers(configured),
    });
    assert.equal(options.resourcePersistencePath, persistence);
    const mf = new Miniflare(options);
    const composite = await mf.getWorker('composite-flac');
    const bucket = await mf.getR2Bucket('INGEST_SCRATCH', 'composite-flac');
    assert.equal((await composite.fetch('https://qualification.invalid/healthz')).status, 200);
    assert.equal(await bucket.get('qualification-send-accepted'), null, 'acceptance marker must be fresh');
    let settled = false;
    void composite.fetch('https://qualification.invalid/prepare-flac?queue-fault=crash', {
      method: 'POST', body: flacFixture().input,
    }).then(() => { settled = true; }, () => { settled = true; });
    for (let i = 0; i < 300; i++) {
      assert.equal(settled, false, 'producer response must remain pending until SIGKILL');
      const marker = await bucket.get('qualification-send-accepted');
      if (marker) {
        assert.deepEqual(await marker.json(), { attempts: 1, accepted: 1 });
        assert.equal(settled, false);
        process.send({ accepted: true });
        return;
      }
      await setTimeout(50);
    }
    throw new Error('qualification native send did not reach crash boundary');
  } catch {
    // No DSN, keyring, object payload or underlying exception crosses IPC/logs.
    process.send({ accepted: false });
  }
});
