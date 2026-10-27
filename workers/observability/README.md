# Integrated staging Tail alert runtime (source only)

`runtime.mjs` connects the real projector, error-window reducer and verdict
renderer through a Tail handler and the `ErrorRateAlerts` Durable Object.
**Integrated source is not configured or live monitoring.** The only consumer
configuration is the inactive [`wrangler.toml.example`](wrangler.toml.example):
no account, route, cron, Tail attachment, deployment environment or secret value.
Existing producer deploy settings are unchanged. Health-red helpers below remain
unconnected; this runtime is not a health probe, synthetic or auto-rollback.

## Runtime contract

- Tail requires `PRODUCER_SCRIPT_NAME` to equal `nntune-api-staging` exactly.
  Missing/invalid configuration makes no binding/network call. Labels are fixed
  `api` / `staging`; other scripts are excluded. Only the first 256 array entries
  are inspected and at most one RPC targets the fixed `api:staging` object.
  Oversized input or rejected configured-producer projections marks coverage
  incomplete, even when no observation survives. Healthy invocations are kept.
- The projector's source-owned grouping allowlists are deliberately narrow:
  `GET /health` and `upstream`; other grouping values become `unknown`, never
  raw URL/message fields. Release identity is the producer's projected SHA or
  honest `unknown`, not the alert consumer's build. RPC revalidates the bounded
  v1 wire schema and rebuilds its whitelist, discarding unexpected properties.
- **DO receipt time**, not Tail event time, defines each half-open
  `[first receipt, first receipt + 60000ms)` window. A coverage-loss-only receipt
  can start a window. Ingress after a deadline never joins that closed window,
  even when its alarm is late. Atomic storage transactions serialize ingress,
  persist observations and the earliest alarm before acknowledgment, and close
  each elapsed window at most once across reconstruction/duplicate alarms.
- Retention is at most two pending windows and 2048 observations total, in
  64-record shards checked against a 65536-byte serialized UTF-8 value limit.
  If a record, batch or capacity limit loses denominator coverage, the affected
  window discards its observations and stays incomplete: **no sampled rate,
  breach alert or healthy/pass verdict**. A third bucket refused by capacity
  leaves a bounded durable lost-coverage interval (timestamps only); receipts
  within it remain suppressed even after space frees. Continued saturation
  coalesces coverage loss conservatively. Later clean windows can recover.
- Closed complete windows use the unchanged reducer policy: `min_invocations: 20`,
  `max_error_rate: { failed: 1, total: 20 }`, empty `per_error_class` /
  `per_release` maps. Breach is strictly greater than 1/20, not greater-or-equal.
  Only an eligible breach rendered by the existing renderer may attempt delivery.
  Closed shards/tombstones are cleaned and no alarm is rearmed when idle.
- The alarm transaction removes closed windows and reserves a **global 300000ms
  cooldown before external I/O**. Network I/O holds no transaction, and its
  completion writes no stale state over concurrent ingress. Delivery is
  **at-most-once attempt, not exactly-once delivery**: a crash after reservation
  but before POST loses that alert. Failure consumes the same cooldown; no
  429/5xx, redirect, timeout or network retry is scheduled. A later independent
  breach after expiry can attempt. A durable outbox/acknowledgment/retry policy
  is future work requiring a separately reviewed delivery contract.
- `OPS_ALERT_WEBHOOK_URL` is the secret **binding name only**. Delivery accepts
  only HTTPS `discord.com`, numeric webhook ID and one URL-safe token path
  segment; credentials, ports (including explicitly written default ports),
  queries, fragments, other hosts and extra segments are rejected. It uses
  JSON POST, `redirect: 'manual'` (the Workers-supported no-follow mode: a 3xx
  is returned unfollowed and treated as a failed attempt, never a retry), a
  5000ms abort/race deadline, and immediate response-body cancellation (zero body bytes retained). Errors/responses,
  webhook URL and token are never logged, stored, rendered or returned.
