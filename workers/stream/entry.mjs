import * as sdk from "./build/worker/shim.mjs";
import { guardDurableObjects } from "./ingress.mjs";

// Keep the SDK constructor proxies and every non-fetch dispatch unchanged.
guardDurableObjects(sdk);
export * from "./build/worker/shim.mjs";
export { default } from "./build/worker/shim.mjs";
