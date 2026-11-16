# Runtime probe observation checks

`evaluateObservations` in `observations.mjs` checks observations produced by a
separate generated-Worker probe. It does not build or invoke the Worker and its
unit fixtures are synthetic; a passing unit test is **not** runtime compatibility
or SDK branch-coverage evidence.

The caller supplies `{ pins, versions, controls, transition }` and must fail the
probe if the returned `ok` is false. `pins` should come from the `runtime` entry
in `.github/worker-toolchain.json`; the caller must independently resolve and
report the versions actually loaded. The repair build uses that manifest's
`repair-profile`, independently of these runtime version checks.

`controls.bodyError` and `controls.get` are `{ kind, status, text }` results.
The transition object uses the same result shape in `a` and `b`, native context
IDs `ctxA`, `ctxA2`, `ctxB`, `hasStartActiveSpan`, `hasGetActiveSpan`, body-pull
and reader counts, `aSettledAtBarrier`, `unhandled`, and an ordered `log`.
The producer marks a `microtask.queued` event `origin: "handler-b"` only when
it has observed that origin; the corresponding `microtask.run` must link via
`queuedSeq` and record its actual `ranCtx`. Unobserved origins or contexts must
not be invented to pass. Actual invocation, artifact provenance, runtime stdio,
timeouts, branch reachability, and version resolution remain the caller's
responsibility. Hosted Actions provides the isolation boundary.

Run the independent checks with `node --test workers/repair/tests/runtime/observations.test.mjs`.
