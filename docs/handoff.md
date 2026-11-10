# Handoff (dated snapshot — not a live work queue)

Snapshot date: 2026-11-10 (UTC).

## Current source state

- Default branch `main` at `b926dd6d` (feat(web): add versioned
  attribution inventory and validation contract).
- Public roadmap entry point added 2026-11-09 (merged change
  `774af103`): [ROADMAP.md](../ROADMAP.md), [README.md](../README.md).
- Mission: invite-only listening from a shared curated catalogue —
  lossless first, official video embed fallback, otherwise hidden.
  One concurrent stream, private playlists, no downloads. Invite-gated
  staging subscriptions in test mode; live billing not activated.
- Product is pre-launch; no production service is serving.

## Relevant public branches and changes

- `main` head `b926dd6d` — attribution inventory and validation
  contract.
- This handoff change itself is a docs-only change on its own branch;
  after merge it does not change runtime behaviour.

## Concrete blockers

- M1 exit verification pending: exact-head staging evidence journey
  (invite → subscription → admin-ingested catalogue → play/seek)
  plus rollback evidence, on a deploy whose access control and
  build-identity legs can run. Tracked in the traceability matrix as
  PARTIAL/GAP rows; closing those rows is separate work.
- The staging deployment-evidence basis depends on the staging
  lanes reporting deployed status with matching source revisions;
  skipped lanes are not evidence.

## Last verification

- Exact-head Actions evidence for `b926dd6d` is the CI record on
  that revision (see the Actions run links on the merged change).
- Docs-only changes use focused static and diff checks plus exact-head
  Actions runs; no local full suite. This change's verification is
  its CI `ci-ok` gate plus review.

## Next unfinished step

Complete M1 exit verification on the next staging promotion: run the
exact-head deploy, the post-deploy end-to-end journey and the
rollback-pointer procedure, then update the traceability matrix and
this handoff in the same change that closes a row.

This file is a dated snapshot. Live assignments, dependencies and
status remain in the assignment tracker.
