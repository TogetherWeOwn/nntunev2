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
   and OIDs. The authorized staging database owner provides direct staging
   `CONNECT` at issuance, with `public` schema `USAGE` from the authorized
   schema executor and the intended membership shape. Read back both logins
   at that baseline. Stop if the issuer cannot establish it before application
   grants, or if either identity, the target or retirement authority is
   unverified. Before issuing, confirm the grant, verification and withdrawal
   path against the landed instructions from
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
3. **Grant and verify base rights (77f), then apply the overlay (77c).** After
   preflight passes, use the landed lifecycle's `grant` and `verify` actions
   for exact base rights and both routed identities. Require the publication
   writer's five column-scoped row-lock grants and enabled update guards, and
   confirm the reader has no update access. Stop on ACL or identity drift.
   77c sets `PRIVATE_TRIAL_SCHEMA_WRITER_ROLE` and
   `PRIVATE_TRIAL_SCHEMA_READER_ROLE` in staging for that pair. Dispatch
   [`migrations-staging-apply`](../../.github/workflows/migrations-staging-apply.yml)
   from protected `main` with `file=private-trial`, `mode=single`, the full
   reviewed `ref`, `applied_by` and the recorded exact-DDL approval in
   `board_approval`. Require a successful job, its bundle digest and matching
   admission-table receipt. 77f uses `verify-overlay` to check the effective
   ACLs for both routed roles before 77c binds them. See the
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
   [protected cleanup dispatch](#protected-cleanup-dispatch)
   for tone A against the exact trial scope, invite, upload session, job,
   track, object and bucket. Require the admission and digest reference to be
   retired, the exact media and digest page versions absent, and the selected
   source row removed. Stop on a refusal, shared reference or ambiguous readback.
   For a partial delete or unconfirmed cleanup commit, resume using the same
   pins and confirm both versions, including any version already absent.
   Handle tone B and its admission through the separate fixture disposition.
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

### Protected cleanup dispatch

After containment, supply `PRIVATE_TRIAL_CLEANUP_CONTAINMENT` in the protected
`staging` Environment. Use the verified disable run's `build_sha` and its
`api` and `stream` pins, each with `deployment_id` and `version_id`. Set
`session_revoked`, `cohort_cleared` and `armed_runs_drained` to `true` only
after completing and checking those steps. Leave the upload, playback,
journey, observation and Stream deploy arms unset. Keep the original staging
storage settings available for the runner to match against the receipt.
If a later staging deploy changes either serving pin, obtain fresh disable
receipts before dispatch and retain the original fixture scope and hash.

Store the selected tone A receipt as `PRIVATE_TRIAL_CLEANUP_SCOPE`. Supply
`object`, `generation`, `map`, `track`, `session`, `job`, `issuer`, `invite`,
`cutoff` (Unix seconds), `media_version` and `page_version`, and the original
`provider` object with `environment`, `library`, `configuration`,
`root_version`, `bucket`, `endpoint` and `region`. Pin the independently
verified cleanup login with `database_host`, `database_port`, `database_role`
and `database_role_oid`. Use `PRIVATE_TRIAL_CLEANUP_DATABASE_URL` for its
direct `nntunev2_staging` connection with `sslmode=require`. Keep the role
separate from upload, read, publication and enrollment. Use
`PRIVATE_TRIAL_CLEANUP_S3` for JSON with `access_key` and `secret_key` from the
independent exact-version DELETE and HEAD credential. Its policy must allow
only the two selected fixture keys and no list or bulk operations. Record the
SHA-256 of the exact UTF-8 credential JSON as `s3_credential_sha256` in the
scope. Before arming, verify both identities and qualify the provider with the
same no-list policy on a disposable versioned fixture. Require version-specific
HEAD to return the selected version while present and an empty HTTP 404 after
exact-version deletion. Treat HTTP 403, ambiguity or a policy that needs list
access as a stop. Keep the scope and credentials in protected custody.

Once the cutoff, retention and lease windows have elapsed, dispatch
[`deploy`](../../.github/workflows/deploy.yml) from protected `main` with
`private_trial_cleanup_scope_sha256` set to the SHA-256 of the exact UTF-8
scope JSON. The job checks the disable deployments under the staging deploy
lock and invokes the isolated Worker once. `confirmed` means two verified
version absences and a committed source finalization. Stop on `refused`,
`withheld`, `unconfirmed` or missing evidence. Reconcile SQL and provider state
before a fresh dispatch with the original scope and hash. Keep those pins
until cleanup is confirmed, then clear the cleanup settings in custody and
withdraw its scoped access.

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
