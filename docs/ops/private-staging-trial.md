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
   grantable `EXECUTE` on both reviewed 0052 routines and `EXECUTE` on
   `storage_object_intent_append_only()` to install the locator trigger. Confirm
   both logins lack access to other connectable databases. Retain the original
   protected scope for withdrawal.
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
   and separate reader and writer verdicts through the provider's routed
   endpoint. After the owner grants CONNECT, the executor installs the base
   grants, guards and overlay in one transaction. If routing fails, keep the
   trial disarmed and correct the dedicated connection or fence and retire.
   Hand the verified pair to the trial operator through protected custody.
4. **Verify isolated serving and cleanup access.** Keep the trial disarmed
   until a separately reviewed serving lane proves that the exact build's
   account, invite, session, publication, Library, playback and cleanup
   connections all reach the same pinned isolated database.
   Check the mounted upload caller and actual identities for all four dedicated,
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
   isolated build with Stream enabled. Require successful API, Stream
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
   isolated lane's disable deploy procedure. Cancel queued or running armed
   deploys and use a fresh protected build so the gate reads the cleared
   settings. Require a deployed API and a Stream receipt showing
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
2. **Withdraw access.** Once the trial operator removes the bindings, dispatch
   `fence` and `retire` using the original scope and the reviewed isolated
   workflow. Require reports that both logins are fenced and drained, their
   overlay and base grants and lock guards are withdrawn, and their names and
   OIDs are absent. Retirement accepts an earlier overlay bundle digest when
   the receipt, pinned role OIDs, executor ownership and ACLs pass. The overlay
   and its receipt remain for data reconciliation. Remove retired credentials
   from custody and withdraw any temporary executor grants through their original
   grantor. If withdrawal refuses, reconcile with the grant owner while both
   logins stay fenced.

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
HEAD to return the selected version while present. Require exact-version
DELETE to return an empty HTTP 204 and, if it includes `x-amz-version-id`,
require that value to match the selected version. Require the subsequent
version-specific HEAD to return an empty HTTP 404. Repeat the same DELETE and
HEAD against the now-absent version and require the same status, body and
optional version header checks. Treat HTTP 403, ambiguity or a policy that
needs list access as a stop. Keep the scope and credentials in protected custody.

#### Provider-only preflight

For a hosted run, use [staging-trial-provider-preflight](../../.github/workflows/staging-trial-provider-preflight.yml).
For the fixture boundary, provision and verify a reachable HTTPS ingress
forwarding to its loopback listener through the protected rollout. The
disposable qualification uses a runner-local endpoint.
Prepare the two disposable versions and read back the credential policy as
described below. Supply the four settings below as secrets in the existing
`staging` Environment for this run. The endpoint and region secrets must match
the staging Actions variables of the same names. During the protected
provider-only window, keep the upload, playback and journey arms unset and
bind both endpoint inputs to the reviewed fixture ingress. The workflow
compares the settings before sending a request and keeps the variable values
out of its logs. Clear the temporary inputs or restore the prior settings on
closeout.

