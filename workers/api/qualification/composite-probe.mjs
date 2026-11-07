// Harness root only. Neither bridge is present in the ordinary ingest build.
import { WorkerEntrypoint } from 'cloudflare:workers';
import * as generated from '../../ingest/build/worker/shim.mjs';

export default class QualificationComposite extends WorkerEntrypoint {
  async fetch(request) {
    const worker = new generated.default(this.ctx, this.env);
    const url = new URL(request.url);
    const path = url.pathname;
    if (path === '/prepare-flac' && url.searchParams.has('queue-fault')) {
      // Keep the actual Rust producer and native Queue. Fail only at the send
      // boundary: before acceptance, or after acceptance with a lost reply.
      const fault = url.searchParams.get('queue-fault');
      if (!['before', 'after', 'crash'].includes(fault)) throw new Error('unknown qualification Queue fault');
      const queue = this.env.INGEST_QUEUE;
      const thisBucket = this.env.INGEST_SCRATCH;
      let attempts = 0;
      let accepted = 0;
      const patched = new Proxy(queue, {
        get(target, property) {
          if (property === 'send') return async (...args) => {
            attempts += 1;
            if (fault !== 'before') {
              await target.send(...args);
              accepted += 1;
            }
            if (fault === 'crash') {
              await thisBucket.put('qualification-send-accepted', JSON.stringify({ attempts, accepted }));
              // Keep real platform IO pending until the external parent kills
              // the complete subprocess group; do not return a simulated error.
              await new Promise(resolve => setTimeout(resolve, 60_000));
              throw new Error('qualification crash did not occur');
            }
            throw new Error('qualification Queue reply unavailable');
          };
          const value = Reflect.get(target, property, target);
          return typeof value === 'function' && property !== 'constructor' ? value.bind(target) : value;
        },
      });
      const response = await worker.qualification_prepare(request, { ...this.env, INGEST_QUEUE: patched });
      const headers = new Headers(response.headers);
      headers.set('x-qualification-send-attempts', String(attempts));
      headers.set('x-qualification-send-accepted', String(accepted));
      return new Response(response.body, { status: response.status, headers });
    }
    if (path.startsWith('/prepare-range-fault/') || path.startsWith('/recover-range-fault/')) {
      // Fault only the actual R2 GET response; retain its metadata and custody.
      // Preparation still performs the real create-only PUT/HEAD first.
      const recovery = path.startsWith('/recover-range-fault/');
      const fault = path.slice((recovery ? '/recover-range-fault/' : '/prepare-range-fault/').length);
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
            } else if (fault === 'oversized' || fault === 'stalled' || fault === 'stream-error') {
              result.body = new ReadableStream({ start(controller) {
                controller.enqueue(new Uint8Array(fault === 'oversized' ? object.range.length + 1 : 1));
                if (fault === 'stream-error') controller.error(new Error('qualification R2 stream detail'));
                // Open oversize must hit the byte cap, not EOF or the deadline.
                // A stalled valid prefix instead exercises the existing deadline.
              } });
            } else if (fault === 'chunked-long') {
              result.body = new ReadableStream({ start(controller) {
                controller.enqueue(new Uint8Array(object.range.length));
                controller.enqueue(new Uint8Array(1));
                // Leave open so EOF length validation cannot mask a missing
                // cumulative streaming bound across individually valid chunks.
              } });
            } else throw new Error('unknown qualification fault');
            return result;
          };
          const value = Reflect.get(target, property, target);
          // EnvBinding validates constructor.name; it is not a bucket method.
          return typeof value === 'function' && property !== 'constructor' ? value.bind(target) : value;
        },
      });
      const env = { ...this.env, INGEST_SCRATCH: patched };
      const response = await (recovery
        ? worker.qualification_recover(request, env)
        : worker.qualification_prepare(request, env));
      const headers = new Headers(response.headers);
      headers.set('x-qualification-range-reads', String(reads));
      return new Response(response.body, { status: response.status, headers });
    }
    if (path === '/prepare' || path === '/prepare-flac') return worker.qualification_prepare(request, this.env);
    if (path.startsWith('/recover-plan-fault/')) {
      // Open real stored custody first, then fault only the private response.
      // No fault can write a plan, substitute dispatch authority or reach HTTP.
      const fault = path.slice('/recover-plan-fault/'.length);
      const plans = this.env.API_PREPARATION_PLANS;
      let calls = 0;
      const patched = new Proxy(plans, {
        get(target, property) {
          if (property === 'fetch') return async (...args) => {
            const response = await target.fetch(...args);
            if (response.status !== 200) throw new Error('qualification plan unavailable');
            calls += 1;
            const bytes = new Uint8Array(await response.arrayBuffer());
            const headers = new Headers(response.headers);
            headers.delete('content-length');
            let status = response.status;
            let body = bytes;
            if (fault === 'status') status = 503;
            else if (fault === 'redirect') {
              status = 302;
              headers.set('location', 'https://qualification.invalid/not-a-plan');
            } else if (fault === 'type') headers.set('content-type', 'text/plain');
            else if (fault === 'digest') {
              // Keep canonical encoding/geometry valid: three length-prefixed
              // atoms, a one-byte version and u64 predecessor precede the
              // original digest. That digest has no decoder shape restriction.
              if (new TextDecoder().decode(bytes.slice(0, 8)) !== 'NNPREP02') throw new Error('qualification plan format');
              let offset = 8;
              for (let i = 0; i < 3; i++) {
                const length = bytes[offset];
                if (!length || length > 64) throw new Error('qualification plan atom');
                offset += 1 + length;
              }
              offset += 1 + 8;
              if (offset + 32 > bytes.length) throw new Error('qualification plan bounds');
              body = bytes.slice();
              body[offset] ^= 1;
            } else if (fault === 'short') body = bytes.slice(0, -1);
            else if (fault === 'oversized' || fault === 'stalled' || fault === 'stream-error') {
              body = new ReadableStream({ start(controller) {
                // Leave the oversize stream open: a prompt refusal must come
                // from the 1-MiB cap, not EOF/digest rejection or the deadline.
                controller.enqueue(fault === 'oversized' ? new Uint8Array(1_048_577) : bytes.slice(0, 1));
                if (fault === 'stream-error') controller.error(new Error('qualification stream detail'));
              } });
            } else throw new Error('unknown qualification plan fault');
            return new Response(body, { status, headers });
          };
          const value = Reflect.get(target, property, target);
          return typeof value === 'function' && property !== 'constructor' ? value.bind(target) : value;
        },
      });
      const response = await worker.qualification_recover(request, { ...this.env, API_PREPARATION_PLANS: patched });
      const headers = new Headers(response.headers);
      headers.set('x-qualification-plan-calls', String(calls));
      return new Response(response.body, { status: response.status, headers });
    }
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
