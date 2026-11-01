# NNTune v2 observability runbook

This runbook covers the Cloudflare-hosted NNTune v2 app. It does not cover the legacy service or use an external uptime monitor. **The escape-detection kit is not live yet.** The current repository does not configure Discord alert rules, a two-minute health alert, five-minute user-flow synthetics, production synthetics, release-tagged error tracking, or an error-rate rollback action. Do not treat this runbook or a green CI run as proof of deployed monitoring.

## Integrated error-rate runtime: staging deployment lane, delivery not activated

[`workers/observability/runtime.mjs`](../workers/observability/runtime.mjs)
integrates the staging API Tail projector, durable receive-time error windows,
reducer, renderer and secret-bound Discord delivery. The
[`deploy-alerts-staging` workflow](../.github/workflows/deploy-alerts-staging.yml)
deploys only `nntune-alerts-staging` after a qualifying push to `main`, when the
existing repository deployment token is configured. It derives a temporary
staging config from the [inactive local example](../workers/observability/wrangler.toml.example).
It does not provision a secret or attach the consumer to `nntune-api-staging`.
An already provisioned `OPS_ALERT_WEBHOOK_URL` is allowed only as `secret_text`;
its value is never read by the verifier. **A merged lane is not evidence that the
consumer is deployed, attached or delivering messages.** Check the workflow's
actual deploy and verify results; a credential-gate skip is not a deployment.
Producer deployment settings are unchanged. The [runtime contract and local
verification](../workers/observability/README.md#runtime-contract) describe the
frozen interfaces, retention caps and deterministic local tests. A green local
suite is not Cloudflare/Discord delivery evidence.

Complete 60-second DO receipt-time windows alert only above a 1/20 failure rate
with at least 20 invocations. Incomplete/overloaded windows are **lost coverage**,
not healthy monitoring: they emit neither a sampled-rate alert nor a pass verdict.
The renderer includes the configured logs link, original receive-time boundaries
and producer release identity (or honest `unknown`), never a consumer build SHA.
Only `GET /health` / `upstream` grouping is currently allowlisted; other grouping
values remain unknown. This is not the separate two-minute sustained-health rule.

Delivery reserves one durable 300000ms global cooldown before any POST, including
failed delivery. This is **at-most-once attempt**, not exactly-once delivery: a
crash after reservation can lose the message; 429/5xx, redirect, network and
5000ms timeout failures have no retry. Response bodies are cancelled without
retaining text. `OPS_ALERT_WEBHOOK_URL` is a secret binding name, not a value to
inspect or paste. A missing/invalid webhook or `LOGS_URL` disables delivery while
cleanup continues. Consumer logs/traces must stay disabled to avoid secret URL
capture in automatic subrequest spans. Public HTTP is always empty 404.

The deployment guard freezes four runtime modules to
`818e57680414c4414cc3c96e2f3f9a6ca38d80bf` and rejects config drift before deploying.
The generated config disables consumer observability/logs/invocation logs/traces,
workers.dev and preview URLs; it has no routes and only the `v1` SQLite
`ErrorRateAlerts` migration. Verification lists every namespace page, matches the
binding's namespace ID/class/script, and rejects unexpected bindings or enabled
logging. The required `tail-projection` CI job executes the same freeze guard and
offline rejection fixtures. The verifier prints IDs and binding names, not values.
A verification failure happens **after deployment**: it does not automatically
undo a serving version. Do not rerun a failed deployment as a diagnostic shortcut.

Secret provisioning and Tail attachment remain a separate staging-first machine
contract: exact-head CI and review, existing-plan/cost and secret-version checks,
approved attachment, controlled staging breach plus real message/log-link evidence,
health and a tested rollback. This lane supplies only the consumer deployment;
it is not permission to provision secrets, attach Tail, run live delivery or change
production. Missing/invalid configuration keeps delivery fail-closed.

### Staging consumer rollback

This is a recorded procedure, **not an exercised live rollback receipt**. Before
any activation or later redeploy, the authorized executor records the workflow
run/head, current serving deployment/version (from Cloudflare Deployments, not
just the newest uploaded version), last known-good version, namespace ID and
attachment state in the private deployment record. Never record secret values.

1. Pause further staging consumer deployments through the approved workflow
   control, and retain the failed run's evidence. Producer and production data
   remain untouched. If a separately approved Tail attachment is active, its
   authorized executor restores/removes only that attachment under its own
   recorded rollback contract; this lane does not manage producer settings.
2. For an existing known-good version with the same DO migration lineage, use
   pinned Wrangler 4.153.0:
   `wrangler rollback "$LAST_KNOWN_GOOD_VERSION" --name nntune-alerts-staging`.
   Select the recorded version explicitly; never rely on implicit "previous".
   [Cloudflare rollback limits](https://developers.cloudflare.com/workers/versions-and-deployments/rollbacks/)
   prohibit crossing a DO lifecycle migration. No new migration, Worker deletion,
   namespace deletion or durable-history deletion is an undo step here.
3. On the first deployment there is no pre-creation version to restore. Keep the
   consumer unattached, pause the lane and preserve the Worker/namespace. Fix
   forward through an exact-head reviewed PR; do not invent a rollback ID.
4. Recheck Cloudflare's active deployment against the selected version and run
   `python3 .github/scripts/alerts_staging.py verify` using existing scoped
   credentials. Confirm observability remains off and attachment state matches the
   approved record. Record version/namespace IDs and staging API health; require
   controlled delivery evidence separately before calling monitoring live.

Source undo is a normal gated revert PR. Reverting workflow source does not undo
a Cloudflare deployment. No live deployment, delivery test or rollback drill was
performed while adding this lane. Production stays passive.

## Current baseline and gaps

- **Cloudflare Logs:** use the Cloudflare dashboard under **Workers & Pages → [service] → Observability → Logs** only if Workers Logs are enabled for that deployed service. The `staging-api` lane's generated config sets `[env.staging.observability] enabled = true`; a deploy from before that setting left `nntune-api-staging` with `observability: null`, which persists no records, so confirm the Worker's settings show observability enabled before expecting any record. The logging contract is implemented in code but not yet emitted by any deployed Worker: [`docs/logging-standard.md`](logging-standard.md) specifies the six-key target envelope (`ts`, `level`, `worker`, `trace_id`, `event`, `fields`) with `x-trace-id` propagation, and [`docs/api-errors.md`](api-errors.md) defines the shared `{code, message, retryable}` taxonomy whose `code` is the `error_class` grouping key. `api`/`ingest`/`mb` emit the envelope in code via `crates/nntune-logging`, each record carrying `build_sha` in `fields` (40-hex commit from the public `BUILD_SHA` var, explicit `"unknown"` until a lane wires it; `stream` has only the build-SHA normalizer mirror, no emission yet), but `deploy.yml` carries only the api staging lane (`staging-api`, skipped until the Operator wires staging) so no backend Worker is proven serving; until its first `success` no release/`build_sha` field is deployed, so do not expect `trace_id`/`request_id`, `build_sha`, `release`, `route`/`job`, or `error_class` filters to return results. Cloudflare's native issue grouping is platform-generated; group application errors by the registry `code` in the configured structured-log or tail-consumer/error-tracking view rather than assuming the native grouping key.
- **Cloudflare Issues:** **Workers & Pages → [service] → Observability → Issues** can show grouped Worker failures when enabled. Error-class grouping and release-to-commit linkage are not configured by this repository yet. Use the issue as a starting point, not proof of release attribution.
- **Staging synthetic:** `.github/workflows/e2e-staging.yml` triggers at minute 17 every six hours (`17 */6 * * *`). Each configured scheduled or manually dispatched run executes only the health-only `synthetic` job (monitoring-only); the full staging journey runs solely on the post-deploy call from `deploy.yml` after a successful `staging-web` deploy of the same run. The journey includes signup and other writes and is staging-only. This is not the required five-minute user-flow synthetic. Jobs skip when staging is not configured; check the workflow run and job results before relying on it.
- **Production synthetic:** none is configured. `e2e-staging.yml` exercises the staging URL only; `deploy-production.yml` (web only, disabled unless `PRODUCTION_DEPLOY_ENABLED=true`) runs one post-promotion smoke check, not a scheduled synthetic. Do not attempt production signup, playback setup, or other writes. Any later production synthetic must be explicitly read-only and passive.
- **Alerts:** `deploy-alerts-staging.yml` now defines a staging-only consumer deployment lane with frozen error-rate logic; it neither provisions the Discord destination/secret nor attaches Tail. Check the actual run and environment before calling any monitoring live. No two-minute sustained-red threshold is configured. The expected Discord destination and secret injection still need environment-level verification; do not infer they exist from a secret name mentioned in a task.
- **Rollout/rollback:** `deploy-production.yml` promotes the web Worker through the shared `TogetherWeOwn/.github` gradual-promote action (5% → 50% → 100%, per-stage health gates, automatic rollback); the lane is disabled unless `PRODUCTION_DEPLOY_ENABLED=true`. No error-rate auto-rollback is wired. Of the backend Workers, only the api staging lane deploys one (the alerts consumer has its own lane) — see the future-lanes section of [`docs/ops.md`](ops.md#staging-promotion-checklist). The current staging checklist in [`docs/ops.md`](ops.md#staging-promotion-checklist) documents the web redeploy rollback; it does not cover Worker rollback or prove which SHA is serving.

## Where to look after the kit is enabled

1. Open the Cloudflare dashboard for the account that owns NNTune v2.
2. Go to **Workers & Pages** and select the affected service/environment: `nntune-web-staging` or `nntune-web`; Worker services, when deployed, are named `nntune-api`, `nntune-ingest`, `nntune-stream`, `nntune-mb`, `nntune-repair`, and `nntune-transcode`; staging Workers use the `-staging` suffix (for example `nntune-stream-staging`, `nntune-repair-staging`, `nntune-transcode-staging`). Name sources: `nntune-web[-staging]` is pinned in `apps/web/wrangler.toml`, and `nntune-stream[-staging]` / `nntune-repair[-staging]` / `nntune-transcode[-staging]` in their active `wrangler.toml` files; `nntune-api[-staging]` is pinned in the config `deploy.yml` generates for `staging-api`; `nntune-ingest` and `nntune-mb` come from the `wrangler.toml.example` templates and crate names (no active `wrangler.toml` yet), so their `-staging` names follow the same convention but are not pinned in a deploy config.
3. In **Observability → Logs**, search for a `trace_id` (or legacy `request_id`/job identifier) and inspect `build_sha`, `release`, `route`/`job`, and `error_class` **only after those fields are present in the deployed structured events**. Follow the same trace ID through downstream Worker, Durable Object, and queue/consumer events where propagation has been implemented.
4. In **Observability → Issues**, inspect grouped uncaught Worker failures. Use the linked structured event or configured tail-consumer/error-tracking event to find its release and error class; the native issue view may not carry those fields.
5. In **Notifications**, inspect the configured NNTune alert rule and Discord destination. Test delivery with a controlled staging alert and verify the resulting message in the ops channel. Never copy a webhook value into a ticket, terminal transcript, or repository.

Dashboard labels and service names are navigation hints; exact URLs and availability are account/environment-dependent. If a service, field, alert rule, or destination is absent, record it as missing/unverified rather than assuming it is active.

## From an alert to the responsible change

Once release-tagged alert events are available:

1. Open the alert's log link. Record timestamp, environment, `trace_id` (legacy `stream` events may carry `request_id` instead) or job ID, `error_class`, and full `build_sha`/`release`. Do not copy user data or secret-bearing fields into incident notes.
2. Open the commit for that full SHA in GitHub and inspect its associated pull request. If the alert does not link directly, search the repository's commit/PR history for the exact SHA and compare it with the release reported by the deployed service. Do not infer causality from the newest PR title alone.
3. Follow the `trace_id` across Worker → Durable Object/queue → consumer logs where propagation is implemented. (Legacy `stream` events may still carry `request_id`.) For an uncaught exception, start with the Cloudflare Issue and its stack trace, then confirm the matching release in the structured event or error-tracking view.
4. Record the environment, first/last-seen times, error class, correlation ID, release SHA, related commit/PR URL, impact without PII, and mitigation/rollback decision.

Until the deployed service exposes and verifies its build identity, neither a workflow run nor the public `/api/health` response proves which SHA is serving. See the build-identity caveat in [`docs/ops.md`](ops.md#staging-promotion-checklist).

## Alert response

- **5xx/error-rate spike or red health/readiness:** open the linked logs and establish whether the signal is isolated or sustained. The requested readiness alert threshold is more than two minutes continuously red; until that rule is configured and tested in staging, use this as a response target, not a claim about current paging behavior. Confirm environment and serving SHA before changing anything.
- **Unhandled exception or crash loop:** inspect the Cloudflare Issue and corresponding structured/error-tracking event. Correlate by `error_class` and trace/job ID (legacy `stream` events may carry `request_id`) when those fields are deployed; do not log raw exception messages if they may contain secrets or personal data.
- **Synthetic failure:** inspect the run's environment, flow step, and timestamp. Current scheduled coverage is staging-only: a health smoke every six hours (scheduled/manual runs never run the full journey). The requested five-minute staging flow and a separate passive, read-only production check are not configured; do not claim either is active until each is implemented and verified in its allowed environment.
- **Discord notification missing:** inspect the alert rule and destination configuration after they have been provisioned. Verify secret availability by name only through the authorized configuration path. Never print, fetch, or rotate the webhook value as a diagnostic shortcut.

## Rollback and verification boundaries

Staging is the first verification target. The current web rollback procedure is to rerun all jobs for the last known-good successful staging deployment as documented in [`docs/ops.md`](ops.md#staging-promotion-checklist); confirm `staging-web` succeeds and verify staging health. The rerun also redeploys the api Worker when `staging-api` is configured, but that is not verified api rollback; it does not deploy or roll back the ingest, stream, MB, repair or transcode Workers, and does not prove the serving SHA. Establish and test each Worker rollback separately before relying on it.

Production monitoring is passive. No production deploy, synthetic write, data migration, secret change, or alert-suppression change is authorized by this runbook. Do not manufacture production incidents to test alert delivery.

## Verification checklist for the completed kit

Do not mark an item live without dated evidence for the exact deployed environment and release:

- Workers Logs persist structured JSON events conforming to [`docs/logging-standard.md`](logging-standard.md) (six-key envelope with propagated `trace_id`, namespaced `event`, flat redacted `fields`), carrying `build_sha`/`release` (Git SHA), `route`/`job`, and `error_class` (the [`docs/api-errors.md`](api-errors.md) registry `code`); the dedicated secret/PII redaction test passes.
- A controlled staging error produces a release-linked event and a Discord alert with a useful log link. Confirm the event's SHA points to the responsible commit/PR. Production verification remains passive.
- Staging 5xx/error-rate, uncaught-exception, and health/readiness alerts are exercised; confirm health/readiness fires only after the configured two-minute sustained-red condition.
- The main user-flow synthetic runs every five minutes in staging and alerts on failure. A separately verified production synthetic, if configured, is read-only and passive; none exists in the current repository baseline.
- The shared promotion action's gradual rollout and error-rate rollback are exercised on staging. Record the pre-release and rollback SHAs and verify recovery; do not infer Worker rollback support from the existing web-only procedure.

The current repository baseline does not satisfy this checklist. Use staging evidence for active tests and production configuration/read-only evidence separately.
