# First private staging trial

Use the protected `staging` Environment, `nntunev2_staging`, and the dedicated
versioned staging fixture bucket. Keep the upload, playback and journey arms
unset until step 5. Use the original synthetic FLACs and synthetic accounts.
Record the reviewed commit, run and job links, serving build and version IDs,
fixture receipt references, and pass or stop verdicts. Keep connection details,
keys, passwords, cookies, invite codes and cohort values in secure custody.

## Setup and trial

1. **Prepare the isolated database.** The setup operator reads back the separate branch's
   `nntunev2_staging` database OID, canonical `postgres` owner, migration 0063
   checksum, and both direct and routed addresses. Arrange authorized owner
   and executor connections for that branch and issue the two dedicated SQL
   logins through its authorized issuer. Use the [role names and membership
   rules](../ops.md#private-trial-schema) from the existing lifecycle. The owner
   needs authority to grant CONNECT, fence, drain and retire the pair. Before
   dispatch, confirm the executor owns the five lock tables and can install
   the overlay and grants. The `public` schema owner must authorize `CREATE`
   and grant `USAGE WITH GRANT OPTION` to the executor. The executor needs
   grantable column access, `REFERENCES` for the overlay's foreign keys, and
   grantable `EXECUTE` on both reviewed 0052 routines. Confirm the two SQL-created
   identities can be routed on the provider and that both lack access to other
   connectable databases. Retain the original protected scope for withdrawal.
2. **Check protected inputs.** Store the existing role-scope JSON in
   staging as `ISOLATED_TRIAL_ROLE_SCOPE`. Set `ISOLATED_TRIAL_TARGET` to a
   reviewed JSON object with `database_oid` and `executor_oid` (numbers),
   `direct` and `routed` (exact DNS hostnames), `direct_port` and `routed_port`
   (numbers), and `owner`, `executor`, `reader`, `writer` (exact connection
   usernames including any provider suffix). Set the four protected
   `ISOLATED_TRIAL_{OWNER,EXECUTOR,READER,WRITER}_URL` secrets. Dispatch
   [`staging-trial-isolated-setup`](../../.github/workflows/staging-trial-isolated-setup.yml)
   with `check` from protected `main`, the full reviewed `ref` at its tip, and
   the recorded `board_approval` reference. Require both authority verdicts
   to pass. The workflow uses only isolated inputs and refuses the shared
   catalog pin. Keep the shared staging URLs, roles and routing in service.
3. **Grant and verify access.** Dispatch `grant`, then `verify-overlay`
   with the reviewed ref and approval. Require the exact overlay ACL verdict
   and separate routed reader and writer verdicts. `grant` installs the base
   grants, row-lock guards and overlay in one executor transaction after the
   owner's CONNECT grant. If routing fails after that transaction, keep the
   trial disarmed and either retry with the same pins and corrected dedicated
   connection or fence and retire. Hand the verified isolated pair to the
   trial operator through protected custody.
4. **Verify staging bindings and cleanup access.** Verify isolated routing
   for the exact trial build before arming. Check the candidate's
   mounted upload caller and actual staging identities for all four dedicated,
   uncached Hyperdrive bindings (`UPLOAD`, `AUTH`, `PUBLISH`, `ENROLL`). Use the
   scoped role assigned to each binding by the reviewed caller. Verify the
   staging MusicBrainz binding, API–Stream service bindings, dedicated fixture
   bucket and matching storage configuration and root versions. Confirm a
   protected, reviewed runner can retire the selected fixture and confirm exact
   version absence using separate, scoped cleanup access. Record a separate
   reuse or retirement disposition for the prepublished companion and its trial
   admission. Stop if any binding, identity or cleanup path is missing.
   Before arming, confirm a reviewed drill entry point can use an authorized
   synthetic session after companion admission and before cutoff. Confirm the
   selected staging origin can pace delivery so ciphertext remains outstanding
   after the first plaintext chunk reaches the unbuffered relay client. Stop
   if either prerequisite is missing.
   See [#1816](https://github.com/TogetherWeOwn/nntunev2/pull/1816) and the
   [staging binding instructions](../ops.md#private-stream-staging).
5. **Arm and qualify one synthetic journey (77c).** Follow the
   [private music journey](../../e2e/README.md#private-music-journey-preparation)
   for the fresh owner step-up, recipient invite and prepublished tone B
   companion. Set `PRIVATE_PLAYBACK_STAGING_READ_OBSERVATION` for the prepublished
   tone B track before the armed push so Stream reads the selector at deploy
   time. Arm upload, playback and the private journey on the same protected
   `main` push with Stream deployment enabled. Require successful API, Stream
   and Web deploy readbacks for that run and commit, plus matching `/api/build`
   and `/healthz` before and after playback. Require tone A's upload receipt to
   match the pinned digest and a fresh canonical track visible after Library
   Refresh. Check playback advances, the authenticated nonzero byte range is
   exact, paused seek fires `seeked` and resumes from the selected position, and
   Next selects tone B. After companion admission and before the cohort cutoff,
   use fresh tone B plays for the
   [relay disconnect drill](../ops.md#private-stream-staging). Require upstream
   abort and settlement for the acknowledged byte prefix, plus a separate EOF
   control that reports completion. Clear the observation setting after the
   drill. At cutoff, require Library and playback retry to refuse. Stop on any
   missing or mixed-build readback. Retain the selected tone A upload,
   publication and versioned storage pins for cleanup. Keep tone B's pins
   separately for its recorded disposition.

## Retire and recover

1. **Contain and retire the selected fixture (77c).** Stop new trial activity.
   After the cutoff checks, revoke the stepped-up owner session. Clear
   `PRIVATE_MUSIC_STAGING_JOURNEY`, `PRIVATE_PLAYBACK_STAGING_READ_OBSERVATION`,
   the single-use fixture and cohort settings, then follow the
   [disable deploy](../ops.md#private-stream-staging). Cancel queued or running
   armed deploys and use a fresh protected `main` push so the gate reads the
   cleared settings. Require a deployed API and a Stream receipt showing
   `private_playback_enabled=false` before removing the Stream deployment arm
   and trial runtime bindings.
   Keep the selected pins and authorized cleanup access until cleanup finishes.
   After the cohort cutoff and all retention and lease windows, run the
   [selected-fixture retirement path](../../workers/api/src/handlers/private_trial/cleanup.rs)
   for tone A against the exact trial scope, invite, upload session, job,
   track, object and bucket. Require the admission and digest reference to be
   retired, the exact media and digest page versions absent, and the selected
   source row removed. Stop on a refusal, shared reference or ambiguous readback.
   For a partial delete or unconfirmed cleanup commit, resume using the same
   pins and confirm both versions, including any version already absent.
   Handle tone B and its admission through the separate fixture disposition.
2. **Withdraw access.** Once the trial operator removes the bindings, dispatch
   `fence` and `retire` using the original scope and the reviewed isolated
   workflow. Require reports that both logins are fenced and drained, their
   overlay and base grants and lock guards are withdrawn, and their names and
   OIDs are absent. The overlay and its OID-bound receipt remain for data
   reconciliation. Remove retired credentials from custody and withdraw any
   temporary executor grants through their original grantor. If withdrawal
   refuses, reconcile with the grant owner while both logins stay fenced.

After an interruption, reconcile the last run and actual staging state before
retrying. Use the same reviewed overlay bytes and role OIDs to resume. An
identical install reuses the existing overlay. A conflicting receipt or
replacement role goes to the schema owner before reapply. If a deploy or trial
fails, verify the disabled state and reconcile the upload state, publication and
storage pins before starting a fresh invite and attempt. Treat an unknown upload
result as unconfirmed and use only the reviewed replay path for the same attempt.
If a partial upload has no matching completed publication, stop and hand it to
77c for reconciliation through the reviewed upload path. The selected-fixture
retirement path requires a matching completed publication.
