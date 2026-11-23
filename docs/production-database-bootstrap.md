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
