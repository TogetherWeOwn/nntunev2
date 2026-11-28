# First private staging trial

Use the protected `staging` Environment, `nntunev2_staging`, and the dedicated
versioned staging fixture bucket. Keep the upload, playback and journey arms
unset until step 5. Use the original synthetic FLACs and synthetic accounts.
Record the reviewed commit, run and job links, serving build and version IDs,
fixture receipt references, and pass or stop verdicts. Keep connection details,
keys, passwords, cookies, invite codes and cohort values in secure custody.

## Setup and trial

If dedicated PostgreSQL provisioning stops partway through, use the
[Stage C recovery handoff](private-staging-trial-recovery.md) before retrying.

1. **Prepare the isolated database.** For the dedicated PostgreSQL target,
   verify the [storage handoff](#storage-closeout-before-stage-c1) before C1
   provisioning or database writes. Run the [catalog preflight](#isolated-database-catalog-preflight)
   before preparing ACLs or issuing either login. If the isolated branch has
   ambient database grants, qualify the [database ACL preparation](#isolated-database-acl-preparation)
   before issuance. Issue the two dedicated SQL logins through the branch's
   authorized issuer. Use the [role names and membership
   rules](../ops.md#private-trial-schema) from the existing lifecycle. The owner
   needs authority to grant CONNECT, fence, drain and retire the pair. Before
   dispatch, confirm the executor owns the five lock tables and can install
   the overlay and grants. The `public` schema owner must authorize `CREATE`
   and grant `USAGE WITH GRANT OPTION` to the executor. The executor needs
   grantable column access, `REFERENCES` for the overlay's foreign keys, and
   grantable `EXECUTE` on both reviewed 0052 routines and `EXECUTE` on
   `storage_object_intent_append_only()` to install the locator trigger. Confirm
   both routed logins independently lack CONNECT to every other connectable
   database, including templates. Retain the original protected scope for
   withdrawal.
   For the dedicated PostgreSQL trial with Cloudflare resources, see the
   [Stage C1 temporary API token example](stage-c1-temporary-api-token.md).
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
   version absence using separate, scoped cleanup access. Record the assigned
   cleanup owner and authority evidence in the
   [sign-off check](#check-cleanup-sign-off). Record a separate reuse or
   retirement disposition for the prepublished companion and its trial
   admission. Stop if any binding, identity or cleanup path is missing.
   Before arming, confirm a reviewed drill entry point can use an authorized
   synthetic session after companion admission and before cutoff. Confirm the
   selected staging origin can pace delivery so ciphertext remains outstanding
   after the first plaintext chunk reaches the unbuffered relay client. Stop
   if either prerequisite is missing.
   See [#1816](https://github.com/TogetherWeOwn/nntunev2/pull/1816) and the
   [staging binding instructions](../ops.md#private-stream-staging).
   For the dedicated PostgreSQL target, see the
   [Stage C2 Hyperdrive origin connection](#stage-c2-hyperdrive-origin-connection).
   If the connection IDs match but the Worker still serves a previous
   version, use the [deployment readback rehearsal](#rehearse-a-stale-trial-deployment).
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

### Read ordinary staging progress for a pinned candidate

For a disarmed candidate, match the full recorded SHA, run ID and attempt to the
automatic `main` push of `deploy.yml` at the qualification callback. For an
armed run, use the [private journey proof](../first-private-music-launch.md#qualify-the-candidate).
Read `GET /repos/TogetherWeOwn/nntunev2/actions/runs/<run-id>/attempts/<attempt>`
and the same attempt's `/jobs?per_page=100&page=1` listing. Include every page
and confirm `total_count` before treating an empty listing as evidence. Take job
IDs from that listing and match the SHA and run ID on each job read. This handoff
uses GET requests only and retains the selected candidate.

Use these synthetic snapshots of one attempt to rehearse the callback.

| Saved evidence | What to inspect on the next callback |
| --- | --- |
| The run is `pending` with a null conclusion and a complete zero-job listing. | Read the same attempt's jobs listing for `gate`. For a terminal zero-job failure, REST may return no check runs or annotations to explain it. Before recording a queue rejection, open that attempt's run summary at `https://github.com/TogetherWeOwn/nntunev2/actions/runs/<run-id>/attempts/<attempt>` and confirm the `Queue is full` message for `deploy-staging-main`. Without it, report the failure as unclassified. Use the pinned `<attempt>` in every run and jobs route. |
| The job named `gate` is `queued` with no runner assigned (`runner_id` null or `0`, `runner_name` null or empty) and no steps. | Read `GET /repos/TogetherWeOwn/nntunev2/actions/jobs/<job-id>` for `gate` and check for runner assignment or execution. Actions can populate `started_at` while the job is still queued. |
| `Deploy to staging` and `Deploy api to staging` succeeded, and the full journey is queued, running or lacks a successful result. | Read the attempt's `post-deploy-e2e / synthetic smoke (health)` job for `Run synthetic smoke`, then its `post-deploy-e2e / full journey (…)` job for `Run full journey`, the evidence gate and the receipt record and upload steps. A failed smoke with a skipped journey is a smoke failure. |

In all three snapshots, record qualification as pending. Report the pinned
attempt, last confirmed step and next read-only endpoint in the handoff.
A job row, successful deploy or passing smoke only establishes progress.
Qualification still requires the same attempt's complete ordinary journey,
its [attempt-bound receipt](../../ci/staging-admission.md), and the existing
[staging build identity and health checks](../ops.md#staging-promotion-checklist).
Confirm writer clearance through the existing trial handoff before downstream
use. An older writer's progress belongs to its own attempt.

### Rehearse the verifier handoff offline

The hosted [`e2e-lint` job](../../.github/workflows/ci.yml) runs the offline
test **private staging reporter and receipt wiring with disposable evidence**.
For a [focused reproduction of that test's failure](../../CONTRIBUTING.md#focused-reproduction-only),
run from the repository root with the pinned `e2e` dependencies installed.
The hosted job uses Node 26, Python 3.13 and `jq` 1.8.2. The fixture finds
Bash, Git, `python3` and `jq` on `PATH` in a clean environment. Check that
`env -i PATH="$PATH" python3 --version` and
`env -i PATH="$PATH" jq --version` work. If Python needs a private library
path, use the launcher from the job's **Preserve Python runtime for isolated
helper subprocesses** step.

```sh
python3 -B -I e2e/fixtures/private-music/test_admission_wiring.py "$(command -v node)"
```

The disposable runner checks the original tone A FLAC against its manifest,
then passes the synthetic publication, Library row and play receipt through
the staging journey's observation helper. It checks that
`private-publication`, `private-library` and `private-play` carry the same
track ID, digest, build, run and attempt in `admission.xml`. The existing
receipt step must carry those fields into `private_track` in
`staging-admission.json`. A play receipt for the companion track must fail
with no report. Use the result as an offline handoff check. Live qualification
and admission still require the protected run's evidence.

### Rehearse a stale trial deployment

Keep the trial disarmed. Match each saved
[scoped connection readback](#stage-c2-hyperdrive-origin-connection)
to the protected C1 database setup record. Use the deployment readback for
the Worker on the reviewed trial route to check which version receives
traffic. Rehearse both comparisons offline with these synthetic references.

| Binding | Setup config ID | Saved scoped readback |
| --- | --- | --- |
| `UPLOAD` | `hd-upload` | `hd-upload` |
| `AUTH` | `hd-auth` | `hd-auth` |
| `PUBLISH` | `hd-publish` | `hd-publish` |
| `ENROLL` | `hd-enroll` | `hd-enroll` |

Use the same route, reviewed commit, deploy record and four matches for both
cases. The deploy record names candidate version `version-b`. Evaluate the
current traffic allocation and version using the comparison in the
[staging API deployment identity readback](../../.github/scripts/staging_api_identity.py).

| Case | Current deployment at 100% | Deployment readback |
| --- | --- | --- |
| Stale | `deployment-a` serves the previous `version-a` | Pending. The comparison returns `deployed-version-mismatch`. |
| Matching control | `deployment-b` serves `version-b` | Matched when the version reports the reviewed build SHA and the closing deployment read still returns `deployment-b`. |

For the stale case, mark C2 serving evidence pending. In the protected C2
handoff, cite the deploy record for the expected version and the failed
probe's run and check for the mismatch reason. Record observed deployment
and version IDs only from a saved protected allocation readback tied to that
exact probe observation on the same Worker and route. The unavailable
receipt carries only the mismatch reason. Leave the observed IDs unconfirmed
if the tied readback is missing. Any later history read is fresh evidence for
reconciliation.

The trial operator hands the mismatch to Engineering in **Setup step 4,
Verify isolated serving and cleanup access**. Engineering reconciles the
active route and version with the deploy record through the separately
reviewed isolated serving lane and returns a fresh readback for that route.
Retain the original setup record and its four IDs. Keep the trial disarmed
and serving evidence pending while that lane or readback is missing.

The control clears the deployment identity check. The ordinary disarmed API
deploy omits the four trial bindings. After a version change, C2 needs fresh
binding readbacks and protected read-only results through all four bindings
on the exact reviewed trial version. The existing origin, TLS and cache checks
still apply.
[Binding renderer](../../.github/scripts/staging_stream_config.py)
· [Stage C2 Hyperdrive origin connection](#stage-c2-hyperdrive-origin-connection)

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
   Apply the [sign-off check](#check-cleanup-sign-off) before closing the record.
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
   logins stay fenced. Rehearse conflicting evidence in the
   [retirement readback tabletop](#tabletop-for-conflicting-role-retirement-readbacks).

### Check cleanup sign-off

Use this offline exercise for one disposable synthetic trial. The placeholders
refer to evidence in the same protected trial record.

| Evidence | What to check |
| --- | --- |
| `<cleanup-owner-ref>` | Before arming, identify the assigned cleanup owner and verify the separate cleanup authority for this trial. Match the direct database host, port, role and role OID and its reviewed grants to the isolated target. Match the cleanup credential to the two fixture keys and the effective policy readback for the reviewed storage route. At sign-off, verify from the current protected record that the assigned owner is authorized for this same trial scope. |
| `<fixture-receipt-ref>` and `<scope-ref>` | Link the passing `post-deploy private music journey` job to its recorded run, attempt and build. Match the completed tone A publication and storage receipt to the fixture and provider pins in `PRIVATE_TRIAL_CLEANUP_SCOPE`, including the object, generation, `media_version` and `page_version`. Check that the scope carries the verified cleanup identity pins. |
| `<cleanup-run-ref>` | Link the successful `Retire the selected private staging fixture` job and its `Retire the selected tone A versions once` step in attempt 1 of the recorded [protected cleanup dispatch](#protected-cleanup-dispatch). Require `Private staging fixture cleanup: confirmed` and match that run's `private_trial_cleanup_scope_sha256` input to the SHA-256 of the exact retained UTF-8 scope bytes. |

For the [SeaweedFS route](../../ops/staging-s3-boundary/README.md#render-the-credentials-policy),
verify the managed policy and attachment and compare the mounted Caddyfile
digest with the reviewed render for the cleanup credential. The [provider-only
preflight](#provider-only-preflight) uses a separate disposable credential and
supplies qualification evidence. Sign-off requires the cleanup dispatch for
the completed fixture above.

Mark this fixture's cleanup **Confirmed** only when all three checks pass. In
the exercise, remove the owner assignment after arming, reassign it without
verified authorization for the new owner, or remove the exact cleanup run
receipt. Each leaves sign-off **Pending**. A `confirmed` line from a dispatch
with a different scope digest, or a scope whose media or page version differs
from the original receipt, also leaves it **Pending**.
If setup evidence is missing before arming, keep the trial disarmed. At
closeout, retain the original pins and use the existing reconciliation path
before clearing the cleanup settings or withdrawing cleanup access.

#### Carry forward an already-cleared disposable fixture

In the retirement report for an already selected ordinary `main` push, the
`Retire the selected private staging fixture` job normally skips. The
[workflow](../../.github/workflows/deploy.yml) runs it only on the first
protected `main` dispatch with a cleanup scope input. A push that aborts before
deployment can show the same skip. Use the selected attempt's `staging-web`
and `staging-api` job records and require success of the actual deploy steps
`Deploy to staging` and `Deploy api to staging`. A green job whose deploy step
skipped does not establish deployment. Require the `post-deploy private music
journey` job to be skipped in that attempt so an armed journey cannot use this
report for a new publication.

For a disposable fixture whose original sign-off passed, reuse the read-only
Actions run, job and verdict readback above. Match the original publication
and both exact version pins to the retained scope bytes in protected custody.
Keep the original sign-off filed under the successful retirement dispatch.
The offline retirement report uses only the digest of the original scope, the
separately recorded dispatch input and the fixed verdict line. Keep scope
bytes, credentials and full logs out of the saved report input.

```sh
python3 -I ci/staging_retirement_readback.py "$candidate_sha" "$candidate_run_id" \
  "$candidate_attempt" "$approved_scope_sha256" < retirement-readback.json
```

The input's `actions` object holds the authenticated GitHub REST response bodies
from `repos/TogetherWeOwn/nntunev2/actions/`. Use keys
`runs/<id>/attempts/<attempt>` and `runs/<id>/attempts/<attempt>/jobs` for the
selected push and the original dispatch's attempt `1`. Save each complete jobs
page using `per_page=100&page=1`. Use the attempt-specific routes even when a
job row omits `run_attempt`. Check the workflow revision at each recorded SHA.
The `retirement` object holds `run_id`, `sha`, `job_id`, `scope_sha256` and
`verdict` from the verified original retirement record. Use the recorded
dispatch input for `scope_sha256` and the exact fixed line from that job's
retirement step for `verdict`. Take `approved_scope_sha256` independently from
the original approved scope bytes. GitHub's run and jobs responses alone do
not supply those custody records.

**Already cleared** records the original scope only. Both deploy steps must
succeed in the selected attempt, both the retirement and private music jobs
must skip, and the prior confirmed cleanup must finish before deployment.
Keep the current retirement job recorded as **Skipped**. Record **Pending**
for an aborted deployment, a journey that ran, or missing or mismatched scope,
job or step evidence. Preserve any verified historical sign-off. The report
checks saved evidence and grants no staging or release clearance.

### Isolated database catalog preflight

The observed isolated app catalog contains provider-managed HypoPG 1.4.3 in
`pscale_extensions`. The approved set is exactly PL/pgSQL 1.0 in `pg_catalog`.
The absence of `pg_trgm` is expected here. Migration `0001_mb_mirror.sql`
installs it on the separate MusicBrainz mirror.

Keep setup and the trial arms off while no eligible target is documented.
The next step is to establish from provider documentation or a supported
provider readback whether the isolated staging branch can expose an app
catalog with the approved extension set and supported management access.
Record the blocker if this route is unavailable. Keep the provider-managed
extension and ACLs intact.

For a candidate, independently confirm the ready, nonproduction branch ID
and its direct and routed endpoint pins against provider metadata in private
custody. Confirm the database is distinct from shared staging. From the
reviewed checkout, load `ISOLATED_TRIAL_TARGET` using the shape in step 2,
with planned reader and writer logins, and load just the candidate's owner and
executor URLs as `PRIVATE_TRIAL_OWNER_URL` and `PRIVATE_TRIAL_EXECUTOR_URL` from
secure custody. Use a private operator host with Python 3.11 or later, `psql`
on `PATH`, and a readable system CA bundle at
`/etc/ssl/certs/ca-certificates.crt`. Run the opt-in readback from that session:

```sh
python3 -E -s .github/scripts/staging_trial_role_lifecycle.py check-catalog --isolated
```

The command uses verified TLS and checks the direct owner and executor
sessions against the pinned database OID and identities. It checks the 0063
ledger checksum and exact extension set through the executor. It requires no
reader or writer credentials, role scope, or ambient plan. On an extension
refusal, stop and return to candidate selection above. On an identity or
baseline refusal, reconcile the reviewed pins and migration readback before
retrying. Retain only fixed verdicts and a private reference to the pins.

After a pass, qualify the same target's provider management and consumer
continuity under [ACL preparation](#isolated-database-acl-preparation). For
that operation, confirm separately that the isolated owner can read the 0063
checksum from `public.schema_migrations`. The catalog preflight reads it as
the executor. The protected ambient workflow pins both the provider branch
and database OID.
If the candidate differs from those pins, obtain a separately reviewed
protected target update before dispatch. Keep the trial off until the
protected checks, separate routed reader and writer readbacks, and serving
and cleanup checks in steps 2–4 pass on that same target.

### Isolated database ACL preparation

Before issuing either trial login, collect the normalized `CATALOG` projection
from `.github/scripts/staging_trial_ambient_acl.py` and the original ACL nullness
on the pinned isolated branch through a private read-only session. Inventory
every connectable database and template. Check provider management and each
existing consumer's required CONNECT, TEMP and CREATE, including dormant
consumers. Establish their approved access and a reversible window before any
write. A live session inventory alone cannot establish those dependencies.

Store the private plan as `ISOLATED_TRIAL_AMBIENT_PLAN` and retain it for undo.
Use fields `branch_id`, `direct`, `port` (5432), `baseline` (the catalog projection)
and `consumers` (lists of database OID, role OID, role name and privilege).
Review the exact plan digest and rollback, then pass its SHA-256 in
`ambient_plan_sha256`. Use the isolated owner URL and `ISOLATED_TRIAL_BRANCH_READ`
with a scoped PlanetScale `id:token` to confirm the distinct nonproduction branch
at dispatch. Keep the private plan, snapshots and access values out of logs.

With roles absent, run `check-ambient`, then `restrict-ambient` from protected
`main`. Refuse unsupported grantors, template changes with unverified support,
unproven dependencies, unexpected application ACLs or unexplained drift. The
workflow transfers only reviewed PUBLIC privileges to consumers that need them
and withdraws PUBLIC access atomically. It preserves non-target PUBLIC TEMP
on databases that already deny PUBLIC CONNECT. The target still denies
PUBLIC CREATE, CONNECT and TEMP. If a template has a different owner, require
SET authority for its recorded grantor in both directions and preserve that
grantor on restoration. Verify management connectivity and
the isolated catalog before issuing any login. After retiring both roles, use
the same plan for `restore-ambient`. The inverse restores normalized grants,
including grantors and grant options. Supported PostgreSQL DDL may leave an
explicit ACL where `datacl` was previously NULL. Confirm normalized equality
and record any raw representation difference privately.

### Tabletop for conflicting role retirement readbacks

Use synthetic evidence for one selected isolated database. In both cases the
initial role inventory matches the original target and scope. It lists
`nntune_trial_reader` at OID `71001` and `nntune_trial_writer` at OID `71002`.
The bindings are gone, and the primary `retire` report for that scope shows
`role retirement passed`.

Compare that report with a final `pg_roles` inventory from a separate read-only
session on the same pinned database. Check both names and both OIDs, including
roles with `NOLOGIN`.

| Independent final inventory | Expected role withdrawal verdict |
| --- | --- |
| The reader is absent. `nntune_trial_writer` at OID `71002` is still present with `NOLOGIN`. | **Pending.** Record the discrepancy against the original scope. Keep the trial disarmed and reconcile with the grant owner before closing withdrawal. |
| Neither pinned role name nor either pinned OID appears. | **Verified.** The inventory agrees with the primary report and satisfies the role absence check. |

Walk both cases using the recorded evidence. Keep the initial inventory and
primary report identical when switching to the clean control.

### Protected cleanup dispatch

After containment, supply `PRIVATE_TRIAL_CLEANUP_CONTAINMENT` in the protected
`staging` Environment. Use the verified disable run's `build_sha` and its
`api` and `stream` pins, each with `deployment_id` and `version_id`. Set
`session_revoked`, `cohort_cleared` and `armed_runs_drained` to `true` only
after completing and checking those steps. Leave the upload, playback,
journey, observation and Stream deploy arms unset. Keep the original staging
storage settings available for the runner to match against the receipt.
In the containment receipt, record `cleanup_scope_sha256` from the approved
fixture scope and `cleanup_s3_access_key_sha256` from the independently
verified cleanup access-key ID. Use lowercase SHA-256 hex for both, hashing
the exact UTF-8 bytes of the scope JSON and of the access-key ID respectively.
If a later staging deploy changes either serving pin, obtain fresh disable
receipts and carry forward both original cleanup pins.

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
scope. Derive the containment pin from the verified two-key cleanup policy
readback. The disposable provider preflight has its own access-key ID. Before
arming, verify both identities and qualify the provider with the same no-list
policy on a disposable versioned fixture. Require version-specific
HEAD to return the selected version while present. Require exact-version
DELETE to return an empty HTTP 204 and, if it includes `x-amz-version-id`,
require that value to match the selected version. Require the subsequent
version-specific HEAD to return an empty HTTP 404. Repeat the same DELETE and
HEAD against the now-absent version and require the same status, body and
optional version header checks. Treat HTTP 403, ambiguity or a policy that
needs list access as a stop. Keep the scope and credentials in protected custody.

#### Tabletop for a refused shared ingress reload

In this offline example, the trial is disarmed and hosted qualification has
yet to be dispatched. Engineering is undoing the fixture ingress in the
protected rollout's shared-writer window. The exact fixture DNS record has
been removed and its absence recorded. The shared Caddy server is serving
synthetic revision A, which includes the fixture route and an unrelated route
update. Revision B on disk removes only the fixture route.

1. The process launcher rejects the reload before the Caddy reload process
   starts. Record the refusal and the completed DNS step separately.
2. Engineering restores and verifies revision A on disk from the copy taken
   before writing revision B. An independent readback from the running server
   confirms revision A is still serving both routes. Record the disk and
   serving results separately. The fixture route is still active, so ingress
   rollback remains pending.
3. If that live readback is unavailable, inconclusive or mismatched, record
   what was observed and leave recovery pending even if the disk copy matches
   revision A.

Keep the trial disarmed and hosted qualification pending. Engineering owns
diagnosis and any retry through the existing protected rollout, including
reconciling DNS and serving state before retrying.

#### Provider-only preflight

For a hosted run, use [staging-trial-provider-preflight](../../.github/workflows/staging-trial-provider-preflight.yml).
For the fixture boundary, provision and verify a reachable HTTPS ingress
forwarding to its loopback listener through the protected rollout. The
disposable qualification uses a runner-local endpoint. See the [offline
tabletop](#tabletop-for-a-refused-shared-ingress-reload) for a refused shared
ingress reload during rollback.
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
the trial receipt. Require the pinned `nntunev2-private-fixtures-staging`
bucket. Stop on a different bucket and resolve the storage configuration
before attempting qualification. If restoring settings changes the endpoint
or region, repeat the exact-version HEAD and DELETE qualification on fresh
disposable versions in the pinned bucket at that target. Qualify its two-key
cleanup scope separately. Keep the trial disarmed until serving and cleanup
both qualify. The fixture boundary serves HEAD and DELETE only.
Keep the exact scope JSON and the disposable credential in
protected custody. Use the SHA-256 of the exact UTF-8 bytes supplied as the
scope secret. The dispatch inputs contain only the reviewed full `main` commit
SHA and the lowercase scope digest.

```sh
gh workflow run staging-trial-provider-preflight.yml --ref main \
  -f ref="$reviewed_sha" -f scope_sha256="$scope_sha256"
```

GitHub must resolve the dispatch to that reviewed commit. Open the run and
record its URL and the fixed verdict from the successful probe step. The job
summary reports an incomplete attempt when the probe did not succeed and the
runner can still publish the summary. A skipped job or a rerun supplies no
qualification evidence. Reconcile any interrupted or failed attempt before
preparing a fresh pair and dispatch. Withdraw the disposable credential and
clear the run's Environment secrets after use.

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
scope JSON recorded in the containment receipt. The job checks approval and
the disable deployments under the staging deploy lock. The isolated Worker
checks the exact scope bytes and both credential pins before connecting to
the cleanup database. `confirmed` means two verified version absences and a
committed source finalization. Stop on `refused`,
`withheld`, `unconfirmed` or missing evidence. Reconcile SQL and provider state
before a fresh dispatch with the original approved scope and cleanup key.
Keep those pins while the [sign-off check](#check-cleanup-sign-off) is pending.
Clear the cleanup settings in custody and withdraw the scoped cleanup access
after sign-off is **Confirmed**.

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

### Offline exercise for a stopped journey

Use the placeholder receipts below at a tabletop exercise. Tone A was written,
then `Qualify private listening` stopped before the normal handoff. Use the
recorded Actions links and the protected fixture record to prepare the cleanup
handoff on paper.

| Receipt | Placeholder record |
| --- | --- |
| Trial record | `<trial-run-url>`, attempt `1`, `<trial-build-sha>`, `<journey-job-url>` |
| Actions metadata | The recorded `deploy.yml` run is a `main` push on `<trial-build-sha>`. In attempt `1`, `Require this run's API and Stream deployment` passed and `post-deploy private music journey` was cancelled in `Qualify private listening`. The same run also lists a rerun as attempt `2`. |
| Protected tone A record | The selected upload session and job match a completed publication and its canonical track and object. The original media and digest-page version pins match `<tone-a-scope-ref>`, with approved hash `<tone-a-scope-sha256>`. |
| Containment record | `<disable-run-url>` has the verified disabled API and Stream pins. The session is revoked, the cohort cleared and armed runs drained. The cutoff, retention and lease windows have elapsed. |

1. Match the recorded run, attempt, job and commit against the Actions
   metadata. Stop if they disagree. Keep the stopped verdict on attempt `1`
   even though attempt `2` shares its run ID and commit. `Record candidate
   admission receipt` follows successful qualification and did not run for
   attempt `1`. Use the protected fixture record to establish cleanup scope.
2. Match the tone A session, job, completed publication and both version
   pins to the original protected scope. If that match is ambiguous or the
   publication is incomplete, stop at reconciliation. Keep the companion
   fixture under its separate disposition.
3. On paper, give the cleanup operator the stopped trial run and job links,
   the original scope reference and hash, and the verified disable run and
   containment reference. The operator checks the separately authorized access
   and pins in the [protected cleanup instructions](#protected-cleanup-dispatch).
   The expected next action is a fresh protected `deploy.yml` dispatch with
   `private_trial_cleanup_scope_sha256` for the job `Retire the selected private
   staging fixture`. Record the planned cleanup separately from the stopped
   journey. End the exercise before dispatch.

## Storage closeout before Stage C1

The protected storage-only rollout owner provides the closeout receipt. The
database preparation operator reads it before any C1 provisioning or database
write. Compare it with the approved storage attempt and record the receipt
reference and the outcome in the protected C1 record. Keep the trial disarmed
and the database lane read-only until all three checks pass.

1. **Identify the run.** Match the storage rollout attempt, reviewed merge and
   dispatch commits, and the disposable scope SHA-256. Match the protected
   main `staging-trial-provider-preflight` run ID, attempt `1`, job ID and
   tested SHA to that dispatch. Require the job's executed storage probe and
   its fixed `qualified` verdict. A local disposable pass or a skipped probe
   leaves storage qualification pending. Refer any stop to the storage owner
   for reconciliation before accepting the handoff.
2. **Verify exact absence.** After the probe and before disposal, require
   independent provider reads for the selected version's empty `404` and the
   sibling's matching ETag. At closeout require both pinned versions absent by
   exact-version HEAD and an empty scoped inventory of versions, delete markers
   and multipart uploads. Match the storage owner's completed DNS closeout to
   the exact attempt and require the recorded Caddy, staging Environment and
   scoped resource disposal readbacks. A delete acknowledgement or a successful
   Compose `down` alone leaves closeout pending.
3. **Verify writer release.** Require the timestamped release and successful
   free readback on the original shared writer lock after verified closeout.
   A line saying the launcher will release, a retained writer, an unavailable
   readback or another holder at readback leaves the handoff pending. Use the
   existing writer coordination before C1 writes. A prior release does not
   establish that the window is available now.

### Offline storage handoff walkthrough

Use the following synthetic readbacks as one receipt. The labels stand in for
protected references and full digests. No workflow or provider calls are
needed to walk through the C1 decision.

| Check | Synthetic evidence |
| --- | --- |
| Run | Approved attempt `storage-02` pins merge `M2`, dispatch `D2` and scope digest `S2`. Its protected run `8202`, attempt `1`, job `9202`, tested `D2` with the `S2` dispatch. The probe recorded `qualified` at `10:00 UTC`. |
| Independent verification | At `10:01 UTC`, admin HEAD for scope `S2` reads selected `v2-selected` as empty `404` and the sibling `v2-sibling` as `200` with its original ETag. By `10:03 UTC`, exact HEAD confirms both absent and inventory shows zero versions, delete markers and uploads. At `10:03 UTC`, the storage owner records completed closeout for `storage-02` with passing DNS, Caddy, Environment and scoped resource disposal readbacks. |
| Writer | At `10:04 UTC` the storage owner records release and a free readback on the same shared writer lock. |

The database operator accepts the storage prerequisite for `storage-02` at
`10:05 UTC`. Independently change job `9202` to a job from attempt `storage-01`,
remove the exact HEAD read for `v2-sibling` while leaving its delete success,
or replace the free readback with a retained writer. Each change leaves C1
pending. So does moving the release to `10:02 UTC`, before absence was verified,
or recording that another holder took the lock before readback. A stop before
dispatch also leaves C1 pending even if rollback verifies absence and writer
release. The storage owner resolves missing storage or DNS evidence. The
database operator still needs the separate database admission, rehearsal and
connection evidence for their respective stages.

## Stage C2 Hyperdrive origin connection

**Setup pending.** The dedicated PostgreSQL trial uses four distinct Hyperdrive
configs, one each for `UPLOAD`, `AUTH`, `PUBLISH` and `ENROLL`. Each config uses
the separate PostgreSQL login role assigned by its reviewed caller and scoped
to the trial database. All four use the same dedicated Tunnel TCP route and
serving Access service token for transport access. Each requires origin TLS
`verify-full` with the same dedicated origin CA, uploaded once and referenced by
all four configs, and query caching disabled.
[Binding validation](../../.github/scripts/staging_stream_config.py)

In C0, the read-only account preflight, confirm a complete count of **21
existing Hyperdrive configs or fewer**. That leaves room for all four within
the Workers Paid limit of 25 configs. At 22 or more, stop before provisioning.
An incomplete count leaves C0 pending. Also confirm Tunnel and Access capacity
against the Zero Trust plan and that one eligible account-scoped credential
covers Tunnel, Access and Hyperdrive writes. Cloudflare documents the serving
features individually. Their use together on this origin still needs hosted
evidence.
[Hyperdrive config list](https://developers.cloudflare.com/api/resources/hyperdrive/subresources/configs/methods/list/)
· [Tunnel and Access](https://developers.cloudflare.com/hyperdrive/configuration/connect-to-private-database/)
· [Origin TLS](https://developers.cloudflare.com/hyperdrive/configuration/tls-ssl-certificates-for-hyperdrive/)
· [Cache settings](https://developers.cloudflare.com/hyperdrive/concepts/query-caching/#disable-caching)

For the dedicated stock PostgreSQL route, use the
[stock PostgreSQL preflight](stock-postgresql-trial-preflight.md) for the Access
and Tunnel capacity readback and the separate operator connection.

The [offline C0 reporter](../../scripts/staging_trial_c0_readback.py) reads a
redacted JSON summary on stdin. Use the fields in the
[synthetic summary](../../scripts/fixtures/staging_trial_c0/synthetic.json).
Use `null` for an incomplete inventory, unknown limit or unconfirmed scope or TLS
support. The report gives each observation its own verdict. Exit 0 means the
report was printed, including any pending rows.

To rehearse Hyperdrive pagination offline, pass `--hyperdrive-pages <synthetic-json>`
and the same summary on stdin. The file holds a JSON array of synthetic list
response bodies in page order. Keep `success`, `errors`, `result_info` and the
`id` field of each `result` entry. This mode derives the count from those pages
and leaves it pending when the continuation fails or is unavailable. Use
synthetic responses only. Keep actual provider responses in protected custody.

C1 provisioning waits for completed C0, a passing disposable rehearsal from
the merged source and the [verified storage handoff](#storage-closeout-before-stage-c1).
C2 verifies the actual serving connections after authorized provisioning.
Collect both readbacks independently for each of the four bindings, using its
own `<hyperdrive-config-id>`. Keep each config unchanged across its readback
and Worker query.

1. From the [Hyperdrive config GET](https://developers.cloudflare.com/api/resources/hyperdrive/subresources/configs/methods/get/),
   match the returned ID, `origin.host` and `origin.access_client_id` to the
   reviewed config, Tunnel hostname and dedicated serving Access client.
   Require `mtls.sslmode=verify-full` and match `mtls.ca_certificate_id` to the
   protected upload record for the dedicated origin CA and its pinned
   fingerprint. The Access origin variant uses the Tunnel's configured TCP
   destination port. In the same protected context, run
   [`workers/api/check_hyperdrive_cache.sh`](../../workers/api/check_hyperdrive_cache.sh)
   with `--config-id <hyperdrive-config-id>` and require an executed pass for
   `caching.disabled=true`. Leave `--allow-skip` off. This script checks cache
   metadata only. Keep raw provider responses and script output in protected
   custody.
2. Record the reviewed commit, protected run and exact check reference, and
   serving Worker route and version. Read back that version's binding to the
   same config ID. Reuse a protected Worker connection check when its result
   shows a successful read-only query through that binding. Tie the origin
   connection to the settings above with evidence of its establishment under
   those settings or the protected C1 record showing the config has used them
   since creation. Query caching is disabled while origin pooling remains
   active, including across new Worker-side connections.

Dispatch `deploy.yml` on protected main with `private_trial_identity_mode=upload`
for the first config or `all` for Stage C2. Supply the protected `CF_ACCESS_CLIENT_ID`
and `CF_ACCESS_CLIENT_SECRET`. If readback fails, correct the cause and dispatch
again, or restore the recorded pre-probe disarmed push using the [staging rollback procedure](../ops.md#staging-deploy-lock-cutover-and-rollback).

Record one independently verified result per binding in the protected C2
handoff. Include evidence references and match or pass outcomes. Keep
credentials, connection strings and raw errors out of the record. A missing
config field, binding readback or qualifying Worker result on any of the four
paths leaves setup pending for the whole lane. Keep the trial disarmed. If the
required TLS combination is confirmed unavailable or a cache check fails,
stop and report the affected binding to Engineering. Retain the `verify-full`
requirement. Use these serving results in the C2 comparison of operator and
serving database identities.

### Assemble the four-connection handoff

Record the reviewed revision and the Stage B disposable run and job that
passed the stock PostgreSQL replay, identity and role-isolation checks. Use
that proof as the rehearsal record. For the hosted handoff, take the database
OID, cluster and TLS pins from the independently admitted trial target.
Disposable database and role OIDs apply only to the disposable instance. Match
the hosted target through the separate operator readback before comparing the
four Worker results. [Stock target qualification](../../ops/staging-trial-pg/README.md)
· [Operator preflight](https://github.com/TogetherWeOwn/nntunev2/pull/1942)

Start with the four original, binding-labelled receipts before indexing them
by name. Require one receipt per binding, four distinct config IDs, and four
distinct canonical SQL role names and OIDs. Match each ID to the expected
`<binding>_STAGING_ID` and the binding read back from the same reviewed Worker
version. Match each approved role name to `<binding>_ROLE` and take its OID
from the independently reviewed role pins. Keep approval pins and observed
values separate even when they agree. Record evidence references and verdicts
in the protected handoff. Keep raw results and credentials in custody.

On each receipt, match the config's account, origin host, selected database,
serving Access client and origin CA to the reviewed trial scope. Require
`verify-full` origin TLS established under those settings and an explicit
`caching.disabled=true` pass for that config. Require exactly one read-only
identity row from the same binding. Check these against the same attempt and
unchanged config. A missing result, repeated label or config, wrong Worker
version, mismatched identity, or missing TLS or cache proof leaves the handoff
pending. Account capacity and recovery have their own procedures.
[Origin and cache checks](#stage-c2-hyperdrive-origin-connection)
· [Interrupted setup recovery](https://github.com/TogetherWeOwn/nntunev2/pull/2043)

In this synthetic example, the selected `nntunev2_staging` database has OID
`40200`. All four receipts refer to the same reviewed build, Worker version,
trial target, dedicated serving route and origin CA. Each config reference
stands for a different scoped ID. The TLS and cache passes refer to separate
evidence for each config. Values in the observed column come from the Worker
result, with columns in query order.

| Binding | Config ref | Approved role name and OID | Observed identity row | Origin TLS | `caching.disabled` |
| --- | --- | --- | --- | --- | --- |
| `PRIVATE_TRIAL_UPLOAD_DB` | `cfg-upload` | `trial_upload`, `40201` | `nntunev2_staging, 40200, trial_upload, 40201, trial_upload, 40201` | `verify-full` | `true` |
| `PRIVATE_TRIAL_AUTH_DB` | `cfg-auth` | `trial_auth`, `40202` | `nntunev2_staging, 40200, trial_auth, 40202, trial_auth, 40202` | `verify-full` | `true` |
| `PRIVATE_TRIAL_PUBLISH_DB` | `cfg-publish` | `trial_publish`, `40203` | `nntunev2_staging, 40200, trial_publish, 40203, trial_publish, 40203` | `verify-full` | `true` |
| `PRIVATE_TRIAL_ENROLL_DB` | `cfg-enroll` | `trial_enroll`, `40204` | `nntunev2_staging, 40200, trial_enroll, 40204, trial_enroll, 40204` | `verify-full` | `true` |

For the offline `binding_identity_verdicts` checker, pass the admitted
`database_oid`, index the approved `(role_name, role_oid)` tuples into
`role_pins` by full binding name, and pass the original results as a list of
`(binding_name, observed_row)` pairs. Each observed row is a six-field tuple.
Keep OIDs as integers. Expect four `matched` identity verdicts for the table.
Replacing ENROLL's result with a copy of UPLOAD's leaves both bindings
`pending`. Swapping AUTH and PUBLISH's observed rows leaves both `pending`.
Even with four identity matches, an absent or false cache result or missing
origin TLS evidence keeps the handoff pending. [Offline role checker](../../.github/scripts/staging_trial_role_preflight.py)

Hosted execution requires a protected, read-only Worker probe using the same
four dedicated bindings while the trial is disarmed, and a hosted caller
that verifies the config and deployed-version receipts and supplies the
rows to the checker. Use the dispatch above while all trial arms are off.
The existing hosted role preflight accepts a database OID,
issuer OID and two pre-grant roles and uses the staging migrator connection.
The hosted provider preflight accepts an S3 fixture scope for exact-version
HEAD and DELETE. The example is input to the offline identity checker only.
Keep hosted qualification pending until the protected caller and its four
read-only results are available. [Deploy binding configuration](../../.github/scripts/staging_stream_config.py)
· [Hosted role preflight](../../.github/workflows/staging-trial-role-preflight.yml)
· [Hosted provider preflight](../../.github/workflows/staging-trial-provider-preflight.yml)
