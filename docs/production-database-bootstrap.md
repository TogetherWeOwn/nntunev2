# First-launch database catalog preflight

The first private music-only release requires a dedicated logical
PostgreSQL database. The checked-in production deployment handles the web
Worker. A separate protected database task must qualify the bootstrap and a
later protected operation must cut over the production API binding.

## Provider operation

PlanetScale Postgres documents SQL `CREATE DATABASE` for multiple logical
databases inside a cluster. The provider's project-level Create a database
API instead provisions a database product with a cluster configuration.
The documented direct PostgreSQL endpoint uses port 5432, a provider-issued
hostname and `sslmode=verify-full` with the system CA trust store.
[Postgres compatibility](https://planetscale.com/docs/postgres/postgres-compatibility) ·
[Create a database API](https://planetscale.com/docs/api/reference/create_database) ·
[Connection quickstart](https://planetscale.com/docs/postgres/connecting/quickstart)

PostgreSQL requires an effective `CREATEDB` role or a superuser. PlanetScale
documents `CREATEDB` on its default administrative role and withholds
superuser access. A future protected operation should use `TEMPLATE template0`
to start from the standard template. If creation names another owner, the
creator needs `SET ROLE` authority for that owner. `CREATE DATABASE` runs
outside a transaction block. Confirm eligibility on the actual branch and
plan first. Same-cluster placement shares compute and connection limits, and
storage, backups and transfer can affect billing. Verify capacity, current
pricing and budget before provisioning. Public docs establish the general
operation and pricing model. [Managing roles](https://planetscale.com/docs/postgres/connecting/roles) ·
[PostgreSQL CREATE DATABASE](https://www.postgresql.org/docs/current/sql-createdatabase.html) ·
[PlanetScale pricing](https://planetscale.com/docs/postgres/pricing)

## What the source check establishes

The [catalog preflight](../.github/scripts/database_bootstrap_preflight.py)
generates one bounded read-only catalog probe. It checks the expected
maintenance database, login and effective SQL role, the intended name's
absence, the creator's `LOGIN` and `CREATEDB` attributes, and the expected
`template0` catalog entry. Its result parser expects one verdict from a
successful quiet, tuples-only, unaligned `psql` run with `ON_ERROR_STOP`.
The module is imported by the disposable PostgreSQL test in CI.

An unexpected database name in the catalog, a duplicate-name error during
creation, or an ambiguous creation result stops the operation. Reconcile
identity and state through a separately authorized read-only observation
before taking another action. The absence verdict describes the catalog at
the time of the probe. The protected task must hold exclusive provisioning
custody across the observation and creation window.

## Protected follow-up

1. Pin the exact reviewed source and migration manifest, provider branch,
   maintenance and launch databases, SQL login and effective creator role,
   and approved execution placement in private evidence. Verify the
   provider-issued direct host and port with CA and hostname verification
   before sending the probe. A matching SQL database or role name alone
   cannot establish branch identity. Keep credentials and operational
   identifiers in approved private custody.
2. Qualify creation and access on a disposable target before asking the
   protected lane to act. Fence access until the database grants are in
   place. PostgreSQL supports creating with `ALLOW_CONNECTIONS false` so
   default `PUBLIC` access can be removed before allowing connections.
   Verify that sequence and the effective minimum migrator, runtime and
   read-only permissions, including ownership, inherited grants, role
   switching, default privileges and bidirectional staging isolation.
3. Obtain a separate read-only launch identity and empty-state receipt.
   Complete the verified snapshot before the first schema change and write
   down how to switch the product off. Use the existing manifest-checked
   [migration runner](../.github/scripts/apply_migration.py) and its atomic
   ledger when a protected migration lane is available. If data exists,
   follow the applicable backup, isolated restore and rollback rehearsal
   requirements. [Production releases](../CONTRIBUTING.md#production-releases)
4. At separate protected cutover, verify the exact production API version
   and Hyperdrive binding to the approved database and runtime role, with
   query caching disabled, and read back the resulting configuration.
   [Hyperdrive bindings](ops.md#hyperdrive-bindings)

## Hosted execution candidate

The [manual workflow](../.github/workflows/deploy-database-bootstrap.yml)
is off while the repository variable `DATABASE_BOOTSTRAP_CANDIDATE_SHA`
is unset. The [disposable CI job](../.github/workflows/ci.yml)
runs the [setup caller](../.github/scripts/database_bootstrap_setup.py)
on standard Ubuntu with generated authorization and an isolated PostgreSQL
server. It checks admission, TLS, roles, fixed verdicts and interrupted
grant rollback. Live reachability, managed role authority and deployment
approval require protected readback.

GitHub assigns these VMs without a region or static outbound IP guarantee.
The documented provider direct endpoint uses a public TLS path on TCP 5432
without a same-region client prerequisite. For the first-launch empty-data
path, keep GitHub's placement unpinned and allow only the privately verified
provider hostname, port and trusted CA in the client. Confirm the target's
current network policy and approved data-handling scope before dispatch.
If it requires a fixed geography, source IP or private network, use an
approved placement that meets it. Moving the job to a private repository
changes log access and billing and still leaves runner placement unpinned.
[Hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
· [Provider connections](https://planetscale.com/docs/postgres/connecting)
· [IP restrictions](https://planetscale.com/docs/postgres/connecting/ip-restrictions)

### Separate Environment and approval

GitHub supports required reviewers on this public repository. The existing
production Environment has only a branch restriction. Under separate release
admission, configure `production-db-bootstrap` **before** enabling the job.
GitHub can create a missing Environment from a workflow reference without
protection. Verify both `production-db-bootstrap` and `production-db-baseline`
using the [Environment protection readback](#environment-protection-readback)
before enabling the corresponding job. Follow the
[custodian handoff](#custodian-handoff) for the setup and baseline order.
Keep Environment administration with the custodian and check rule changes
before approval. One listed reviewer
suffices on GitHub, so require only the approved independent user or team in
each Environment. The release review and T0 code reviews are separate from
this deployment approval.
[Environment protection rules](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)
· [Configure environments](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)

1. After Environment readback and release approval, set the repository
   variable `DATABASE_BOOTSTRAP_CANDIDATE_SHA` to the exact approved `main`
   tip. The invoker dispatches that SHA with zero inputs. Preflight refuses
   if it finds any setup authorization in repository, organization or staging
   Environment scope. Remove the candidate variable after the attempt.
   Before approval, independently read the pending deployment, workflow,
   repo, ref, head SHA, run ID, first attempt, actor and Environment. Match
   the privately approved SHA and SHA-256 of the exact plan bytes. The
   Operator stages the matching run pins, plan and absolute expiry only in
   that Environment. Approve that single pending deployment and read back
   its approval history. Reject unexpected pending runs.
   [Workflow run readbacks](https://docs.github.com/en/rest/actions/workflow-runs)
2. Keep the fixed repository-wide job concurrency group, with cancellation
   of the running job disabled. GitHub can replace a pending job. Concurrency
   serializes execution, and the authorization binds the workflow and source
   SHA, the plan digest, GitHub run ID and attempt `1`. Do not renew, reuse or
   replace authorization while a run is pending or active. Quiesce competing
   provisioning paths separately. A canceled or ambiguous attempt requires
   read-only reconciliation and fresh approval for any later run.
   [GitHub concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)
3. The setup process consumes the exact bytes returned by admission.
   Keep the invoker and approver separate from SQL custody. Scope the writer
   to the setup step and the separate aggregate reader to the baseline path.
   Carry the [SQL issuer authority evidence](#sql-issuer-authority-evidence)
   into release review for each plan. Use no workflow inputs or
   public outputs for the plan, target, credentials or SQL results. Refuse
   debug mode. Check the debug controls at organization, repository and
   Environment scope before releasing secrets. Use a fixed command with
   no shell tracing, pass credentials privately, suppress raw child output
   and emit fixed verdicts. Keep step and SQL deadlines, no artifacts or
   caches, no job summaries with live values, and no retry after a write.
   Public job logs are available to any signed-in reader.
   [Secure use](https://docs.github.com/en/actions/reference/security/secure-use)
   · [Debug logging](https://docs.github.com/en/actions/how-tos/monitor-workflows/enable-debug-logging)
   · [Job logs](https://docs.github.com/en/actions/how-tos/monitor-workflows/use-workflow-run-logs)
4. The Operator records the credential expiry before approval and revokes
   each temporary role after use. Use a bootstrap TTL at most two hours and
   an independently scoped aggregate reader TTL at most 120 seconds. Read back
   that authorization and credential material were removed from the
   Environment and secure custody, that the temporary roles are gone, and
   that their server sessions are closed. A hard kill can skip runner
   cleanup. Cancel the run, verify the final job state and hosted disposal,
   revoke temporary authority and terminate surviving sessions through the
   protected custodian. Read back target existence, connection fence,
   ownership, effective grants and sibling isolation. An incomplete
   readback keeps the release closed. A later run requires fresh approval.
   [Job cancellation](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-cancellation)
   · [Provider role management](https://planetscale.com/docs/postgres/connecting/roles)

### Environment protection readback

1. In `TogetherWeOwn/nntunev2`, record the independently approved reviewer
   for each Environment as `User:ID` or `Team:ID`, using the numeric GitHub
   ID from the release evidence. In Settings, check the approved reviewer,
   **Prevent self-review**, and that **Allow administrators to bypass
   configured protection rules** is unchecked.
2. Save the successful JSON response bodies for each exact Environment in a
   private evidence directory. Replace `<env>` with `production-db-bootstrap`
   and `production-db-baseline` in the read-only endpoints and filenames
   below. Keep each branch policy response paired with its source endpoint
   to bind it to the named Environment.

   | GET endpoint under `/repos/TogetherWeOwn/nntunev2` | Filename |
   | --- | --- |
   | `/environments/<env>` | `<env>.environment.json` |
   | `/environments/<env>/deployment-branch-policies?per_page=100` | `<env>.branch-policies.json` |

   [Environment endpoint](https://docs.github.com/en/rest/deployments/environments)
   · [Deployment branch policies endpoint](https://docs.github.com/en/rest/deployments/branch-policies)

3. Run the [offline checker](../.github/scripts/database_bootstrap_environment_readback.py)
   with the approved identities and the saved files.

   ```sh
   python3 -B .github/scripts/database_bootstrap_environment_readback.py "$evidence_dir" \
     --bootstrap-reviewer "$approved_bootstrap_reviewer" \
     --baseline-reviewer "$approved_baseline_reviewer"
   ```

   Record the capture time, REST API version, endpoints, approved identities,
   checker revision, output and exit status in release evidence. Exit 0 means
   both saved REST readbacks match. GitHub's branch policy schema makes `type`
   optional. The saved list must explicitly identify the sole `main` rule as
   `type: "branch"`. An omitted type fails the check because the rule could
   target a tag. Recheck the live settings before approval.
4. GitHub's published GET Environment schema leaves `can_admins_bypass`
   unspecified. Plan to record the custodian's direct Settings readback of the
   unchecked bypass control for each exact Environment. If the REST body omits
   the field or supplies a non-boolean value, the checker reports bypass as
   unresolved and exits nonzero. Keep that result alongside the Settings
   readback. The Settings readback covers administrator bypass only. An explicit
   REST value of `true` is a failed protection check.

#### Offline before and after rehearsal

Run this single case from the repository root. It uses the existing checker
on temporary synthetic response bodies for both Environments. Reviewer
identities in the fixture are synthetic.

```sh
python3 -B .github/scripts/test_database_bootstrap_environment_readback.py \
  EnvironmentReadbackTests.test_custodian_handoff_before_and_after
```

| Synthetic fixture | `production-db-bootstrap` | `production-db-baseline` | Checker exit |
| --- | --- | --- | --- |
| Before | `main` branch and `main` tag refused | Administrator bypass enabled and refused | `1` |
| After | Sole `main` branch and bypass disabled | Sole `main` branch and bypass disabled | `0` |

Both after readbacks also include the sole approved reviewer and prevention
of self-review. The result applies to the synthetic bodies. The custodian
records the actual Settings and REST readbacks for the live handoff.

### Custodian handoff

| Workflow | Protected job Environment | Repository variable kept unset until that attempt is authorized |
| --- | --- | --- |
| [First-launch database bootstrap](../.github/workflows/deploy-database-bootstrap.yml) | `production-db-bootstrap` | `DATABASE_BOOTSTRAP_CANDIDATE_SHA` |
| [First-launch database baseline](../.github/workflows/deploy-database-baseline.yml) | `production-db-baseline` | `DATABASE_BASELINE_CANDIDATE_SHA` |

1. Keep both candidate variables unset while the prerequisites are gathered.
   For the stock PostgreSQL staging target, carry the merged disposable
   Stage A result into the fresh Stage B rerun, then qualify capacity,
   scoped authority, and combined Tunnel/Access and origin TLS at Stage C0.
   Before production database setup, complete the served private trial on
   the approved staging target and capture the exact-candidate staging
   evidence in steps 1–3 of
   [Qualify the candidate](first-private-music-launch.md#qualify-the-candidate).
   [Staging database qualification](../ops/staging-trial-pg/README.md)
2. Obtain the GitHub custodian's recorded authorization for the separate
   Environment change and record the independently approved reviewer for each
   Environment. The custodian captures the before state, including an absent
   Environment if applicable. In the approved change, explicitly configure
   both Environments with their sole required reviewer, self-review
   prevention, administrator bypass disabled, and a custom deployment rule
   for the `main` **branch** only. Capture the after state and recheck Settings
   using the [Environment protection readback](#environment-protection-readback).
   If REST leaves only bypass unresolved, retain the nonzero result and the
   custodian's explicit Settings readback. Any other discrepancy keeps the
   handoff pending. Completing this change leaves both variables
   unset and stages no authorization.
3. Before the first protected write, verify SQL custodian authority, the
   exact production branch and direct TLS route, and the ability to issue,
   fence and retire the scoped roles. Verify the successful pre-bootstrap
   snapshot and obtain independent release approval for the exact candidate
   and protected setup plan. Only then use the bootstrap admission and pending
   deployment checks above. The approved Environment reviewer authorizes that
   exact pending attempt after verifying its pins and current protection.
   [Setup role custody](#setup-role-custody-and-lifecycle) ·
   [Snapshot and metering](#snapshot-and-metering) ·
   [Production releases](../CONTRIBUTING.md#production-releases)
4. When the setup attempt ends, clear its candidate variable and authorization.
   Complete cleanup even when setup is refused, canceled or needs
   reconciliation. Under [setup role custody](#setup-role-custody-and-lifecycle),
   reconcile any uncertain issuance before proceeding. If the temporary
   setup role was issued, retire it using the verified issuance OID,
   require `retired`, and confirm removal of its credential, role and
   sessions. If the attempt ended before issuance, verify and record that
   the planned role is absent on the approved branch. Keep baseline pending
   until cleanup, setup success and the new database OID, reader fence,
   grants and sibling isolation are verified.
5. Use the verified database OID and the separately issued reader OID for the
   baseline plan. Recheck the baseline Environment and confirm approval of
   the baseline candidate and plan. Obtain Environment approval for its exact
   pending attempt as part of the [protected baseline](#protected-baseline)
   sequence. Require `empty` and completed reader teardown before handing off
   to the later migration and cutover gates.

### Setup plan and enforced limits

Version 2 is one UTF-8 JSON object with exactly `version`, `host`, `port`,
`ca_pem`, `maintenance`, `launch`, `branch_id`, `setup_login`, `setup_role`,
`owner_role`, `migrator_role` and `reader_role`. The version and port are
JSON integers. All other values are strings. Duplicate fields, missing or
extra fields, and every other version, including version 1, are refused.
A new approval must bind the SHA-256 of the exact version 2 bytes.

- `branch_id` is the approved PlanetScale branch ID. This caller accepts
  10- or 12-character IDs made up of lowercase ASCII letters or digits.
- `setup_role` is the bare SQL setup role and may contain 1–63 ASCII
  letters, digits, underscores or hyphens. Use this name for catalog
  checks, SQL role attributes, grants, ownership and teardown. The SQL
  session must match it exactly as both `session_user` and `current_user`.
- `setup_login` is the direct primary connection username. It must equal
  `setup_role + "." + branch_id` exactly. The caller passes it as `PGUSER`.
  Bare logins, mismatched suffixes or prefixes, and replica or pool
  selectors are refused before invoking `psql`. The password is separate.

PlanetScale documents this routing format for SQL-created roles and
distinguishes `username` and `base_username` in the managed Roles API.
Independently verify the branch ID and direct endpoint against the approved
provider target before approving the digest. Tuple validation checks their
consistency with the approved plan. The authorization is the source of the
branch and endpoint pins. The caller requires a single DNS name or IPv4
address, port 5432, one PEM trust certificate, distinct SQL roles and
verified TLS. [Provider roles](https://planetscale.com/docs/postgres/connecting/roles#creating-roles-via-create-role)
· [Direct connections](https://planetscale.com/docs/postgres/connecting#authentication)
· [Roles API schema](https://planetscale.com/docs/openapi.yaml)

The catalog check requires a primary, a non-admin `CREATEDB` login expiring
within the authorization, a loginless owner and a non-inheriting reader with
`LOGIN`, a one-connection limit, `VALID UNTIL 'epoch'`, and no memberships in
other roles, elevated role attributes or open sessions. Verify `PASSWORD NULL`
at issuance. The setup login cannot read the password catalog.

After fenced creation, one transaction revokes PUBLIC, admits the migrator
and transfers ownership. After `complete`, use a separately authorized,
TLS-verified read-only catalog check on the approved direct endpoint to
record the provider branch, endpoint and new `pg_database.oid` privately.
Match the expected owner and ACLs, confirm that the pending reader still
lacks launch CONNECT, and compare the staging and reserved database OIDs,
owners, connection settings and ACLs to the pre-setup observation. An
unexpected result pauses the handoff. Use the verified OID for the protected
baseline. Reader and serving grants and migrations use separate protected paths.

The hosted disposable fixture models routing with a test-only adapter at
the `psql` boundary. It checks the exact routed `PGUSER` on each invocation,
then supplies the bare name to real libpq and TLS PostgreSQL. It also maps
the approved login to a different eligible SQL role and verifies refusal
before writes. Before live writes, authorized provider readback must verify
that the approved branch ID belongs to the pinned direct endpoint and that
the routed primary login under verified TLS reaches the maintenance database
with both SQL identities equal to the expected bare role. Read back the
actual role attributes, memberships and expiry through the protected path.

Admission limits the event to 256 KiB, the decoded plan to 4 KiB and
the remaining authorization to two hours. The caller accepts up to 1 KiB
of password bytes, submits at most three fixed scripts of up to 4 KiB
each, retains at most 64 stdout bytes per script and discards stderr.
The shared client deadline is 60 seconds or the remaining authorization,
whichever is shorter. GitHub caps the step at two minutes and the job at
five. The client requests a three-second connect timeout. PostgreSQL
receives 10-second statement, two-second lock and 15-second transaction
timeouts. Server processing, wire bytes, provider metering and hard-kill
session closure require separate evidence. A failure from the point of
attempted creation is uncertain. The caller stops without retry or deletion.

### Setup handoff after a rerun

GitHub keeps the run ID and increments the attempt on a rerun. The
[setup workflow](../.github/workflows/deploy-database-bootstrap.yml) gates
both jobs on attempt `1`, and the
[setup caller](../.github/scripts/database_bootstrap_setup.py) checks the
same attempt and authorized run before opening a SQL connection. The
caller's fixed `complete` verdict carries no run identity. Attribute it to
the protected job in its attempt-specific GitHub record. The setup plan
digest and the database OID identify the approved bytes and the SQL target.
Both can match a saved readback for an earlier attempt.

Rehearse with synthetic run `8102`, branch `fixture001`, database
`launch_fixture` (OID `41003`) and issued setup role OID `41001`. Use the
same candidate SHA and setup plan digest throughout. Assume the saved
Environment protection and scoped SQL issuer authority are verified for the
original approval. Evaluate these records in order:

| Evidence presented | Handoff decision |
| --- | --- |
| Run `8102` was rerun. The presented setup readback cites `8102/1`, and the branch, database OID and plan digest match. The latest attempt has no verified job readback. | Keep the handoff pending. Preserve the original approval and issuer records under `8102/1`. Read back the current attempt and its jobs from the attempt-specific GitHub endpoints. |
| The `8102/2` readback confirms the protected setup job was skipped. The original setup job ended without a verified `complete` verdict or its SQL cleanup is unresolved. | Record the rerun as skipped. Reconcile the original attempt under SQL custody on the approved TLS route, using the recorded issuance OID for role cleanup where applicable. Keep baseline pending while the outcome or cleanup is unresolved. |
| Complete control — the `8102/2` readback confirms the skip. The separate `8102/1` record shows the approved protected job succeeded and emitted `complete`, with the approved candidate and exact plan digest. Cleanup readback confirms removal of the candidate variable, authorization and credential material, retirement of the issued setup role and absence of its sessions. A current read-only catalog check on the approved TLS route confirms OID `41003`, expected owner and grants, the pending reader's connection fence and unchanged sibling databases. | Retain the completed setup under `8102/1` and carry its verified database OID into preparation for a separately approved baseline. Record `8102/2` only as skipped. |

The original approval and issuance records remain evidence for their own
attempt and role. A later authorized setup needs a new dispatch and its own
approval. The baseline retains its separate Environment, run and reader
issuer checks. [Workflow attempt readback](https://docs.github.com/en/rest/actions/workflow-runs#get-a-workflow-run-attempt)
· [Jobs for a workflow attempt](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run-attempt)

### Protected baseline

The [baseline workflow](../.github/workflows/deploy-database-baseline.yml)
shares setup's lock and uses a separate `production-db-baseline` Environment.
Complete the [Environment protection readback](#environment-protection-readback)
before setting the repository variable `DATABASE_BASELINE_CANDIDATE_SHA` to the
approved main SHA. Dispatch from `main` with zero inputs. The staging preflight
rejects any baseline authorization visible outside the protected Environment.

For the first attempt, stage `BASELINE_ENABLED=true`, `BASELINE_SHA`,
`BASELINE_RUN_ID`, `BASELINE_EXPIRY`, `BASELINE_PLAN_B64`, `BASELINE_PLAN_DIGEST`
and `BASELINE_READER_PASSWORD` only in `production-db-baseline`. Use the
pending deployment's run ID, an absolute Unix expiry in seconds, standard
base64 of the exact UTF-8 JSON plan, and its lowercase hex SHA-256. Check
the pending deployment and its approval history as for setup. Activate the
reader just before approving the job, with expiry at most 120 seconds from
activation and no later than the run authorization. Keep the candidate
variable unset outside the approved dispatch.

The version 2 plan requires `version`, `host`, `port`, `ca_pem`, `branch_id`,
`launch`, `database_oid`, `reader_role` and `reader_oid` only. Use JSON
integers for version, port and both OIDs. Pin the setup readback's database OID
and the issuance receipt's reader OID. Activation checks that the plan and
the receipt refer to the same role and match the setup route. The runner
routes as `reader_role.branch_id` over verified TLS and checks both SQL
identities and both OIDs before reading any data. Give the reader only launch
CONNECT, schema USAGE and direct SELECT on physical relations. Quiesce schema,
grants and writes through the read.

One TLS-verified read-only snapshot checks primary status, identity and rights.
It scans up to 256 user relations and large objects, and refuses incomplete coverage.
Ledger and reference rows count as data. Record the job URL, attempt and verdict.
Link the [SQL issuer authority evidence](#sql-issuer-authority-evidence) in
the baseline review alongside the verified setup readback and reader issuance OID.

When reviewing the first-launch baseline, reconcile the saved catalog inventory
and required reader privilege result with the accepted baseline plan. Use the
[offline reader evidence check](../.github/scripts/database_bootstrap_reader_evidence.py)
from the repository root with a private, normalized JSON record on stdin.

```sh
python3 -IB .github/scripts/database_bootstrap_reader_evidence.py < evidence.json
```

- `review` contains the approved `branch_id`, `database_oid`, `reader_role`,
  `reader_oid` and `inventory_sha256` of the reviewed catalog inventory.
- `inventory` contains the observed `branch_id`, `database_oid` and
  `inventory_sha256` for the complete catalog readback.
- `reader_privileges` contains the observed `branch_id`, `database_oid`,
  `inventory_sha256` of the catalog covered by the privilege result, and the
  role name and OID that the result actually checked (`reader_role` and
  `reader_oid`). Record `result: passed` only for a completed, successful
  privilege check. Use JSON integers for OIDs and lowercase hex for digests.

The protected baseline emits only a fixed verdict. Arrange a separate authorized
read-only catalog capture in advance. Take the snapshot while the scoped reader
is active, before fencing or retirement. Retain the observed role identity,
effective privilege result and covered inventory together from that snapshot.
Follow the normal cleanup even if capture is incomplete. Use the same
deterministic representation for all three inventories. Include object identities
and their schema definitions or versions so a change to a definition changes the
digest even when the object has the same name. Hash the definitions actually
observed for each readback.
If the privilege readback is unavailable, omit `reader_privileges` so the
evidence stays pending.

Both pending messages leave baseline evidence pending. A plain `pending`
covers absent or unusable readback and other evidence failures, including
a privilege result for a different reader. Recover any missing private
readback for the same attempt. `pending (inventory-mismatch)` means a complete,
scoped inventory differs from the review, or a passing privilege result for the
reviewed reader and database covers a different inventory. Use the separately
authorized read-only catalog check on the reviewed database to compare expected
and observed object identities and definitions. Once the catalog agrees with the
review, obtain a matched inventory and scoped privilege result from one
authorized read-only catalog snapshot while the scoped reader is active, then
rerun the offline check. After reader retirement, use only a retained matched
readback from the same attempt. If it is unavailable, keep the baseline pending
and migration closed. The reader preparation helper requires an absent launch
database.
`consistent` confirms the supplied evidence agrees. The protected run's
`empty` verdict, provenance and retirement evidence are still required.
Keep credentials and raw SQL results out of the normalized record.

### Carry verified staging evidence into database approval

Use the [pinned staging attempt and admission record](first-private-music-launch.md#qualify-the-candidate)
for each exact database candidate. Match the candidate SHA, current staging run
and attempt, and unique attempt-bound artifact and Actions digest. Retain the
matching `verified_private_music_staging` pins and verifier job reference from
[staging admission](../ci/staging-admission.md#verified-private-staging-pins).
If verification is unavailable, the receipt is superseded or the candidate SHA
changes, keep the affected approval pending until the exact candidate has
eligible staging evidence. Live evidence depends on the protected same-build
journey and reviewed Product and QA enrollment. The checked-in policy has no
approval, and the [workflow that calls the verifier](../.github/workflows/deploy-production.yml)
currently closes application promotion at preflight.

In the private release record, cite that staging evidence from each exact-plan
approval. For example, with a synthetic candidate `<sha>` and staging run `100`,
attempt `1`, `#evidence-staging-100-1` points to
`staging-admission-<sha>-100-1-private-music`, its verified digest and job
evidence. The first-attempt database run IDs below come from the corresponding
pending deployments.

| Private approval reference | Candidate and protected authorization |
| --- | --- |
| `#evidence-bootstrap-200-1`, citing `#evidence-staging-100-1` | `DATABASE_BOOTSTRAP_CANDIDATE_SHA` is `<sha>`. The bootstrap Environment binds `BOOTSTRAP_SHA` to `<sha>` and `BOOTSTRAP_RUN_ID` to `200`, with the approved setup plan's exact bytes and SHA-256 in `BOOTSTRAP_PLAN_B64` and `BOOTSTRAP_PLAN_DIGEST`. |
| `#evidence-baseline-201-1`, citing `#evidence-staging-100-1` and the verified setup readback | `DATABASE_BASELINE_CANDIDATE_SHA` is `<sha>` while that exact candidate remains eligible. The baseline Environment binds `BASELINE_SHA` to `<sha>` and `BASELINE_RUN_ID` to `201`, with its separately approved plan's exact bytes and SHA-256 in `BASELINE_PLAN_B64` and `BASELINE_PLAN_DIGEST`. The plan uses the verified launch database OID and the separately issued reader OID. |

Both preflights require the approved SHA to be the dispatched `main` tip.
If `main` advances between successful setup and baseline dispatch, retain the
original setup approval and verified database OID. Qualify the new tip in
staging and obtain fresh baseline approval for that SHA and the exact baseline
plan before enabling baseline. The earlier receipt stays bound to its own SHA.

Both workflows dispatch with zero inputs. The evidence references stay in the
private release record. The protected runners consume their own Environment's
authorization. Bootstrap uses `BOOTSTRAP_EXPIRY` and `BOOTSTRAP_SETUP_PASSWORD`.
Baseline uses `BASELINE_EXPIRY`, `BASELINE_READER_PASSWORD` and
`BASELINE_ENABLED=true`. Keep the pending-deployment readback, approval and
cleanup rules above. Setup success and verified setup-role retirement precede
baseline approval.

Rehearse the mapping from the repository root with a synthetic receipt, synthetic
run IDs and synthetic plan bytes. The fixture uses the existing verifier with
substitutes for Actions and serving identity reads, then checks the real database
admission and plan parsers with certificate validation stubbed out.

```sh
python3 -B ci/test_staging_admission.py \
  SourceBindingTests.test_private_staging_receipt_handoff_to_database_admission
```

### Reader role custody and lifecycle

The dormant [reader SQL](../.github/scripts/database_bootstrap_reader_role.py) prepares a
passwordless setup-visible login through a separate pinned custodian. After setup, match
the baseline plan to the setup route and the new database OID. Activate once, with a
fresh SCRAM credential expiring within 120 seconds and no later than the authorized attempt.
Reconcile shared ACLs separately if activation refuses. The fixture uses direct SELECT on
physical relations. Only the protected read-only caller may use the credential, since its
SQL role grants permit reading the underlying rows.

Serialize lifecycle calls and hold schema and grant custody until retirement.
After success, refusal or cancellation, remove the candidate variable, all
seven Environment secrets and the 0600 connection reference from secure
custody. The runner holds only the reader credential, closes its own client
and removes the temporary CA. The separate authorized SQL custodian fences
the reader from maintenance using the private issuance OID. Keep the custodian's
private copy of the exact activation plan through retirement readback. Use that
plan to retire the reader on the pinned launch database, drain sessions and
revoke grants. Require SQL success and the fixed retired result, and
read back credential removal and role and session absence. If activation never
granted access, maintenance retirement also works, including after interrupted setup.

After cancellation or an uncertain result, verify the job stopped and fence
the reader even if the launch route is unavailable. Keep the role fenced while
reconciling its issuance identity, grants, and sessions before completing retirement.
Use a fresh run, approval and role for another attempt. The disposable fixture
exercises the routed caller and custodian with synthetic credentials and a
private temporary connection reference. GitHub secret removal and the live
custodian readbacks belong to the protected release.

#### Offline rehearsal: stop before baseline capture

Use the placeholder records below for a tabletop exercise. No workflow or
database connection is needed. Start with completed setup on `fixture001`
(database OID `41001`) and a private activation plan for reader OID `42001`.
In the exercise, the reader has been activated for run `101`, attempt `1`.
Preflight has passed and the baseline job is waiting for Environment approval.
The release operator calls a stop while the job is still waiting.

| Readback to rehearse | Expected decision |
| --- | --- |
| Run `101` is canceled. Its baseline job never started and has no verdict. | Record the stopped attempt and leave the new baseline absent. Keep migration closed even if a prior receipt says `empty`. |
| The candidate variable, seven baseline Environment secrets and 0600 connection reference are absent. The issuance OID and exact activation plan remain in private SQL custody. | Verify authorization removal. The separate SQL custodian carries out [reader retirement](#reader-role-custody-and-lifecycle) even though the job never started. |
| Maintenance readback for reader `42001` shows no login and a cleared credential. | Keep the fence committed even if the launch route is unavailable. Keep migration closed while retirement is unresolved. |
| On the pinned launch database `41001`, retirement commits with `retired`, the grants are revoked, and the role and its sessions are absent. | Record cleanup complete. The canceled attempt still has no baseline. |
| Run `102`, attempt `1`, is proposed for the same setup target. | Require fresh approval, run-bound authorization and a new reader role. Verify a new baseline for the same database and branch before using it in the existing release handoff. |

#### Interpret incomplete readback after setup

Record setup and the read-only baseline separately in the private release
record. Use the job URL and attempt for each fixed verdict. For setup,
`database-bootstrap-setup: complete` must come from the successful protected
setup job for the approved candidate and plan. Retain the branch and database
OID from the [setup catalog readback](#setup-plan-and-enforced-limits). To verify
empty, use the successful protected baseline job for the plan pinned to that
same branch and database OID and to its issued reader OID.

In the stop rehearsal above, setup on synthetic branch `fixture001` yields
database OID `41001`. Compare that completed setup with the other cases below.

| Existing evidence | Record and next read-only step |
| --- | --- |
| The protected setup job is confirmed to have never started, for example it was skipped before execution and there is no earlier setup attempt for the target. | Record setup as never run. Use the [catalog preflight](#what-the-source-check-establishes) to verify target absence on the approved maintenance route. |
| The setup caller returned `database-bootstrap-setup: refused`. | Record that this attempt refused before a write. Use the catalog preflight to check whether the launch name is absent. If the name exists, stop and [reconcile database identity and state](#what-the-source-check-establishes) through the separately authorized read-only check. |
| The setup job record is missing, a started setup job has no usable verdict, or setup returned `outcome-uncertain`. | Leave setup unverified. Read the approved run and attempt's job record and reconcile the target catalog under the existing setup procedure. |
| Setup is verified complete. The baseline job never started, its verdict is unavailable, or the saved readback is incomplete. | Keep setup complete and baseline evidence pending. If a baseline attempt exists, read its job conclusion and fixed verdict and recover any private readback for that attempt. If the evidence is still unavailable, follow the read-only steps below. |
| Setup is verified complete. The baseline returned `database-bootstrap-baseline: refused`. The disposable fixture exercises refusal when a leaf relation is unreadable or uses row-level security. | Keep setup complete and baseline evidence pending. `refused` supplies no specific cause. Use the approved read-only catalog route to reconcile the target OID, relation coverage and effective reader privileges before deciding whether the supported baseline can verify this target. |
| Setup is verified complete. The matching baseline succeeded with `database-bootstrap-baseline: empty` and its readback is available. | Record verified empty at the observation time. Continue with the existing reader retirement and release handoff. A matching `rows-present` verdict instead follows the [release path for a database with data](first-private-music-launch.md#qualify-the-candidate). |

If migrations may already have run, first reconcile the saved migration run
and ledger against the reviewed manifest on the same pinned database using
read-only access. Leave the database and applied migrations in place. Keep
migration continuation and cutover pending while their evidence is unresolved.

If the first schema change is still pending and the empty-first-release path
applies, use the existing separately authorized read-only baseline for the
same database after reconciling any prior reader retirement. Record the new
verdict under its own attempt and observation time. Keep any unrecovered
verdict from an earlier attempt unverified.

### Setup role custody and lifecycle

The dormant [role SQL](../.github/scripts/database_bootstrap_role.py)
uses the same plan's bare setup and owner roles. The separate custodian
supplies an approved absolute expiry within two hours and a freshly
generated password. SQL carries a SCRAM verifier derived from that password.
Keep the verifier private with the other issuance inputs. Issuance commits
the setup role and its single SET-only, non-inherited, non-admin owner
membership together. It grants LOGIN and CREATEDB, caps concurrent
connections at one and reads back the exact attributes, expiry and
membership. PostgreSQL gives a non-superuser issuer an administrative
grant on the new role. Issuance checks that this grant carries neither
SET nor INHERIT. CREATEDB is cluster-wide during the window. The protected
setup caller pins the target database name. The aggregate reader keeps its
own credential and grants.

Keep the private issuance OID and require exclusive custody of the role
through retirement. A duplicate issuance refuses. After ambiguous issuance,
reconcile catalog identity under that custody before supplying a receipt.
The first cleanup transaction removes login, password and CREATEDB. Commit
it before terminating sessions. The second transaction confirms the fence,
terminates sessions for the issued OID and drops the role with its
memberships. Require the fixed `retired` verdict and a successful commit.
A matching absence can be retried. A replacement role with the same name
is refused. If the role still owns a database, the drop fails while the
login fence stays committed. Reconcile the database separately before
retrying cleanup. Keep role SQL, credentials, OIDs and raw diagnostics
in private custody and out of published evidence.

`VALID UNTIL` limits fresh password authentication. An already-open
session can continue after expiry or a login fence until terminated. The
hosted disposable proof uses verified TLS, SCRAM, a scoped non-superuser
issuer and the test-only routing adapter. It exercises expiration, an open
session with the owner selected, and cleanup after successful and
interrupted setup.

PlanetScale's managed role interfaces expose a fixed set of inherited
roles and a provider TTL. They lack `CREATEDB` and SET-only membership
controls, and the TTL documentation gives no guarantee for `rolvaliduntil`.
The SQL route requires PostgreSQL 16+ membership options, a custodian with
effective `CREATEROLE` and `CREATEDB`, authority to grant the owner role
and terminate the setup role's sessions, and the routed primary username
on the approved direct endpoint with hostname verification. The provider
documents SQL-created roles under its default administrator. The fixture
issuer uses `pg_signal_backend`, which can signal other non-superuser
sessions. Cleanup selects sessions by the issued role's OID before sending
signals. Protected live proof must establish that the custodian can issue
and retire this exact role and that the routed endpoint enforces native
password expiry, exposes both bare SQL identities, admits the caller's
sequential connections under the connection limit and lets the custodian
terminate and read back its sessions.
[PlanetScale roles](https://planetscale.com/docs/postgres/connecting/roles)
· [Managed Roles API](https://planetscale.com/docs/api/reference/create_role)
· [PostgreSQL CREATE ROLE](https://www.postgresql.org/docs/current/sql-createrole.html)

### SQL issuer authority evidence

Before each release review, the Operator adds the applicable issuer evidence
to the existing private release record. Bind each entry to its protected
workflow, candidate SHA and SHA-256 of that workflow's exact plan bytes
(`BOOTSTRAP_PLAN_DIGEST` or `BASELINE_PLAN_DIGEST`).
Keep the staging qualification and Environment protection references in
the same release record. The [Environment checker](#environment-protection-readback)
checks saved GitHub policy. SQL authority requires its own observed evidence.

For each SQL issuer operation, record the expected issuer and selected database
alongside the observation. Include the provider branch and verified direct
primary TLS route, the bare issuer role and its catalog OID, `session_user`,
`current_user`, `current_database()`, and the database OID where it exists.
Record when the evidence was observed and link the private readback showing
effective authority for the named roles and database. Match both session
identities to the expected issuer and match the observed route and database
to the plan and operation. Keep credentials, connection strings, SCRAM
verifiers and raw diagnostics in their existing custody outside this record.

| Release review | Selected database and role scope to verify | Where the evidence goes |
| --- | --- | --- |
| Bootstrap setup | On maintenance, verify the setup issuer's `CREATEROLE` and `CREATEDB`, authority to grant the named owner with SET-only membership to the temporary setup role, and authority to fence, drain and drop that role. Record the planned launch name and its verified absence. The reader issuer also uses maintenance to prepare the named passwordless reader and must be able to fence, drain and retire an unactivated reader there. Verify its non-superuser `CREATEROLE` and authority scoped to that reader. | Cite the issuer evidence in the bootstrap plan's review. Keep the issued setup and pending reader OIDs in separate private issuance readbacks once those operations are authorized and completed. |
| Protected baseline | Use the setup readback's launch database and OID for reader activation and launch grant cleanup. Verify that the pinned reader issuer has authority to administer the issued reader and grant and revoke launch CONNECT, schema USAGE and direct SELECT over the intended physical relations. Verify it can fence from maintenance, terminate the issued reader's sessions and retire that role. Match the private reader issuance OID and the setup route to the baseline plan. | Cite the launch/database and reader OID readbacks and the reader issuer evidence in the baseline plan's review. |

Use the existing release sequence for role changes. Preparing this record
uses observed evidence and performs no role issuance, activation or dispatch.
The issuer identities and authority readbacks stay in the private release
record. The setup and baseline callers still take their respective version 2
plans through protected admission.
Any missing, mismatched or unresolved issuer evidence keeps that preparation
pending. Record the specific gap and link the observation that shows it.

#### Offline issuer evidence rehearsal

Use one synthetic candidate SHA and the separate SHA-256 digests of its
synthetic setup and baseline plans. In the bootstrap entry select `postgres`
(OID `5`) on branch `fixture001` at `db.fixture.invalid` on port `5432`. Record a
matching primary TLS observation. Pin `fixture_issuer` (OID `41001`) for
`fixture_creator` and `fixture_owner`, and `fixture_reader_issuer` (OID
`41002`) for `Fixture-Protected-Reader`. Both session identities match the
respective issuer. The intended target `launch_fixture` is absent. In the
later baseline entry use the same branch and endpoint, select `launch_fixture`
with setup readback OID `41003` and link the reader issuance OID `41004`.
Use a synthetic timestamp and a private readback reference for each
observation. The positive control records every scoped capability in the
table above as verified for its issuer and operation.

For each workflow, use `sha256sum` on its version 2 plan file. Compare the
computed digest with the digest in that workflow's issuer entry. A mismatch
keeps the entry pending. Record the digest comparison and the Operator's
disposition for the bootstrap and baseline entries.

| Synthetic evidence reviewed | Preparation outcome to record |
| --- | --- |
| Both bootstrap issuers match their expected identities and maintenance route. The setup issuer's authority to delegate `fixture_owner` is unresolved. | `Pending — SQL issuer authority incomplete` for bootstrap. Leave baseline pending through setup and its readback. |
| Bootstrap evidence also verifies delegation of the named owner and the remaining scoped issue, fence, drain and retirement capabilities. | The bootstrap issuer evidence is complete. With the rest of the bootstrap prerequisites supplied, mark the preparation `Eligible for independent release review`. |
| The later baseline record has matching launch and reader OIDs, but the reader issuer's direct SELECT grant option is unresolved, or the observation came from maintenance. | `Pending — SQL issuer authority incomplete` for baseline. |
| A complete baseline issuer entry carries the bootstrap plan digest. | `Pending — SQL issuer authority incomplete` for baseline. Record the plan-digest mismatch. |
| The baseline record carries the baseline plan digest and verifies the pinned issuer on the selected launch database, the launch grant and cleanup scope, and the maintenance fence. | With the rest of the baseline prerequisites supplied, mark the preparation `Eligible for independent release review`. |

These are Operator entries. Keep release approval pending until the reviewer
decides. The [hosted disposable fixture](../.github/scripts/test_database_bootstrap_isolation.py)
exercises the underlying custody behavior with synthetic SQL issuers. It
refuses setup without owner delegation, reader preparation under another
issuer, and reader activation on the wrong database or with a missing grant
option. The complete controls exercise the intended role operations on the
disposable instance.

### Snapshot and metering

Standard hosted public-repo minutes are free. A private Enterprise repo can
enforce the same Environment rules and restrict log readers. Confirm its
approved custody, included compute balance and paid overage policy before
using it. No larger runner, new repository or paid service is needed for
the disposable proof. [GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)

For an independently verified empty first release, prefer a successful
ordinary native snapshot of the exact branch before the first protected
write. Coordinate the release window with that schedule and read back the
capture time, completion, restore scope and expiry. The pre-bootstrap
snapshot recovers the branch before the launch database and grants exist.
After setup, verify the launch empty-state separately. If migration recovery
needs to preserve the new database and grants, verify a successful snapshot
after setup and before migration. Prefer the next ordinary snapshot when
its timing and scope satisfy that recovery objective. Keep the ordinary
retention policy. If the gate needs a different snapshot, verify eligibility,
retention and budget separately. Each expired backup releases its own
storage, and the oldest retained backup pins the WAL window across the
whole branch. A retention limit controls time. It does not bound snapshot
bytes or WAL generation. A narrow client transfer budget bounds its own
reads when enforced before use. Shared provider egress and billable usage
require a dispatch-time billing readback. Keep any monetary exposure in
the release evidence. [PlanetScale backups](https://planetscale.com/docs/postgres/backups)
· [Pricing](https://planetscale.com/docs/postgres/pricing)

Use the [offline snapshot check](../.github/scripts/database_bootstrap_snapshot.py)
with an operator-normalized JSON record in private custody. Run
`python3 -IB .github/scripts/database_bootstrap_snapshot.py < evidence.json`
from the repository root. The check reads at most 4 KiB of UTF-8 JSON and
emits a fixed verdict. It checks the consistency of values supplied by the
operator. Provider readback and release decisions remain separate.

Supply exactly two objects in the record. Use integer UTC Unix seconds for
all times and the same precision for observations and the reviewed schedule.

- `review` contains the approved `branch_id`, `phase`, the applicable
  `protected_write_at` and `window_end_at`. Use `pre-bootstrap` for the
  first protected write. For optional `pre-migration` evidence, use the
  migration write time and also supply the verified `setup_completed_at`.
- `snapshot` contains the observed `branch_id`, `captured_at`,
  `completed_at`, `readback_at` and `expires_at`. Normalize the confirmed
  kind, restore scope and completion as `kind: ordinary-native`,
  `restore_scope: whole-branch` and `status: succeeded`.

If the provider reports success but omits the capture time, leave snapshot
evidence pending until you can verify the capture time.

Capture must be at or before completion, and completion at or before
readback. Read back success before the applicable write, which must occur
by the reviewed window end. Expiry must be after that end. For
pre-migration evidence, capture must be strictly after setup completion.

When the post-setup recovery objective calls for a pre-migration snapshot,
cross-check it against the exact protected baseline plan. In private custody,
add `baseline_branch_id` at the top level using `branch_id` from the version 2
plan bound to the verified baseline run. Check that the run returned `empty`
for the database OID from the completed setup readback. Run
`python3 -IB .github/scripts/database_bootstrap_snapshot.py --baseline-handoff < evidence.json`.
This mode requires the pre-migration phase, recorded setup completion and a
snapshot on the baseline branch. If evidence is incomplete or mismatched,
pause continuation and rerun this offline check with the matching readbacks.
Use the Environment and baseline checks separately as part of the protected
release process.

If private evidence publication fails or saves only part of the record after
setup and baseline readback, keep migration closed and finish reader retirement
under the original issuance pins. Reconcile the existing database over the
approved route against the recorded database OID before retrying publication.
Require a complete saved handoff for the same branch before continuing. The
disposable CI fixture rehearses both publication failures with local files and
verifies reader and session removal before checking retry evidence.

#### Retry when a snapshot label is already in use

If a provider response says the snapshot label already exists, keep migration
closed while you reconcile the prior attempt. Use the provider snapshot
identifier in the private prior readback to identify that snapshot on the
approved branch. A label alone leaves identity unresolved. Keep the existing
snapshot and its retention in place while reconciling. Once the prior snapshot
is verified, normalize its complete readback and run the same
`--baseline-handoff` check.

In a synthetic retry on branch `fixture001`, the prior readback identifies
snapshot `fixture-a` and the response names the existing label
`fixture-handoff`.

| Evidence in the retry | Expected decision |
| --- | --- |
| The response names only the label. | Keep migration closed pending readback. |
| The same label points to `fixture-b` on the same branch, captured before setup and completed after setup. | Keep migration closed. The readback identifies a different snapshot, and the capture predates setup. |
| The readback identifies `fixture-a` and confirms a successful capture after setup with the reviewed timing and restore scope. | The offline check accepts the complete evidence. The separate protected release checks still apply. |

## Post-migration schema inventory

Before app cutover, reconcile the current migration manifest and ledger on
the launch database. Keep the schema inventory readback and its provenance
in private custody. Run the [offline inventory check](../.github/scripts/database_bootstrap_inventory.py)
from the repository root with `python3 -IB .github/scripts/database_bootstrap_inventory.py < inventory-binding.json`.

Supply exactly two JSON objects under `migration` and `inventory` in one
record. Both contain `branch_id`, the integer `database_oid`, and the decimal
strings `run_id` and `run_attempt`. Take the `migration` fields and integer
`completed_at` from the current successful protected migration. Take the
`inventory` fields and integer `readback_at` from the saved readback and its
recorded provenance. Use UTC Unix seconds. Keep the run and attempt from that
provenance when preparing the record.

The checker reads up to 4 KiB and returns `consistent` only when the branch,
database OID, run and attempt match and the readback follows completion. A
recent readback associated with an earlier migration run still refuses.
On refusal or missing provenance, keep cutover pending until a readback for
the current run can be verified. Confirm provenance and schema completeness
separately during the manifest and ledger reconciliation.
