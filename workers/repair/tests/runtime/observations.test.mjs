import { strict as assert } from "node:assert";
import { test } from "node:test";
import { evaluateObservations } from "./observations.mjs";

function fixture() {
  const response = (status, error) => ({ kind: "response", status, text: JSON.stringify({ error }) });
  return {
    pins: { miniflare: "5", workerd: "1" },
    versions: { miniflare: "5", workerd: "1" },
    controls: { bodyError: response(400, "invalid_request"), get: response(404, "not_found") },
    transition: {
      hasStartActiveSpan: true, hasGetActiveSpan: true,
      ctxA: "a", ctxA2: "a", ctxB: "b", pullCount: 1, readCallsAtBarrier: 1,
      aSettledAtBarrier: false, unhandled: [],
      a: response(400, "invalid_request"), b: response(404, "not_found"),
      log: [
        { seq: 1, event: "A.body-errored" },
        { seq: 2, event: "B.invoke" },
        { seq: 3, event: "microtask.queued", origin: "handler-b", ctx: "b" },
        { seq: 4, event: "microtask.run", queuedSeq: 3, ranCtx: "b" },
        { seq: 5, event: "A.settled" },
      ],
    },
  };
}

function fails(input, name) {
  const result = evaluateObservations(input);
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((check) => check.name === name)?.ok, false);
}

test("accepts a complete observation without claiming SDK branch coverage", () => {
  assert.equal(evaluateObservations(fixture()).ok, true);
});

test("fails closed on missing or mismatched runtime pins and controls", () => {
  const value = fixture();
  value.versions.workerd = "other";
  fails(value, "workerd version");
  delete value.pins.miniflare;
  fails(value, "miniflare version");
  value.controls.bodyError.text = '{"unrelated":"invalid_request"}';
  fails(value, "body-error control");
  value.controls.get.text = "not JSON";
  fails(value, "GET control");
});

test("requires stable distinct native identities and a pending body", () => {
  const value = fixture();
  value.transition.ctxB = "a";
  fails(value, "distinct stable contexts");
  value.transition.ctxB = "lookup-threw";
  fails(value, "distinct stable contexts");
  value.transition.aSettledAtBarrier = true;
  fails(value, "pending body read");
});

test("rejects fabricated ordering, wrong context, and unlinked ticks", () => {
  const value = fixture();
  value.transition.log[2].origin = "other";
  fails(value, "B handler tick before A settlement");
  value.transition.log[2].origin = "handler-b";
  value.transition.log[3].queuedSeq = 99;
  fails(value, "B handler tick before A settlement");
  value.transition.log[3].queuedSeq = 3;
  value.transition.log[3].ranCtx = "a";
  fails(value, "B handler tick before A settlement");
  value.transition.log[3].seq = 2;
  fails(value, "ordered event log");
  const sparse = fixture();
  sparse.transition.log.length++;
  fails(sparse, "ordered event log");
});

test("missing transition, rejected handlers and unhandled errors never pass", () => {
  const value = fixture();
  value.transition.a = { kind: "rejection", message: "withActiveSpan missing" };
  fails(value, "transition outcomes");
  value.transition.unhandled.push("failure");
  fails(value, "no unhandled rejections");
  delete value.transition;
  fails(value, "native tracing available");
});
