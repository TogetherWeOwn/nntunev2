# Tail observation projection

`project-tail-event.mjs` exports a pure, deterministic `projectTailEvent(event, policy)`
that maps one staging API Tail invocation to at most one privacy-safe observation.
It keeps healthy invocations so a later error-rate reducer has an honest denominator.

This module is not wired or deployed: no Tail consumer, alert rule, dashboard,
webhook delivery, durable aggregation, or live alerting reads from it. It performs
no network, environment, clock, logging, or storage access.

Run the acceptance suite from the repository root with Node 24+:

```sh
node --experimental-vm-modules --test workers/observability/tests/project-tail-event.test.mjs
```

Bounds: at most 64 log entries, 4 messages per entry, and 16384 UTF-8 bytes per
string are inspected before JSON parsing. Oversized, truncated, or malformed log
containers yield unknown identity and grouping while platform outcome, HTTP status,
and exception signals remain usable.

# Error-rate window verdicts

`evaluate-error-window.mjs` exports a pure, deterministic
`evaluateErrorWindow(observations, policy)` that reduces an explicit array of
v1 observations the caller has already windowed (v1 observations carry no
timestamp, so window membership is the caller's job) to one error-rate verdict
for the staging API. Each call evaluates exactly one window; no cross-window
state is kept.

The policy binds the `api`/`staging` labels, a `min_invocations` denominator
floor, an overall `max_error_rate` rational, and up to 64 `per_error_class`
and 64 `per_release` threshold rules. Nonmatching labels are excluded and
counted, malformed entries are skipped and counted, and invalid policy or a
non-array root fails closed to `null` — never a fabricated verdict. Breach is
strictly greater on exact integer rationals, never on the rounded decimal;
below-floor windows report `unknown`/`insufficient_data`. Breakdowns emit the
first 32 keys in code-unit order with a summed overflow bucket, so output stays
bounded for adversarial input.

This module is not wired or deployed: no Tail consumer, scheduled execution,
webhook delivery, dashboard, or live alerting reads from it. It performs no
network, environment, clock, logging, or storage access.

Run the acceptance suite from the repository root with Node 24+:

```sh
node --experimental-vm-modules --test workers/observability/tests/evaluate-error-window.test.mjs
```