- `LOGS_URL` must be an absolute HTTP(S) log-view link without credentials or
  whitespace, at most 1024 characters; delivery endpoints are not log links.
  Missing/invalid link or webhook disables delivery, **not storage cleanup**.
  The secret endpoint never reaches the renderer. Consumer observability,
  invocation logs and traces stay disabled so automatic subrequest spans cannot
  capture the secret URL. Public HTTP always returns empty 404, without binding,
  storage or network access. There is no public HTTP ingestion endpoint.

Transport/binding failure is lost coverage, not successful monitoring; this slice
has no reliable ingress replay, overload dashboard or alert-delivery receipt.
Bounded state and local tests do not establish platform quotas, runtime throughput,
Cloudflare alarm timing, Tail completeness or successful real Discord delivery.

## Local verification and later activation

Run all six suites with Node 24 (all transport/storage/clock effects are local
VM doubles; real merged helpers execute, no endpoint access):

```sh
node --check workers/observability/runtime.mjs
node --experimental-vm-modules --test workers/observability/tests/*.test.mjs
```

The two runtime acceptance files are the independent frozen handoff; do not alter
them to make implementation pass. CI's existing Node-24 `tail-projection` gate
runs runtime syntax/acceptance in addition to all pre-existing helper suites.
Wrangler 4.150.0 (the repository pin) does not recognize the `.toml.example`
extension as a config: passing it directly reports a missing entry point. For a
local bundle, copy the example to a run-owned scratch file named `wrangler.toml`,
change only `main` to the absolute path of `workers/observability/runtime.mjs`,
then run the pinned Wrangler with `deploy --dry-run --config` pointing to that
scratch TOML and `--outdir` pointing to a scratch output directory. The local
validation produced 42.29KiB / gzip 9.68KiB and recognized the DO and two public
vars, without a secret or remote binding. This is not permission to deploy.

A separate **staging-first activation** needs exact-head CI/review gates, an
existing-plan/cost check, secret-bound machine contract, approved Tail attachment,
health/controlled staging delivery evidence and a recorded/tested rollback.
Production stays passive; no production mutations or incident injection.
Source-only undo is a normal reviewed PR revert. No live rollback has been tested
by this slice. See the [observability runbook](../../docs/runbook-observability.md).

# Tail observation projection

`project-tail-event.mjs` exports a pure, deterministic `projectTailEvent(event, policy)`
that maps one staging API Tail invocation to at most one privacy-safe observation.
It keeps healthy invocations so a later error-rate reducer has an honest denominator.

The source-only runtime above imports this helper; nothing in this slice is
configured or deployed for live alerting. The helper itself performs no network,
environment, clock, logging, or storage access.

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

The source-only runtime above imports this helper, but no live alerting is
configured or deployed. The helper itself performs no network, environment,
clock, logging, or storage access.

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

The source-only runtime above imports this helper, but no live alerting is
configured or deployed. The helper itself performs no network, environment,
clock, logging, or storage access and carries no webhook URL or secret.

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

# Health-red verdict-to-alert payloads

`render-health-alert.mjs` exports a pure, deterministic
`renderHealthAlert(verdict, context)` that renders one `breach`/`breach`
sustained health-red verdict plus caller context (`logsUrl`, `windowLabel`)
into an alert payload object (`{content, embeds}`) shaped for a Discord
webhook body. Any other verdict status, malformed verdict or context, or
invalid input fails closed to `null` — never a fabricated alert, never a
throw.

Breach payloads carry the verdict status, the exact `red/valid` rational,
the red/valid counts, the longest red span versus the sustained-red
threshold in ms, the service/environment labels, the logs URL, and the
window label. Build-SHA correlation flows through the logs URL only;
health samples carry no release identity, so the renderer invents none.
Output stays within alert bounds (content <= 2000 chars, one embed,
6000-char total, <= 25 fields): oversized strings truncate to a prefix
plus "…", so identical input always yields byte-identical output.

This module is not wired or deployed: no consumer, schedule, delivery,
dashboard, or live alerting reads from it. It performs no network,
environment, clock, logging, or storage access and carries no webhook URL
or secret.

Run the acceptance suite from the repository root with Node 24+:

```sh
node --experimental-vm-modules --test workers/observability/tests/render-health-alert.test.mjs
```
