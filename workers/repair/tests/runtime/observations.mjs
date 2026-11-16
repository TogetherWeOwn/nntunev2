// Evaluate observations from a generated-Worker probe; this does not invoke a Worker.
// A successful result establishes only the listed observations, not SDK branch coverage.
export function evaluateObservations({ pins, versions, controls, transition }) {
  const checks = [];
  const check = (name, ok) => checks.push({ name, ok: ok === true });
  for (const name of ["miniflare", "workerd"]) {
    check(`${name} version`, typeof pins?.[name] === "string" && pins[name].length > 0 && versions?.[name] === pins[name]);
  }
  const response = (value, status, code) => {
    if (value?.kind !== "response" || value.status !== status || typeof value.text !== "string") return false;
    try {
      return JSON.parse(value.text)?.error === code;
    } catch {
      return false;
    }
  };
  check("body-error control", response(controls?.bodyError, 400, "invalid_request"));
  check("GET control", response(controls?.get, 404, "not_found"));
  const t = transition;
  check("native tracing available", t?.hasStartActiveSpan === true && t?.hasGetActiveSpan === true);
  const context = (id) => typeof id === "string" && id !== "" && id !== "none" && id !== "lookup-threw";
  check("distinct stable contexts", context(t?.ctxA) && context(t?.ctxB) && t.ctxA === t.ctxA2 && t.ctxA !== t.ctxB);
  check("pending body read", Number.isInteger(t?.pullCount) && t.pullCount > 0 && Number.isInteger(t.readCallsAtBarrier) && t.readCallsAtBarrier > 0 && t.aSettledAtBarrier === false);
  const log = Array.isArray(t?.log) ? t.log : [];
  // Reject malformed, duplicate or unordered sequence numbers rather than sorting them.
  check("ordered event log", log.length > 0 && log.every((e, i) => Number.isSafeInteger(e?.seq) && e.seq > (i ? log[i - 1]?.seq : 0)));
  const event = (name) => log.find((e) => e?.event === name);
  const error = event("A.body-errored");
  const invoke = event("B.invoke");
  const settled = event("A.settled");
  check("body error before B invocation", !!error && !!invoke && error.seq < invoke.seq);
  // The producer must identify its handler-origin tick; the log alone does not prove
  // that a particular SDK branch ran or that its continuation used a saved span.
  const queued = log.filter((e) => e?.event === "microtask.queued" && e.origin === "handler-b" && e.ctx === t?.ctxB && invoke && invoke.seq < e.seq);
  const ran = log.find((e) => e?.event === "microtask.run" && e.ranCtx === t?.ctxB && queued.some((q) => q.seq === e.queuedSeq && q.seq < e.seq));
  check("B handler tick before A settlement", !!invoke && !!ran && !!settled && invoke.seq < ran.seq && ran.seq < settled.seq);
  check("transition outcomes", response(t?.a, 400, "invalid_request") && response(t?.b, 404, "not_found"));
  check("no harness error", t?.harnessError == null);
  check("no unhandled rejections", Array.isArray(t?.unhandled) && t.unhandled.length === 0);
  return { ok: checks.every((item) => item.ok), checks };
}
