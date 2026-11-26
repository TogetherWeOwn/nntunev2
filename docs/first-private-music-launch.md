# First private music launch

This procedure is for the first invite-only, music-only release to the owner
and explicitly invited friends and family. Keep public signup, the music-video
interface and live billing outside this release. Use the existing
[private listening controls](ops.md#private-listening-qualification).

Record candidate dispatch readiness in the [pinned candidate template](#pinned-candidate-template).
On an armed push to protected `main`, `deploy.yml` binds the Web, API and
Stream deployments to the same attempt and observes the serving deployments
before and after the private Playwright journey. The journey checks the serving
build through `/api/build` and `/healthz` at both ends. The workflow evaluates
the Playwright evidence against the reviewed contract, requiring cases to pass
without retries. It uploads the attempt-bound receipt only when the evaluator
returns `satisfied`.

Scoped staging access and a live qualifying attempt remain pending. Follow the
[protected staging trial runbook](ops/private-staging-trial.md) for setup,
qualification and cleanup. The admission policy has no approvals, so the
protected job cannot upload a satisfied receipt and the verifier refuses
admission. After successful qualification, the Actions summary exposes the
case names and first-attempt outcomes from that exact attempt as
[contract enrollment review evidence](../ci/staging-admission.md#protected-contract-enrollment-review).
The raw report stays on the runner. Keep enrollment pending until Product
approves applicability and QA separately approves the executable mapping
against the protected live evidence. Enrollment requires a reviewed policy
change. Synthetic receipts are for rehearsal only.

Production preflight currently refuses application promotion, even when the
lane is enabled. Maintenance bootstrap requires a passing ordinary full journey
and a matching receipt under the approved ordinary contract. An armed private
run skips the ordinary journey. If a reviewed path opens the app lane, the
staging-evidence verifier requires the approved private journey and the exact
staging deployment evidence. That path must also support timely trial cleanup.
Production API/data-plane and Access cutover still need protected lanes, evidence and
fallback. Keep deployment and release approvals pending in the candidate record
until their evidence is available.
[Staging deploy](../.github/workflows/deploy.yml) ·
[Production preflight](../.github/workflows/deploy-production.yml) ·
[Staging admission policy](../ci/staging-admission-policy.json) ·
[Cancelled production approval rehearsal](ops.md#offline-rehearsal-for-cancelled-production-approval)

## Qualify the candidate

1. Select a full `main` commit SHA and pin its build artifacts. Complete the
   [staging promotion checklist](ops.md#staging-promotion-checklist) using its
   private music E2E box and the shared checks, including the last-good staging
   restore evidence. Record required CI check results and each included PR's
   review clearance on the relevant SHA. Main can keep moving.
2. Pin the successful main-push `deploy.yml` run **and attempt** for that SHA.
   Match successful health proof, actual Web/API/Stream deploy steps, the
   `post-deploy private music journey` and its receipt upload to it. The
   ordinary `post-deploy-e2e` job is skipped while the private arm is set.
   Require matching Web, API and Stream Worker version and deployment IDs at
   both ends of the [private journey](../e2e/README.md#private-music-journey-preparation).
   It must show invite acceptance, upload through Library arrival, play, seek,
   queue, anonymous and unlisted refusal, revoked or expired refusal, and
   video off. Record Product approval of applicability and separate QA approval
   of the executable mapping.
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
   stops the release. [Check the saved manifest digest](#check-the-baseline-manifest-offline)
   before carrying the baseline forward. With `rows-present`, use the normal
   fresh backup, isolated restore and exact undo rehearsal for a schema or data
   change.
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

### Check the baseline manifest offline

Set `candidate_sha` to the full commit under **Candidate** and
`baseline_manifest_sha256` to the saved manifest digest under **Production
baseline** in the private release record. From a local clone containing the
candidate commit and its manifest, compare the exact manifest bytes from that
commit with the saved digest.

```bash
(
  set -o pipefail
  manifest_hash=$(git --no-replace-objects cat-file blob "${candidate_sha:?}:migrations/manifest.toml" | sha256sum) || exit 1
  test "${manifest_hash%% *}" = "${baseline_manifest_sha256:?}"
)
```

For a synthetic rehearsal, keep the candidate, protected run and attempt,
branch, database OID, reader, required catalog objects and successful `empty`
verdict fixed. Compare two saved handoffs against the same candidate.

| Saved manifest digest | Result and next step |
| --- | --- |
| Digest from an earlier manifest revision with the same required catalog objects. | The comparison exits nonzero. Keep baseline evidence pending and migration closed. Read the pinned attempt's job and source SHA and reconcile the saved digest against that attempt's private evidence before accepting any correction. |
| Digest of the manifest at `candidate_sha`. | The comparison exits zero. With the other baseline evidence verified, carry it forward to snapshot verification. |

Keep the handoff pending if the candidate manifest cannot be read. A digest
match verifies the manifest pin. The protected run's verdict and provenance,
catalog coverage and reader retirement still require their existing evidence.

### Offline handoff example after a rerun

In this synthetic handoff, `deploy.yml` reruns the same `<sha>` under the same
`<run-id>`. The run-level artifact listing can contain receipts from both attempts.

| Artifact name | Handoff disposition |
| --- | --- |
| `staging-admission-<sha>-<run-id>-1-private-music` | Historical evidence from attempt 1, which the rerun superseded. |
| `staging-admission-<sha>-<run-id>-2-private-music` | Use for verification when the run and required jobs identify attempt 2 and exactly one artifact has this name. |

If the handoff only identifies the run ID, the attempt records disagree, or
the selected attempt has no unique matching artifact, record **Staging
admission** as Pending and **Dispatch readiness** as Blocked. Resolve the
attempt and artifact from the same protected run before resuming step 3.
The name selects the artifact for verification. The verifier still checks
the archive digest and the receipt's own SHA, run ID and attempt.

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

See the [synthetic post-recovery handoff](ops/first-release-recovery-example.md)
for an example of recording the serving build and the pending release decision.

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

### Rehearse the paired backend readback offline

Run `python3 -I -B -m unittest discover -s .github/scripts -p test_production_backend_readback.py -v`
to exercise the [release caller](../.github/scripts/production_backend_readback.py)
with synthetic provider responses. It takes the reviewed candidate SHA, the
reviewed provider account, and a `WorkerPin` for each of `nntune-api` and
`nntune-stream`. Get both Workers' build, version and deployment IDs from
their reviewed publication evidence, independently of the readback. The
existing Web promotion receipt supplies only Web evidence.

The caller requires a silent, uncached reader that returns the decoded
provider result and enforces the supplied monotonic deadline through the
entire request. A match returns exit status 0. A refusal returns 1. Both
print only a verdict and any applicable fixed backend and reason labels.
The rehearsal has no live reader or deploy hook. A match describes serving
allocations at the time of the readback. Health, access and admission use
their existing checks.

### Check the observation record offline

Run `python3 ci/production_observation.py pins.json watch.json` on two private
outcome records. Collect the pins separately from the watch, using the accepted
release record and the protected rollout evidence. The verifier reads local
files and returns success only for a complete, healthy watch. The operator
still confirms that each evidence link resolves and supports the recorded outcome.
For final smoke, confirm the pinned attempt's `deploy-production` job and the
successful **Verify production serves the promoted build** step, including its
completion time. The offline job link check only matches the release run ID.

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
| `samples` | Start the opening readback at successful smoke and record it before the first five-minute boundary. Keep consecutive readbacks at most five minutes apart and the last exactly at `closed_at`, at least 30 minutes after smoke |

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

Before dispatch, record `<candidate-sha>` under **Candidate** and
`<private-maintenance-seed-evidence>` under **Protected switch-off**. Leave the
rollback artifact reference pending until the original rollout uploads
`production-rollback-intent-<original-rollout-run-id>-1` before shifting traffic.
Add its private link alongside the seed evidence. Protected recovery uses the
sealed intent from attempt 1 of that rollout.

After successful final smoke, the [promotion proof](ops.md#production-promotion-publishes-a-calver-release)
uses the separate `production-promotion-<candidate-sha>-<original-rollout-run-id>-1`
receipt for release publication.

| Item | Pin or evidence to record |
| --- | --- |
| Candidate | Full main SHA, artifact digest(s), exact-head required checks and per-PR review references |
| Staging attempt | `deploy.yml` run URL, run ID, attempt, health and private journey job URLs and conclusions, staging window and disable-deploy timing |
| Staging admission | Artifact name and Actions digest, opening and closing UTC, Web/API/Stream Worker, build SHA, version and deployment IDs, contract/policy/manifest digests, Product and QA references, final verifier job URL and pin match |
| Staging rollback | Last-good SHA, run/attempt, restore and identity readback |
| Production baseline | Protected source SHA, run/attempt and job URL, private provider branch and logical DB/OID reference, manifest digest, read time and `empty` verdict, separate no-live-user/data confirmation, writer fence and custodian |
| Pre-schema snapshot | Private snapshot reference, capture and completion UTC, branch and restore scope, expiry, verification reference and first schema apply boundary |
| Protected switch-off | Executors and protected controls for Access/invites and API/media, private seed evidence for the verified maintenance Worker/deployment/version/seed SHA/module digest, sealed intent custody reference and original rollout artifact link when published, expected denials and grant expiry boundary |
| Release review and dispatch | Independent reviewer, decision (initially `Pending`, then `APPROVE` or `BLOCK`), UTC and review reference for the pinned SHA and staging attempt, protected workflow and rollout run/attempt, final deployment/version and smoke result |
| Dispatch readiness | Pending. Record ready or blocked under the [existing qualification steps](#qualify-the-candidate), with a safe reference to the supporting evidence or blocker. |
| Health watch and recovery | Watcher, backup, UTC start/end, health and error baseline/threshold, readback schedule and results. Actual disposition pending until the [watch](#watch-the-first-30-minutes) or [switch-off](#switch-off-and-verify) has an outcome. Record the watch result when run and whether recovery was invoked. If used, record the trigger and UTC, recovery attempt and outcome, denial and final allocation readbacks. Link the release thread and product log entries. |

When a scheduled readback is still pending during the watch, name the check
and its scheduled UTC time in the private release handoff. For example, “The
Stream error threshold readback scheduled for 12:15 UTC is `Pending`. The
release disposition is `Pending`.”

### Personal-use exit assessment

Complete this assessment in the protected release thread. Use the **Staging
attempt** and **Staging admission** fields above for the same-build evidence.
Record pass or fail only when the protected attempt supplies a case-level
verdict. Cite the case ID and a safe reference to that verdict in the accepted
receipt or protected job evidence. An aggregate job verdict leaves each case
pending unless its own verdict is available.
See the [journey assertions](../ci/staging-admission.md#draft-contract-for-the-first-private-music-release).
Live billing is assessed separately.

- **Private invite scope:** Pending. Record the approved scope and access window
  for the owner and explicitly invited friends and family.
- **Unresolved issues and applicability:** Pending. Record each open issue or
  evidence gap, whether it applies to this invite scope, its effect on the
  decision and a safe reference. Use the **Staging rollback** and **Protected
  switch-off** fields above for recovery readiness and any known limits.

| Journey outcome | Result | Safe evidence reference |
| --- | --- | --- |
| `invite-acceptance` | Pending | — |
| `upload-to-library` | Pending | — |
| `play` | Pending | — |
| `seek` | Pending | — |
| `queue` | Pending | — |
| `anonymous-refused` | Pending | — |
| `unlisted-refused` | Pending | — |
| `revoked-or-expired-refused` | Pending | — |
| `video-off` | Pending | — |

### Offline handoff example with a skipped step

In this synthetic rehearsal, eight journey cases have passing observations
from one attempt. The operator deliberately skips `seek` when the time
reserved for the manual playback check runs out.

| Handoff item | Synthetic entry |
| --- | --- |
| Journey outcome `seek` | Pending |
| Unresolved issues and applicability | `seek` applies and was deliberately skipped. The manual playback check ran out of time. |
| Rehearsal verdict | Pending. Remaining step is `seek`. In the next offline rehearsal, pause playback, seek mid-track, resume playback and verify it advances from the sought position. |

The offline admission fixture returns `incomplete` and names `seek` as the
only incomplete case.
[Synthetic fixture](../ci/test_staging_admission.py)
