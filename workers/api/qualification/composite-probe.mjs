// Harness root only. Neither bridge is present in the ordinary ingest build.
import { WorkerEntrypoint } from 'cloudflare:workers';
import * as generated from '../../ingest/build/worker/shim.mjs';

export default class QualificationComposite extends WorkerEntrypoint {
  async fetch(request) {
    const worker = new generated.default(this.ctx, this.env);
    const path = new URL(request.url).pathname;
    if (path === '/prepare') return worker.qualification_prepare(request, this.env);
    if (path === '/recover') return worker.qualification_recover(request, this.env);
    if (path === '/abi') {
      return Response.json(Object.getOwnPropertyNames(generated.default.prototype).sort());
    }
    return worker.fetch(request);
  }

  async queue(batch) {
    const worker = new generated.default(this.ctx, this.env);
    // Positively observe the real Rust pointer recovery, not just callback return.
    // This is an extra read-only invocation; the unmodified generated consumer
    // still receives the actual native batch and owns ack/retry settlement.
    for (const message of batch.messages) {
      const response = await worker.qualification_recover(new Request('https://qualification.invalid/', {
        method: 'POST', body: JSON.stringify(message.body),
      }), this.env);
      if (response.status !== 204) throw new Error('qualification recovery refused');
    }
    await worker.queue(batch);
    await this.env.INGEST_SCRATCH.put('qualification-composite-delivered', JSON.stringify({
      count: batch.messages.length,
    }));
  }
}
