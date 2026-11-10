# Deploy and rollback runbook

This runbook summarises the promotion and rollback procedures. The
authoritative inventory and checklist live in
[docs/ops.md](../ops.md); verify against it before any operation.

## Prerequisites

- Exact-head CI green on the promotion revision: the `ci-ok`
  aggregate gate, `gitleaks` and `pr-lint` on that revision. Record
  the promotion revision and the Actions run and job links. Full
  suites run on Actions only.
- For staging: the staging deploy workflow run for the promotion
  revision with the web job conclusion `success` (not `skipped`), and
  the post-deploy staging end-to-end pass on the same run. Same-build
  evidence only when the deployed build identity returns the
  promotion revision itself; a health response alone never proves
  which revision is serving.
- Access control intact on the staging hostname: owner policy and
  service-token policy still gate it; credentials available to the
  job through authorized injection (names only, never values).
- For the API worker: executed cache-disabled gate, read-only schema
  verification and deploy steps succeeded, with `deployed=true` on
  the promotion run. A config-skipped lane is not deploy evidence.

## Deploy (staging)

1. Record the exact head revision being promoted and its Actions
   evidence.
2. Push `main` to trigger the staging deploy workflow. Require the
   web job `success` plus the same-run post-deploy end-to-end result.
3. Verify staging health and the build identity surface. Claim
   same-build evidence only on an exact revision match.
4. Record the rollback pointer **before** any later promotion: the
   last good staging revision and its successful push-triggered
   deploy run with the web job `success`.

## Rollback

Staging rollback is a redeploy, never a data restore.

1. Open the recorded last-good deploy run for the rollback target
   revision.
2. Choose **Re-run all jobs**. Do not dispatch on the old revision
   manually — the web lane is push-only and will be skipped.
3. Require the rerun's web job `success` plus staging health.
4. A rerun redeploys every configured lane; that is not verified
   per-worker rollback unless each worker's own outcome evidence is
   recorded. Worker rollback restores worker code only; binding,
   schema or data changes need their own rollback statement.

## Production

Production promotion is manual-only and human-gated. It requires
release approval, a pre-seeded single-version production deployment,
the secret-shape gate passing on the promotion revision, and staging
evidence on that same revision. A successful production promotion
publishes a calendar-version release through the shared releaser; a
re-promotion of an already-tagged commit skips. Do not promote
production from this page alone — complete the checklist with the
release owner.

## Safe failure paths

- Any required check red: fix in code or record a flake/quarantine
  decision with a tracker link; never dismiss.
- Staging schema gate red: a merged migration not yet applied out-of-band
  is the usual cause. Apply and record it through the migration
  procedure in [docs/ops.md](../ops.md), then re-verify. Never use
  record-only to bless unapplied bytes or rewrite history rows.
- Unknown migration outcome (timeout or disconnect): reconcile the
  exact checksum receipt and catalog effects through the authorized
  evidence path before any retry. Never infer rollback from a missing
  acknowledgement.
- Missing deployment evidence (skipped lane, `unknown` build
  identity): treat as not deployed, not as a pass. Wire the missing
  piece and re-run.
