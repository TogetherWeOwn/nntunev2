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

M1 exit verification cannot pass yet (see
[docs/m1-exit-traceability.md](m1-exit-traceability.md) rows 3, 4 and 6):

1. Admin ingest must seed one curated album on staging. Matching
   depends on the MetaBrainz replacement, which is not yet imported or
   activated.
2. Rows 3 and 4 keep their ingest, play, seek, meter and queue legs
   parked by name until admin ingest can seed a curated track on
   staging; un-park each leg by removing its spec guard together with
   its gate entry.
3. Row 6 stays a GAP until the `staging-api` lane records
   `deployed=true`, `outcome=deployed` and a matching `source_sha`,
   and the rollback pointer (last good staging revision plus its
   deploy run) is recorded per the staging promotion checklist.
4. Only then run the exact-head journey (invite → subscription →
   admin-ingested catalogue → play/seek) and record the rollback
   pointer, updating the traceability matrix and this handoff in the
   same change that closes a row.

## Last verification

- Exact-head Actions evidence for
  `b926dd6db10730c0986ae72a4c09fbd5125b34d8`:
  - ci: https://github.com/TogetherWeOwn/nntunev2/actions/runs/42870751010
    (conclusion `success`, push event on that SHA)
  - deploy: https://github.com/TogetherWeOwn/nntunev2/actions/runs/42870749181
    (conclusion `success`, push event on that SHA)
- In the deploy run, `staging-web` and `staging-api` executed their
  deploy steps (`Deploy to staging` and `Deploy api to staging` /
  `Record API deployment outcome` respectively, all `success`),
  whereas `staging-mb` (`Prepare`/`Build`/`Generate`/
  `Deploy mb to staging`) and `staging-ingest` (`Prepare`/`Build`/
  Hyperdrive gate/`Generate`/`Deploy ingest to staging`) skipped
  theirs — only the configuration check and `Record outcome` ran,
  so those lanes were unconfigured and their green jobs are not
  deploys. It does not prove M1 exit: a green run is not staging
  evidence (see blockers below), and row 6 stays a GAP until the
  `staging-api` lane records `deployed=true`, `outcome=deployed`
  and a matching `source_sha` with the rollback pointer under the
  staging promotion checklist.
- Docs-only changes use focused static and diff checks plus exact-head
  Actions runs; no local full suite. This change's verification is
  its CI `ci-ok` gate plus review.

## Next unfinished step

First unfinished step: admin ingest seeds one curated album on
staging (blocker 1 above). Do not start the M1 exit journey until
the parked legs in rows 3 and 4 can be un-parked, the `staging-api`
lane records `deployed=true`, `outcome=deployed` and a matching
`source_sha`, and the rollback pointer is ready (blockers 2–4).

This file is a dated snapshot. Live assignments, dependencies and
status remain in the assignment tracker.
