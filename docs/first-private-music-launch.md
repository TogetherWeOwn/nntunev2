# First private music launch

This procedure is for the first invite-only, music-only release to the owner
and explicitly invited friends and family. Keep public signup, the music-video
interface and live billing outside this release. Use the existing
[private listening controls](ops.md#private-listening-qualification).

**Current dispatch status: blocked.** The private staging job runs Playwright
but still needs protected wiring for the health proof, the three deployment
pins, and the record and upload steps for the attempt-bound receipt. A reviewed
release path must also resolve when staging can safely disable the trial and
open the protected app lane, which currently refuses promotion in preflight.
Enroll real Product and QA approval through the reviewed staging policy.
Production API/data-plane and Access cutover need protected lanes, evidence and
fallback. A synthetic receipt is for rehearsal only.
[Staging deploy](../.github/workflows/deploy.yml) ·
[Production preflight](../.github/workflows/deploy-production.yml) ·
[Staging admission policy](../ci/staging-admission-policy.json)

## Qualify the candidate

1. Select a full `main` commit SHA and pin its build artifacts. Complete the
   [staging promotion checklist](ops.md#staging-promotion-checklist) using its
   private music E2E box and the shared checks, including the last-good staging
   restore evidence. Record required CI check results and each included PR's
   review clearance on the relevant SHA. Main can keep moving.
2. Pin the successful main-push `deploy.yml` run **and attempt** for that SHA.
   Match successful health proof, actual Web/API/Stream deploy steps and the
   `post-deploy private music journey` to it. The ordinary post-deploy smoke
   is skipped while the private arm is set. Require matching Web, API and
   Stream Worker version and deployment IDs at both ends of the journey. The
   [private journey](../e2e/README.md#private-music-journey-preparation) must
   show upload through Library arrival, play, seek, queue, anonymous and unlisted
   refusal, revoked or expired refusal, and video off. Record Product approval
   of applicability and separate QA approval of the executable mapping.
   Run again from the reviewed producer and verifier source if that source or
   the enrolled contract changes. [Admission verifier](../ci/staging-admission.md)
3. Before release review, collect the staging pins from the protected Actions
   run, jobs and attempt-bound artifact. In the supported promotion lane,
   cross-check them against the `verified_private_music_staging` line in
   **Verify candidate deployment and accepted journey receipt**. Stop if the
   verifier refuses, the line is absent, or the SHA, attempt, digests or
   deployment pins differ. Keep the release on the reviewed evidence. If a
   rerun or drift invalidates it, obtain fresh staging evidence and review the
   updated candidate. The trial cleanup's disable deploy changes the serving
   staging tuple and makes the earlier receipt unusable. Do not postpone
   required cleanup or hold `main` to preserve a receipt. Proceed toward
   release only with a reviewed staging window that supports admission and
   timely cleanup. Otherwise perform cleanup and leave promotion blocked.
   [Trial cleanup](../e2e/README.md#private-music-journey-preparation)
4. Quiesce launch writes and schema/grant changes. Use the
   [protected read-only database baseline](production-database-bootstrap.md#protected-baseline)
   on the pinned production branch and logical database. Require `empty` and
   confirm there are no live users or member data in the launch scope. Ledger
   and reference rows count as data. An unknown, incomplete or refused result
   stops the release. With `rows-present`, use the normal fresh backup, isolated
   restore and exact undo rehearsal for a schema or data change.
5. For a verified empty first release, take or qualify a fresh provider snapshot
   of the exact branch and scope before the first schema change, under the
   same write fence. Read back completion, capture time, expiry and coverage
   of the logical database and grants the change relies on. Keep pre-bootstrap
   and pre-schema snapshots distinct when their recovery targets differ. If
   writes could have resumed after the baseline, repeat it and re-qualify the
   snapshot. The empty-first-release path requires no restore rehearsal.
   Reconcile the current migration manifest and ledger before app cutover.
   [Snapshot and metering](production-database-bootstrap.md#snapshot-and-metering)
6. Verify the binding-free maintenance fallback, private Access route and
   backend/media switch-off. Commission an independent cross-family production
   release reviewer to review the exact SHA, staging attempt, database evidence,
   switch-off and watch plan. Require explicit `APPROVE` on those pins and record
   the decision. This is separate from the PR reviewers' source approval.
   Dispatch only through the supported protected workflow with the existing
   environment and credential custodians. [Production release gates](../CONTRIBUTING.md#production-releases)

## Switch off and verify

Before promotion, name the executor and backup and pin the protected control
for each surface. Verify the maintenance seed's Worker, deployment, version,
seed SHA and module digest, including `503`, `Cache-Control: no-store` and
`X-Robots-Tag: noindex` on its isolated path. Freeze Worker, route, DNS and
Access writers as required by [protected recovery](ops.md#protected-production-worker-recovery).

1. Close new admission at the approved Access and invite controls and disable
   new deployments. Keep the private restriction in force. Unsetting
   `PRIVATE_LISTENING_COHORT` by itself returns playback to the paid gate.
2. A failed final smoke uses the same-job restore when the allocation is pinned.
   For a regression during the watch or an interruption at 100%, use a fresh
   `deploy-production.yml` dispatch on `main` with `mode=recover-worker`, the
   promoted `sha` and the original `recovery_run_id` (attempt 1). Retain the
   accepted SHA, original hostname, rollback custody, sealed intent, key and
   writer freeze through readback.
   On an ambiguous result reconcile allocation, reservation and audit before
   any retry. [Protected recovery](https://github.com/TogetherWeOwn/nntunev2/pull/1783)
3. Read back the pinned maintenance version at 100%, private Access denials
   and the maintenance response through an authorized probe. Use the separately
   approved backend controls to stop new grants and verify direct API and media
   denial, including across existing sessions and a previously issued media
   grant. A signed grant can survive cohort revocation until expiry. Keep the
   service fenced and verify denial before declaring switch-off complete.
   Preserve the database and snapshot. Record the job, allocation and denial
   readbacks and then retire the temporary release-window controls.

## Watch the first 30 minutes

Start at successful final smoke on the pinned 100% allocation. Assign a named
watcher and a backup with recovery access through at least 30 minutes. At the
start, every five minutes and at close, record UTC and confirm the serving Web
and API identities, health/readiness, authorized private listening, expected
denials, and Web/API/Stream errors against the pre-recorded baseline and error
threshold. Keep media probes to the approved synthetic fixture. Record only
outcomes and safe evidence links. The watcher needs usable error and health
readbacks even if automatic alerts are unavailable.

Switch off immediately on build drift, failed health, loss of expected access
control, broken authorized playback or a breached error threshold. Treat lost
coverage as a failed watch and invoke the switch-off plan. Record the trigger,
time, recovery outcome and final allocation. A successful watch ends with all
readbacks healthy after at least 30 minutes. Record the outcome in the release
thread and product log. [Observability runbook](runbook-observability.md)

### Check the observation record offline

Run `python3 ci/production_observation.py pins.json watch.json` on two private
outcome records. Collect the pins separately from the watch, using the accepted
release record and the protected rollout evidence. The verifier reads local
files and returns success only for a complete, healthy watch. The operator
still confirms that each evidence link resolves and supports the recorded outcome.

Both JSON objects use `schema: 1`. Use UTC timestamps with seconds and a `Z` or
`+00:00` suffix. Private evidence links are same-record anchors of the form
`#evidence-<lowercase-slug>`, pointing to sections of the private release record
with the backing evidence. Keep raw logs, credentials, member identifiers and
provider connection details out of these inputs.

| `pins.json` field | Contents |
| --- | --- |
| `candidate` | Full `sha` and expected `api_sha`, decimal strings `run_id` and `run_attempt`, and UUIDs `promoted_version_id` and `deployment_id` |
| `final_smoke` | Candidate fields except `api_sha`, `allocation_percent` of `100`, `outcome` of `success`, `completed_at`, and `evidence` linking to the same run's Actions job |
| `error_policy` | `recorded_at` before smoke completion and `evidence` for the pre-recorded Web/API/Stream baseline and thresholds |

| `watch.json` field | Contents |
| --- | --- |
| `run_id`, `run_attempt`, `error_policy_evidence` | The pinned run and attempt strings and the pinned error policy link |
| `watcher`, `backup` | Each has a distinct operator `name` (use the canonical handle), `from`, `through`, `recovery_access` of `confirmed` and an `evidence` link for the assignment and access covering the whole watch |
| `closed_at`, `result` | Time of the closing readback and the recorded `satisfied` disposition |
| `samples` | Ordered with the first `observed_at` at successful smoke, at most five minutes between consecutive readbacks, and the last exactly at `closed_at` after at least 30 minutes |

Each sample has `observed_at`, `web_sha`, `api_sha`, `promoted_version_id`,
`deployment_id`, `allocation_percent`, `evidence` and a `readbacks` object.
Record `health`, `readiness`, `authorized_playback` and `expected_denials` as
`passed` only when confirmed. Record `web_errors`, `api_errors` and
`stream_errors` as `within-threshold` only when usable readbacks confirm it
against the pinned baseline and thresholds. A missing or unsuccessful check
refuses the record. The [synthetic fixtures](../ci/test_production_observation.py)
show the field values and successful and refused records.

## Pinned candidate template

Copy this into the private release record. Fill the evidence before dispatch
and append the rollout and watch results as they happen. Use private references
for provider targets, custody and baselines. Keep secrets, member identifiers,
raw diagnostics and connection details out of the record. The
[offline verifier fixtures](../ci/test_staging_admission.py) contain synthetic
receipts for rehearsing the staging section only.

| Item | Pin or evidence to record |
| --- | --- |
| Candidate | Full main SHA, artifact digest(s), exact-head required checks and per-PR review references |
| Staging attempt | `deploy.yml` run URL, run ID, attempt, health and private journey job URLs and conclusions, staging window and disable-deploy timing |
| Staging admission | Artifact name and Actions digest, opening and closing UTC, Web/API/Stream Worker, build SHA, version and deployment IDs, contract/policy/manifest digests, Product and QA references, final verifier job URL and pin match |
| Staging rollback | Last-good SHA, run/attempt, restore and identity readback |
| Production baseline | Protected source SHA, run/attempt and job URL, private provider branch and logical DB/OID reference, manifest digest, read time and `empty` verdict, separate no-live-user/data confirmation, writer fence and custodian |
| Pre-schema snapshot | Private snapshot reference, capture and completion UTC, branch and restore scope, expiry, verification reference and first schema apply boundary |
| Protected switch-off | Executors and protected controls for Access/invites and API/media, verified maintenance Worker/deployment/version/seed SHA/module digest, sealed intent custody reference, expected denials and grant expiry boundary |
| Release review and dispatch | Independent reviewer, decision/time/reference, accepted SHA and staging attempt, protected workflow and rollout run/attempt, final deployment/version and smoke result |
| Health watch | Watcher, backup, UTC start/end, health and error baseline/threshold, readback schedule and results, switch-off trigger/recovery attempt if used, final disposition |
