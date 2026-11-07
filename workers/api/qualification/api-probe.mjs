// Harness-only root. Import the real generated class, Wasm and platform, not VM doubles.
import { WorkerEntrypoint } from 'cloudflare:workers';
import * as generated from '../build/worker/shim.mjs';
import * as facade from '../src/runtime.mjs';
export { default, IngestPreparationPlans } from '../src/runtime.mjs';

let bridgeCalls = 0;
export class QualificationProbe extends WorkerEntrypoint {
  async fetch(request) {
    if (new URL(request.url).pathname === '/fault') {
      generated.default.prototype.ingest_preparation_plan = () => {
        bridgeCalls++;
        throw new Error('qualification fault must not escape');
      };
      return new Response(null, { status: 204 });
    }
    const publicApi = new generated.default(this.ctx, this.env);
    return Response.json({
      generated: Object.keys(generated).sort(),
      facade: Object.keys(facade).sort(),
      generatedPrototype: Object.getOwnPropertyNames(generated.default.prototype).sort(),
      publicPrototype: Object.getOwnPropertyNames(facade.default.prototype).sort(),
      namedPrototype: Object.getOwnPropertyNames(facade.IngestPreparationPlans.prototype).sort(),
      bridgeOnInstance: typeof publicApi.ingest_preparation_plan,
      bridgeCalls,
    });
  }
}
