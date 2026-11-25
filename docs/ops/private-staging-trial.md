# First private staging trial

Use the protected `staging` Environment, `nntunev2_staging`, and the dedicated
versioned staging fixture bucket. Keep the upload, playback and journey arms
unset until step 5. Use the original synthetic FLACs and synthetic accounts.
Record the reviewed commit, run and job links, serving build and version IDs,
fixture receipt references, and pass or stop verdicts. Keep connection details,
keys, passwords, cookies, invite codes and cohort values in secure custody.

## Setup and trial

1. **Issue the scoped pair (77f).** The Chief of Staff owns the secure handoff of
   the temporary staging setup login. 77f uses the authorized issuer and pins
   the staging catalog plus the canonical reader and publication-writer names
   and OIDs. Before application grants, read back two distinct logins with
   direct staging `CONNECT`, `public` schema `USAGE` and the intended membership
   shape. Stop if issuance or retirement authority, the target, or either
   identity is unverified. Issuance uses the separate authorized provider
   route. Before issuing, confirm the grant, verification and withdrawal path
   against the landed instructions from
   [#1828](https://github.com/TogetherWeOwn/nntunev2/pull/1828). Follow the
   Operator's secure handoff for the temporary login's retirement.
2. **Preflight before application grants (77f).** Supply the pinned scope as
   `PRIVATE_TRIAL_ROLE_PREFLIGHT_SCOPE` in staging and dispatch
   [`staging-trial-role-preflight`](../../.github/workflows/staging-trial-role-preflight.yml)
   from protected `main`. Require every summary verdict to pass against the
   recorded 0063 baseline, including database isolation, membership and absence
   of application privileges. Stop on a refusal, skipped job or missing verdict.
   Clear the Environment secret after readback and retain the original scope
   securely for retries and retirement. See
   [#1814](https://github.com/TogetherWeOwn/nntunev2/pull/1814).
3. **Verify base rights (77f), then apply the overlay (77c).** Read back the
   exact base grants and both routed identities before handoff. Require the
   publication writer's five column-scoped row-lock grants and enabled update
   guards, and confirm the reader has no update access. Stop on ACL or identity
   drift. 77c sets `PRIVATE_TRIAL_SCHEMA_WRITER_ROLE` and
   `PRIVATE_TRIAL_SCHEMA_READER_ROLE` in staging for that pair. Dispatch
   [`migrations-staging-apply`](../../.github/workflows/migrations-staging-apply.yml)
   from protected `main` with `file=private-trial`, `mode=single`, the full
   reviewed `ref`, `applied_by` and the recorded exact-DDL approval in
   `board_approval`. Require a successful job, its bundle digest and matching
   admission-table receipt. 77f verifies the effective overlay ACLs for both
   routed roles before 77c binds them. See the
   [schema and lock instructions](../ops.md#private-trial-schema),
   [#1800](https://github.com/TogetherWeOwn/nntunev2/pull/1800) and
   [#1817](https://github.com/TogetherWeOwn/nntunev2/pull/1817).
4. **Verify staging bindings and cleanup access (77c).** Check the candidate's
   mounted upload caller and actual staging identities for all four dedicated,
   uncached Hyperdrive bindings (`UPLOAD`, `AUTH`, `PUBLISH`, `ENROLL`). Use the
   scoped role assigned to each binding by the reviewed caller. Verify the
   staging MusicBrainz binding, API–Stream service bindings, dedicated fixture
   bucket and matching storage configuration and root versions. Confirm a
   protected, reviewed runner can retire the selected fixture and confirm exact
   version absence. Stop if any binding, identity or cleanup path is missing.
   See [#1816](https://github.com/TogetherWeOwn/nntunev2/pull/1816) and the
   [staging binding instructions](../ops.md#private-stream-staging).
5. **Arm and qualify one synthetic journey (77c).** Follow the
   [private music journey](../../e2e/README.md#private-music-journey-preparation)
   for the fresh owner step-up, recipient invite and prepublished tone B
   companion. Arm upload, playback and the private journey on the same protected
   `main` push with Stream deployment enabled. Require successful API, Stream
   and Web deploy readbacks for that run and commit, plus matching `/api/build`
   and `/healthz` before and after playback. Require tone A's upload receipt to
   match the pinned digest and a fresh canonical track visible after Library
   Refresh. Check playback advances, the authenticated nonzero byte range is
   exact, paused seek fires `seeked` and resumes from the selected position,
   Next selects tone B, and cutoff denies Library and playback retry. Stop on
   any missing or mixed-build readback. Retain the selected upload, publication
   and versioned storage pins for cleanup.

## Retire and recover

1. **Contain and retire the selected fixture (77c).** Stop new trial activity.
   After the cutoff checks, revoke the stepped-up owner session, clear the
   single-use fixture and cohort settings, and follow the
   [disable deploy](../ops.md#private-stream-staging). Cancel queued or running
   armed deploys and use a fresh protected `main` push so the gate reads the
   cleared settings. Require a deployed API and a Stream receipt showing
   `private_playback_enabled=false` before removing the Stream deployment arm
   and trial runtime bindings.
   Keep the selected pins and authorized cleanup access until cleanup finishes.
   After the cohort cutoff and all retention and lease windows, run the
   [selected-fixture retirement path](../../workers/api/src/handlers/private_trial/cleanup.rs)
   against the exact trial scope, invite, upload session, job, track, object
   and bucket. Require the admission and digest reference to be retired, the
   exact media and digest page versions absent, and the selected source row
   removed. Stop on a refusal, shared reference or ambiguous version readback.
   For a partial delete or unconfirmed cleanup commit, resume using the same
   pins and confirm both versions, including any version already absent.
2. **Withdraw access (77f with 77c and the schema owner).** Once trial bindings
   are removed, clear both overlay role variables. Using the original pinned
   scope, fence and drain the two logins, withdraw their overlay grants with
   the schema owner, then withdraw the base grants and retire the roles via
   [#1828](https://github.com/TogetherWeOwn/nntunev2/pull/1828). Require the
   writer's lock-only UPDATE access gone before guard removal, and both pinned
   names, OIDs and sessions absent before closing. Remove retired credentials
   from custody. If reversal refuses because of residual privileges, keep the
   logins fenced and have the grant owner reconcile the exact access before
   resuming withdrawal.

After an interruption, reconcile the last run and actual staging state before
retrying. For a partial role setup, 77f compares the pinned identities and
effective grants with the landed lifecycle contract and resumes only a supported
step. The baseline preflight applies before application grants. For an
unconfirmed overlay apply, 77c checks the receipt and ACLs, then retries with the
same reviewed bytes and role pair if they match, or if the overlay is absent.
The overlay treats an identical install as a no-op. A conflicting receipt or
replacement role goes to the schema owner before reapply. If a deploy or trial
fails, verify the disabled state and reconcile the upload state, publication and
storage pins before starting a fresh invite and attempt. Treat an unknown upload
result as unconfirmed and use only the reviewed replay path for the same attempt.
If a partial upload has no matching completed publication, stop and hand it to
77c for reconciliation through the reviewed upload path. The selected-fixture
retirement path requires a matching completed publication.