Before arming, verify upload and playback on the reviewed serving endpoint.
Also require a passing [cleanup qualification](#protected-cleanup-dispatch)
on the exact endpoint, region and bucket the cleanup runner will match to
the trial receipt. If restoring settings changes any of those values,
repeat the exact-version HEAD and DELETE qualification on fresh disposable
versions at the restored target. Qualify its two-key cleanup scope separately.
Keep the trial disarmed until both checks pass. The fixture boundary serves
HEAD and DELETE only.
Keep the exact scope JSON and the disposable credential in
protected custody. Use the SHA-256 of the exact UTF-8 bytes supplied as the
scope secret. The dispatch inputs contain only the reviewed full `main` commit
SHA and the lowercase scope digest.

```sh
gh workflow run staging-trial-provider-preflight.yml --ref main \
  -f ref="$reviewed_sha" -f scope_sha256="$scope_sha256"
```

GitHub must resolve the dispatch to that reviewed commit. Open the run and
record its URL and the fixed verdict from the successful probe step. A skipped
job or a rerun supplies no qualification evidence. Reconcile any interrupted or
failed attempt before preparing a fresh pair and dispatch. Withdraw the
disposable credential and clear the run's Environment secrets after use.

For the controlled manual path, build the standalone probe in the protected
qualification environment from the reviewed commit before loading provider
credentials. Use Node 24+, an absolute disposable `probe_dir` and the
`wasm-bindgen` CLI version pinned in `workers/ingest/Cargo.lock`.

```sh
cargo build --locked --manifest-path workers/ingest/Cargo.toml \
  --target wasm32-unknown-unknown --no-default-features --features fixture-provider-preflight \
  --target-dir "$probe_dir/target"
wasm-bindgen --target nodejs --out-dir "$probe_dir" \
  "$probe_dir/target/wasm32-unknown-unknown/debug/nntune_ingest.wasm"
```

Provision two non-member versions of one disposable key on the dedicated staging
provider and fixture bucket. Use `media/00000000000040008000000000000000/<generation>`
with a fresh compact lowercase UUIDv4 generation. Reserve that object ID for
qualification. Take both version IDs from the protected provisioning receipt.
For the [SeaweedFS fixture boundary](../../ops/staging-s3-boundary/README.md),
read back the managed policy and its attachment with the provisioner inside
the private backend container. Independently hash the Caddy configuration
mounted in the boundary container and compare it with the reviewed render for
that access key ID and exact key. Record both readbacks for the second
reviewer. The effective policy must allow version-specific HEAD and DELETE
on this single key and deny listing, bulk delete, unversioned access and
other keys. The launcher sends DELETE for the selected version only.
Run the disposable boundary qualification before the protected provider
dispatch. Keep provisioning and its credentials inside the private backend.
After recording the proof, withdraw the credential and retire the sibling using
the provisioning identity and its exact version pin. Verify the trial cleanup
credential's two-key policy separately.

In the protected environment, load `PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT` and
`PRIVATE_PLAYBACK_STAGING_S3_REGION` from trusted staging settings. Pass
`PRIVATE_TRIAL_PREFLIGHT_SCOPE` as the protected fixture JSON with `bucket`,
`endpoint`, `region`, `generation`, `version` and `sibling_version`. Pass the
disposable `access_key` and `secret_key` in `PRIVATE_TRIAL_PREFLIGHT_S3` as JSON.
Use a clean environment with normal TLS verification and shell tracing off.
Run the probe explicitly from the repository root.

```sh
node .github/scripts/private_trial_provider_preflight.cjs --provider "$probe_dir/nntune_ingest.js"
```

Record the reviewed commit, UTC run time, redacted policy and fixture custody
references, and the fixed verdict line in the protected trial record. A
`qualified` verdict records matching present HEADs, both delete and absence
pairs, and a matching sibling HEAD after the second pair. The probe uses a
fresh HTTPS connection for each HTTP/1.1 request and reads to connection close
to verify that no payload follows the headers. A `refused`, `unconfirmed`,
interrupted or missing verdict blocks arming. A failed attempt can already have
deleted its version. Reconcile the protected fixture before probing a fresh
disposable key and version pair.
Keep origins, keys, version IDs, credentials, raw requests and replies out of
shared evidence. Hosted synthetic fixture results prove client behavior only.
Record actual provider qualification separately before arming the trial.

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
retrying. A grant retry requires the same reviewed overlay bytes and role OIDs.
An identical install reuses the existing overlay. A conflicting receipt or
replacement role goes to the schema owner before reapply. If a deploy or trial
fails, verify the disabled state and reconcile the upload state, publication and
storage pins before starting a fresh invite and attempt. Treat an unknown upload
result as unconfirmed and use only the reviewed replay path for the same attempt.
If a partial upload has no matching completed publication, stop and hand it to
77c for reconciliation through the reviewed upload path. The selected-fixture
retirement path requires a matching completed publication.
