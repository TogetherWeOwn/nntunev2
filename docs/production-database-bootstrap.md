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
is disabled at the job boundary and contains only an admission check. The
existing [disposable CI job](../.github/workflows/ci.yml) exercises that
check with generated pins and uses an isolated, ephemeral PostgreSQL server
on the same standard Ubuntu runner class. It exercises verified TCP/TLS,
wrong CA and hostname, separate creator and reader roles, private capture of
command output and traces, and signal-driven disposal. It needs no Environment or real
credentials. The fixture's private Docker network tests the client path.
It supplies no evidence of specific public-origin reachability, provider
role authority or actual deployment approval.

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
protection. In Settings and the [environment REST readback](https://docs.github.com/en/rest/deployments/environments),
verify the required independent reviewer or team, `prevent_self_review: true`
and `can_admins_bypass: false`. Use a custom policy with the sole typed
branch `main`, no tag policy. Verify the list through
[deployment branch policies](https://docs.github.com/en/rest/deployments/branch-policies).
Keep Environment administration with the custodian and check rule changes
before approval. One listed reviewer suffices on GitHub. The release review
and T0 code reviews are separate from this deployment approval.
[Environment protection rules](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)
· [Configure environments](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)

1. The invoker dispatches the exact reviewed `main` tip with zero inputs.
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
3. A separately reviewed live caller must validate the authorization in the
   process that performs the work and consume the exact bytes it validates.
   Keep the invoker and approver separate from SQL custody. Scope the writer
   to the setup step and the separate aggregate reader to the baseline path.
   The release task owns live role validation. Use no workflow inputs or
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
   an independently scoped aggregate reader TTL at most 24 hours. Read back
   that authorization and credential material were removed from the
   Environment and secure custody, that the temporary roles are gone, and
   that their server sessions are closed. A hard kill can skip runner
   cleanup. Cancel the run, verify the final job state and hosted disposal,
   revoke and terminate any surviving database sessions through the protected
   custodian, and reconcile catalog and grant state before considering a new
   attempt. An incomplete readback keeps the release closed.
   [Job cancellation](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-cancellation)
   · [Provider role management](https://planetscale.com/docs/postgres/connecting/roles)

### Snapshot and metering

Standard hosted public-repo minutes are free. A private Enterprise repo can
enforce the same Environment rules and restrict log readers. Confirm its
approved custody, included compute balance and paid overage policy before
using it. No larger runner, new repository or paid service is needed for
the disposable proof. [GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)

For an independently verified empty first release, prefer a successful
ordinary native snapshot of the exact branch before the first protected
write. Coordinate the release window with that schedule and read back the
capture time, completion, restore scope and expiry. Verify the first-release
empty-state separately, before migration. Keep the ordinary retention
policy. If the release gate needs a different snapshot, verify eligibility,
retention and budget separately. Each expired backup releases its own
storage, and the oldest retained backup pins the WAL window across the
whole branch. A retention limit controls time. It does not bound snapshot
bytes or WAL generation. A narrow client transfer budget bounds its own
reads when enforced before use. Shared provider egress and billable usage
require a dispatch-time billing readback. Keep any monetary exposure in
the release evidence. [PlanetScale backups](https://planetscale.com/docs/postgres/backups)
· [Pricing](https://planetscale.com/docs/postgres/pricing)
