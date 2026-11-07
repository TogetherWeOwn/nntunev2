// Source-only facade candidate. Effective main/binding selection is separately
// gated. Qualify the actual builder's generated ABI before configuring this file.
import { WorkerEntrypoint } from "cloudflare:workers";
import GeneratedApi from "../build/worker/shim.mjs";

function unavailable() {
  return new Response("preparation unavailable", {
    status: 503,
    headers: { "Cache-Control": "no-store" },
  });
}

// Composition deliberately excludes every arbitrary generated RPC method from
// the public/default surface. Never re-export or subclass GeneratedApi.
export default class PublicApi extends WorkerEntrypoint {
  async fetch(request) {
    const api = new GeneratedApi(this.ctx, this.env);
    return api.fetch(request);
  }
}

export class IngestPreparationPlans extends WorkerEntrypoint {
  async fetch(request) {
    if (this.env.GENERATION_PREPARATION_MODE !== "preparation_only") {
      return unavailable();
    }
    try {
      const api = new GeneratedApi(this.ctx, this.env);
      if (typeof api.ingest_preparation_plan !== "function") {
        return unavailable();
      }
      // Non-event wasm exports take explicit arguments, unlike generated fetch.
      return await api.ingest_preparation_plan(request, this.env);
    } catch {
      return unavailable();
    }
  }
}
