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

# Verdict-to-alert payloads

`render-verdict-alert.mjs` exports a pure, deterministic
`renderVerdictAlert(verdict, context)` that renders one `breach`/`breach`
error-rate verdict plus caller context (`logsUrl`, `windowLabel`) into an
alert payload object (`{content, embeds}`) shaped for a Discord webhook
body. Any other verdict status, malformed verdict or context, or invalid
input fails closed to `null` — never a fabricated alert, never a throw.

Breach payloads carry the verdict status, the exact `failed/total`
rational plus 4-decimal rate, the failed/total counts, leading breached
rule names (capped), top breakdown entry keys per dimension ranked by
failures, release SHAs, the logs URL, and the window label. Output stays
within alert bounds (content <= 2000 chars, one embed, 6000-char total,
<= 25 fields): oversized strings truncate to a prefix plus "…" and
oversized lists cap to a leading prefix with a "+N more" marker, so
identical input always yields byte-identical output.

This module is not wired or deployed: no consumer, schedule, delivery,
dashboard, or live alerting reads from it. It performs no network,
environment, clock, logging, or storage access and carries no webhook URL
or secret.

Run the acceptance suite from the repository root with Node 24+:

```sh
node --experimental-vm-modules --test workers/observability/tests/render-verdict-alert.test.mjs
```

# Sustained health-red window verdicts

`evaluate-health-window.mjs` exports a pure, deterministic
`evaluateHealthWindow(samples, policy)` that reduces an explicit array of
`{t, ok}` health samples the caller has already windowed (each sample
carries its own caller-supplied ms clock, so window membership is the
caller's job) to one sustained-red verdict for the staging API. Each call
evaluates exactly one window; no cross-window state is kept.

The policy binds the `api`/`staging` labels, a `red_for_ms` sustained-red
threshold (1..3600000), and a `min_samples` denominator floor (1..100000).
Valid samples are ordered by `t` ascending (stable) within each
malformed-delimited segment; malformed entries are skipped, counted, and
split contiguity, so no breach may span untrusted evidence. A contiguous
not-ok run breaches iff (last_t - first_t) >= red_for_ms; below-floor
windows report `unknown`/`insufficient_data` with the longest red span
still reported. Invalid policy or a non-array root fails closed to `null`
— never a fabricated verdict, never a throw.

This module is not wired or deployed: no consumer, schedule, delivery,
dashboard, or live alerting reads from it. It performs no network,
environment, clock, logging, or storage access.

Run the acceptance suite from the repository root with Node 24+:

```sh
node --experimental-vm-modules --test workers/observability/tests/evaluate-health-window.test.mjs
```
