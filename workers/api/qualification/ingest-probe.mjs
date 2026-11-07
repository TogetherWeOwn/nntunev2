// Harness-only platform probe; all fetch/queue delegation uses the actual shim.
// The R2 write/send below tests platform composition, NOT the Rust producer.
import { WorkerEntrypoint } from 'cloudflare:workers';
import * as generated from '../../ingest/build/worker/shim.mjs';

export default class QualificationIngest extends WorkerEntrypoint {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/abi') {
      return Response.json({
        generated: Object.keys(generated).sort(),
        prototype: Object.getOwnPropertyNames(generated.default.prototype).sort(),
      });
    }
    if (path === '/private') return this.env.API_PREPARATION_PLANS.fetch(request);
    if (path === '/disabled') return this.env.DISABLED_PLANS.fetch(request);
    if (path === '/probe' || path === '/fault') return this.env.PROBE.fetch(request);
    if (path === '/configured-probe' || path === '/configured-fault') {
      const probe = new Request(new URL(path === '/configured-fault' ? '/fault' : '/probe', request.url));
      return this.env.CONFIGURED_PROBE.fetch(probe);
    }
    if (path === '/configured-private') return this.env.CONFIGURED_PLANS.fetch(request);
    if (path === '/seed') {
      await this.env.INGEST_SCRATCH.put('qualification-tagged', new Uint8Array([1, 2, 3]));
      await this.env.INGEST_QUEUE.send({ version: 1, id: 'qualification-pointer' });
      return new Response(null, { status: 202 });
    }
    if (path === '/retained') {
      const object = await this.env.INGEST_SCRATCH.get('qualification-tagged');
      const delivered = await this.env.INGEST_SCRATCH.get('qualification-delivered');
      const refused = await this.env.INGEST_SCRATCH.get('qualification-refused');
      return Response.json({
        bytes: object ? Array.from(new Uint8Array(await object.arrayBuffer())) : null,
        delivered: delivered ? await delivered.json() : null,
        refused: refused ? await refused.json() : null,
      });
    }
    return new generated.default(this.ctx, this.env).fetch(request);
  }

  async queue(batch) {
    // Persist only the platform settlement, not the message or arbitrary error.
    // Real generated Rust recovery has no DB binding and must retry, not ack.
    await new generated.default(this.ctx, this.env).queue(batch);
    await this.env.INGEST_SCRATCH.put('qualification-delivered', JSON.stringify({
      count: batch.messages.length,
    }));
  }
}
