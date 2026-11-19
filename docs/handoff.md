# NNTune v2 handoff

Source and evidence snapshot for 2026-11-21 (UTC) at `main` revision
[`eb7d2ee62b2048a07a997ebe09f3d781922ca8e6`](https://github.com/TogetherWeOwn/nntunev2/commit/eb7d2ee62b2048a07a997ebe09f3d781922ca8e6).
Start with the [roadmap](../ROADMAP.md) for agreed scope and the
[M1 matrix](m1-exit-traceability.md) for the staging exit.

## Storage and catalogue

- The target in [ADR 0015](adr/0015-standard-s3-storage-boundary.md) is
  authenticated programmatic ingest of owner-supplied audio, ciphertext-only
  uploads to standard S3, scoped plaintext digest deduplication and seekable
  authenticated reads. Keep the existing readers and inventory during the
  transition. The replacement must retire the obsolete NZB fetch, verify and
  repair path.
- Main contains the [proposed ingest session contract](programmatic-ingest-contract.md),
  inactive request and replay helpers, and code for sealing, exact upload receipts and full
  readback. [#1659](https://github.com/TogetherWeOwn/nntunev2/pull/1659) pins the
  returned media version, [#1661](https://github.com/TogetherWeOwn/nntunev2/pull/1661)
  pins each sidecar version, and [#1662](https://github.com/TogetherWeOwn/nntunev2/pull/1662)
  assembles the completed inventory. [#1663](https://github.com/TogetherWeOwn/nntunev2/pull/1663)
  feeds those pins into the [enrolled verifier](../crates/nntune-core/src/digest_readback.rs)
  and returns in-memory evidence after authenticated readback, digest and FLAC validation.
- Next, persist the exact version pins in fenced ingest state and wire
  verified readback into publication. Session routes, authorised
  dedupe reuse, catalogue publication through the new path and encrypted range
  playback still need integration and qualification. Follow ADR 0015 for
  specialist acceptance on the exact design, verified producer outputs,
  disposable-bucket validation, coexistence and retirement.
- The former MusicBrainz mirror was retired. Offline CC0 projections have
  landed, including [release labels](https://github.com/TogetherWeOwn/nntunev2/pull/1576).
  The bounded replacement still needs an accepted import and read activation.
  Weekly refresh automation remains disabled. Use the
  [ADR 0007 amendment](adr/0007-mb-mirror.md#2026-11-07-amendment-retirement-and-conditional-bounded-replacement)
  and [attribution inventory](attributions.md#scope-and-provenance).

## Player

- The Library uses a persistent native player with
  [accessible absolute seek](https://github.com/TogetherWeOwn/nntunev2/pull/1540),
  [playback speed](https://github.com/TogetherWeOwn/nntunev2/pull/1545),
  [sleep timer](https://github.com/TogetherWeOwn/nntunev2/pull/1547) and
  [Media Session position](https://github.com/TogetherWeOwn/nntunev2/pull/1601).
  These landed controls still need the real curated-track staging journey.
- The [signed R2 rendition receiver](https://github.com/TogetherWeOwn/nntunev2/pull/1651)
  serves an exact Ogg/Opus artifact when given a valid rendition grant. Grant
  issuance, the browser relay and rendition-compatible meter inputs are
  separate work. This receiver is distinct from ADR 0015's encrypted S3 path
  and from displaying the quality of the rendition actually played.
- The 2026-11-04 deferrals still apply to audio leveling, verified
  played-rendition quality details and speculative next-track prefetch.
  [Now Playing lyrics UI](https://github.com/TogetherWeOwn/nntunev2/pull/1572)
  is preparation. Source rights and activation remain subject to the
  [attribution contract](attributions.md#scope-and-provenance).

## M1 staging and release

The snapshot SHA has its own [CI run](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44956283454)
and [deploy run](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44956282557).
The latest completed evidence reviewed for this handoff comes from pushes of
[`7b994af7e24978dd6890b71356b82ce8c8fea74a`](https://github.com/TogetherWeOwn/nntunev2/commit/7b994af7e24978dd6890b71356b82ce8c8fea74a)
on `main`.

| Evidence | Published result |
| --- | --- |
| [CI run](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953849260) | [`ci-ok`](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953849260/job/137001899533) and [`docs-links`](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953849260/job/136997481443) passed. |
| [Deploy run](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953848589) | [Web](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953848589/job/136997504911) and [API](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953848589/job/136997504894) executed their deploy steps successfully. The API cache-disabled, read-only schema and off-and-drained readback steps passed. [MB](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953848589/job/136997504919) and [ingest](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953848589/job/136997603536) skipped their deploy steps. |
| Same-run post-deploy checks | [Synthetic health smoke](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953848589/job/136998802714) passed. The [full journey and its evidence gate](https://github.com/TogetherWeOwn/nntunev2/actions/runs/44953848589/job/136998928254) failed with 631 passed, 12 skipped and 1 failed. The [fresh-account Library case](../e2e/tests/catalogue.spec.ts) failed its empty-array response assertion. |

1. Reconcile the fresh-account Library response with the staging publication
   state and the [E2E evidence contract](../e2e/README.md#evidence). The empty
   Library assertion runs when `SHARED_CATALOGUE_PUBLISHED` is false or unset.
   Enable that flag only after a real curated release is published.
2. Supply a real admin-published matched release for staging. The populated
   catalogue, ingest, playback, seek and related parked checks need a curated
   staging source. Restore the applicable spec legs and evidence-gate entries
   together. Exercise owner-issued invite redemption and active test-mode
   entitlement. The [M1 matrix](m1-exit-traceability.md#matrix)
   identifies each gap.
3. For a promotion candidate, record the serving web and API SHAs at the
   start and end of the Access-protected journey. Match both identities to
   the promotion SHA and retain the passing same-run result.
   Record Worker identity and outcome for each deployed lane. Use the
   [staging readout template](staging-validation-readout-template.md) and the
   [promotion checklist](ops.md#staging-promotion-checklist).
4. Before promotion, record the last-good staging version, SHA and successful
   push deploy run for each affected Worker. Follow the [deploy lock and rollback procedure](ops.md#staging-deploy-lock-cutover-and-rollback)
   and [Worker custody checks](ops.md#staging-worker-custody) before restoring
   an exact version. Retain [per-Worker rollback evidence](ops.md#future-worker-deploy-lanes-required-rollback-evidence).
   An accepted rollback pointer and successful restore run are still outstanding
   in this readout.

M1 exit remains open. The [current production preflight](../.github/workflows/deploy-production.yml#L128)
accepts only `bootstrap-maintenance` and refuses application promotion even
when the lane is enabled. The staging-evidence gate applies to both
maintenance bootstrap and application promotion. It requires the full
passing journey and an [attempt-bound candidate admission receipt](../ci/staging-admission.md).
The [admission policy](../ci/staging-admission-policy.json) has no approvals.
Product approval of the journey contract and separate QA approval of the
executable evidence mapping are still required. Release Manager acceptance
and environment gates also apply. See the
[maintenance seed precondition](ops.md#production-seed-precondition-one-time)
and [production secret-shape gate](ops.md#production-worker-secret-shape-gate).
The [CalVer release receipt](ops.md#production-promotion-publishes-a-calver-release)
is recorded after production promotion and smoke.
Consult [GitHub releases](https://github.com/TogetherWeOwn/nntunev2/releases)
for published release history.
