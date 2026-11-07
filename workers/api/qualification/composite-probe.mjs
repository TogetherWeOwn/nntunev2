// Harness root only. Neither bridge is present in the ordinary ingest build.
import { WorkerEntrypoint } from 'cloudflare:workers';
import * as generated from '../../ingest/build/worker/shim.mjs';

export default class QualificationComposite extends WorkerEntrypoint {
  async fetch(request) {
    const worker = new generated.default(this.ctx, this.env);
    const path = new URL(request.url).pathname;
    if (path.startsWith('/prepare-range-fault/')) {
      // Fault only the GET response after the actual create-only R2 PUT/HEAD.
      // Preserve the real body except in the explicit stream-length controls.
      const fault = path.slice('/prepare-range-fault/'.length);
      const bucket = this.env.INGEST_SCRATCH;
      let reads = 0;
      const patched = new Proxy(bucket, {
        get(target, property) {
          if (property === 'get') return async (...args) => {
            const object = await target.get(...args);
            reads += 1;
            if (!object) return object;
            const result = { key: object.key, version: object.version, size: object.size,
              range: { ...object.range }, body: object.body };
            if (fault === 'offset') result.range.offset += 1;
            else if (fault === 'length') result.range.length -= 1;
            else if (fault === 'string') result.range.offset = String(result.range.offset);
            else if (fault === 'suffix') result.range.suffix = 1;
            else if (fault === 'null') result.range = null;
            else if (fault === 'version') result.version = 'wrong-version';
            else if (fault === 'size') result.size = NaN;
            else if (fault === 'short' || fault === 'long') {
              result.body = new ReadableStream({ start(controller) {
                controller.enqueue(new Uint8Array(object.range.length + (fault === 'short' ? -1 : 1)));
                controller.close();
              } });
            } else throw new Error('unknown qualification fault');
            return result;
          };
          const value = Reflect.get(target, property, target);
          // EnvBinding validates constructor.name; it is not a bucket method.
          return typeof value === 'function' && property !== 'constructor' ? value.bind(target) : value;
        },
      });
      const response = await worker.qualification_prepare(request, { ...this.env, INGEST_SCRATCH: patched });
      const headers = new Headers(response.headers);
      headers.set('x-qualification-range-reads', String(reads));
      return new Response(response.body, { status: response.status, headers });
    }
    if (path === '/prepare' || path === '/prepare-flac') return worker.qualification_prepare(request, this.env);
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
