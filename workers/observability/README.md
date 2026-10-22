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
