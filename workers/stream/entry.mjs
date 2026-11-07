// Stable deployment entry. Account runtime and the ingress decorator remain
// uninstalled source until actual Worker qualification permits their wiring.
export * from "./build/worker/shim.mjs";
export { default } from "./build/worker/shim.mjs";
