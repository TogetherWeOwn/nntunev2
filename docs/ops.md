# NNTune v2 operations

Provisioned resource metadata for staging + prod, as of 2026-10-13.
**Never commit tokens, passwords, database URLs, or personal login identities.**
Secret values live in GitHub Actions secrets, Cloudflare, and the host's secure
credential storage. Resource IDs below are non-credential binding metadata;
the full account ID stays in `CLOUDFLARE_ACCOUNT_ID`, not this public document.
The `gitleaks` gate scans credentials but is not a personal-data policy check.

Every UNDO block is a rollback reference only. **No deletion, credential revocation,
resource mutation, or spend increase is authorized by this documentation update.**
An authorized executor must satisfy the applicable approval/retention gates first.

## Cloudflare

- Account: owner account (short prefix `209cf7dd`).
- Workers Paid + R2 Paid entitlements: confirmed by the host executor on
  2026-10-13; not independently billing-verified by the operator MCP token.

### R2 buckets (created 2026-10-13)

The provisioned layout is **one bucket per concern per environment**, not one
shared bucket. Object key prefixes describe cache scope *within* those buckets;
they do not replace the environment boundary. Worker configs and cache/MusicBrainz
ADRs must use the exact names below. There is no bucket named `nntunev2-art`.
Alignment is **not yet complete** on the sibling branches: the cache ADR in
[PR #2](https://github.com/TogetherWeOwn/nntunev2/pull/2), MB docs/example in
[PR #4](https://github.com/TogetherWeOwn/nntunev2/pull/4), and scaffold configs in
[PR #14](https://github.com/TogetherWeOwn/nntunev2/pull/14) require their owners'
coordinated corrections. This runbook does not change those branches or choose
where the cache ADR's `tmp/` scratch prefix will live.

| Bucket | Env | Intended consumer |
|---|---|---|
| `nntunev2-audio-staging` | staging | stream Worker audio cache |
| `nntunev2-audio-prod` | prod | stream Worker audio cache |
| `nntunev2-art-staging` | staging | MusicBrainz Worker artwork cache |
| `nntunev2-art-prod` | prod | MusicBrainz Worker artwork cache |
| `nntunev2-archive-staging` | staging | reserved archive capacity; no Worker consumer configured |
| `nntunev2-archive-prod` | prod | reserved archive capacity; no Worker consumer configured |

The archive buckets were requested at provisioning time. They are not evidence
of an implemented archival pipeline and must not be bound to a Worker by default.
Do not create unsuffixed replacement buckets or delete these resources to reconcile
an example config; align the examples with this inventory instead.

UNDO (decommissioning reference, not an instruction to execute):
`DELETE /accounts/<acct>/r2/buckets/<name>`, one per bucket, only after authorized
consumer removal and data-retention checks. Never delete cached/archive data as
part of a documentation fix.

### Queues (created 2026-10-13)

| Queue | ID | Env |
|---|---|---|
| `nntunev2-ingest-staging` | `c9e6afdea0ca487c8d20f799a052b045` | staging |
| `nntunev2-ingest-prod` | `2bf7e1a0edd347c9aada2afbc0c24ae5` | prod |

UNDO: API `DELETE /accounts/<acct>/queues/<queue_id>` after authorized
consumer removal and pending-message checks.

### KV namespaces (created 2026-10-13)

| Namespace | ID | Env |
|---|---|---|
| `nntunev2-config-staging` | `1d4f21508a214ebfa8ff7b594989d6f6` | staging |
| `nntunev2-config-prod` | `9664ceed71464521bb6baae2d9374006` | prod |

UNDO: API `DELETE /accounts/<acct>/storage/kv/namespaces/<id>` after authorized
consumer removal and data-retention checks.

### DNS (zone `nntune.com`, id `97e268d5c1d540d42545ed8ab386b9c4`)

- `v2-staging.nntune.com` → `AAAA 100::`, proxied, TTL auto.
  Record ID `97670fbad3e38106c28f71d1df89175e`.
  Placeholder address (`100::` is reserved discard space). The staging deploy
  owner must remove the placeholder when claiming the hostname through a Workers
  custom domain. Provisioning does not establish that the application is deployed.
  v1 `staging.nntune.com` is untouched.
- Prod hostname: not created yet — decided at launch.

UNDO: API `DELETE /zones/<zone>/dns_records/<record_id>` for this placeholder
only; preserve the v1 hostname and any later Worker-owned record.

### Cloudflare Access (staging)

- App: `NNTune v2 staging (nntunev2-staging)`, self-hosted.
- App ID: `785bf815-f579-4768-a1d2-4de7ce19d086`.
- Domain: `v2-staging.nntune.com`; session duration `24h`.
- Owner OTP via IdP `a290d6fa-9235-4ce3-921f-0c9a67916c20`.
  Owner email remains private. Allow policy ID:
  `c2b44297-bab4-4117-a486-57c9421d6a73`.
- Agent QA service-token policy (`non_identity`):
  `3333d7dd-3c14-4b54-8cd2-abc2f4a827f6`, referencing existing service token
  `efe71168-f5e9-4f42-83bf-340473e76e01` (ID only, not credentials).
  Reused at provisioning; no existing key was rotated.
- Created by the host executor; app metadata and both policy types independently
  GET-verified on 2026-10-13. This is configuration verification, not an end-to-end
  login or staging application smoke test.

UNDO: delete only this Access app during authorized decommissioning. Do not delete
or rotate the shared QA service token, and never remove the gate from a live app.

### CI API token + GitHub secrets

- Account-owned token `nntunev2-ci`, created fresh by the host executor.
- Account-scoped Edit/Write groups: Workers Scripts, Workers R2 Storage, Queues,
  Workers KV Storage, Hyperdrive. Workers Routes is scoped to `nntune.com` only.
  No D1 or Pages permissions.
- Host executor verified the account-token metadata as `active` with exactly
  those six groups. `/user/tokens/verify` is not the verification path used for
  this account-owned token. Operator MCP did not independently recheck token IAM.
- Repo secrets `CLOUDFLARE_API_TOKEN` + `CLOUDFLARE_ACCOUNT_ID` set on 2026-10-13
  at `18:46:25Z`; secret-name/update-time metadata independently re-listed.
  No secret value was read for this documentation update.

UNDO: revoke only token `nntunev2-ci` and remove its two repo secrets during
authorized decommissioning, after removing dependent deploy workflows.

## PlanetScale Postgres

Host executor completion and cost-cut records on 2026-10-13 establish the layout
below. PlanetScale sizing and SQL connectivity are host-reported, not an
independent operator database check. Existing TWO databases were not touched.

- Organization: `togetherweown`; region: AWS `us-east-1`; Postgres `18.6`.
- `nntunev2` / `main`: production **PS-10 ARM**, 2 replicas.
- `nntunev2` / `staging`: non-production **PS-DEV**.
- `nntunev2-mb` / `main`: MusicBrainz mirror, production **PS-10 ARM**, 2 replicas.
  The earlier PS-40 allocation was superseded by the completed cost cut.
- MB storage autoscale: gp3, 10 GB–4 TB. The ~100 GB import is a capacity target,
  not already-imported data; host reported the DB was approximately 7 MB at resize.
- Monthly cluster compute: **$60** ($30 app main + $0 staging + $30 MB).
  Metered storage, backups, and egress are additional; $60 is not an all-in bill.
- MB compute after resize: 1/8 CPU + 1 GB RAM (previously 1/2 CPU + 4 GB).
  Expect a slower import and fewer available connections. Before import/deploy,
  its owner must reconcile Worker/Hyperdrive pool limits with the actual branch
  connection allowance; do not assume the prior PS-40 import throughput.

### Database roles and migrator wiring

| Database / branch | Named roles |
|---|---|
| `nntunev2` / `main` | `nntunev2-migrator-main`, `nntunev2-runtime-main`, `nntunev2-ro-main` |
| `nntunev2` / `staging` | `nntunev2-migrator-staging`, `nntunev2-runtime-staging`, `nntunev2-ro-staging` |
| `nntunev2-mb` / `main` | `nntunev2-mb-migrator-main`, `nntunev2-mb-runtime-main`, `nntunev2-mb-ro-main` |

These were created as plain PlanetScale BranchRoles. **Names alone do not enforce
runtime/read-only SQL privileges.** The migration owner must apply and verify the
application-level GRANT/REVOKE role matrix before application use. No claim of
completed least-privilege enforcement is made here.

- Host recorded nine URL files in secure credential storage with mode `0600`.
  They are not present in the operator's local secure-drop view; do not copy them
  into this repo, issue text, or logs.
- Repo secret `PLANETSCALE_MIGRATOR_URL_STAGING` set at `18:59:43Z`;
  name/update-time metadata independently re-listed.
- **Production migrator secret intentionally not set.** Its deployment remains
  deferred to the gated production workflow. This is not a missing-credentials
  blocker and does not authorize a production migration.
- Host reported 7/7 tested NNTune roles passed `select 1` after the resize.
  Nine roles were created; this is not a claim that all nine were retested.
  The seven-role subset was not identified in the report; do not assume it covers
  every production runtime/read-only role. SQL role-matrix verification remains
  an application-use prerequisite.

### Hyperdrive bindings

| Config | ID / GitHub variable | Origin role | Intended binding |
|---|---|---|---|
| `nntunev2-staging` | `61c87403f119430bbc3a7f571df47002` / `HYPERDRIVE_ID_STAGING` | `nntunev2-runtime-staging` | staging app relational data |
| `nntunev2-prod` | `d38a229748f6452bb417c6d33d277a8d` / `HYPERDRIVE_ID_PROD` | `nntunev2-runtime-main` | prod app relational data |
| `nntunev2-mb` | `1c6460d5112448888031973c87f671bc` / `HYPERDRIVE_ID_MB` | `nntunev2-mb-ro-main` | shared read-oriented MB mirror |

There are **three** configs, not separate MB staging/prod configs. The MB origin
role choice is host-reported and still subject to the SQL role matrix above.
The MB import uses its migrator role, not this read-oriented binding.
Staging and prod MB reads share this one config and PS-10 origin: load, cache
behavior, and outages are coupled. The import also competes on that same MB
branch's connection/compute allowance, even though it uses a separate role.
The app staging/prod Hyperdrive configs target their own app branches, not MB.

All three configs independently GET-resolved on 2026-10-13; IDs match the repo
variables set at `18:59:44Z`. All currently report caching enabled and
`origin_connection_limit = 60`. Application owners must account for query-cache
freshness and branch connection limits before deploying or starting the import.
Creation-time origin connectivity was verified by the host executor; a metadata
GET is not an ongoing SQL connectivity check.

UNDO: during authorized decommissioning, unbind consumers and remove the three
Hyperdrive configs and corresponding repo variables, then revoke new roles and
remove DBs only under the backup/data-retention approval gate. Never delete
`main` data to reconcile docs. To undo the MB compute cost cut, the privileged
executor would PATCH the MB main branch changes endpoint with
`{"cluster_size":"PS_40_AWS_ARM"}`; this raises spend and is **not authorized**
by this runbook. Credentials/hosts remained unchanged by the resize.

## Environments matrix

Binding names are chosen in Worker configs; resource names must match this table.

| Concern / intended consumer | staging | prod |
|---|---|---|
| R2 audio / stream Worker | `nntunev2-audio-staging` | `nntunev2-audio-prod` |
| R2 artwork / MB Worker | `nntunev2-art-staging` | `nntunev2-art-prod` |
| R2 archive / reserved, no consumer | `nntunev2-archive-staging` | `nntunev2-archive-prod` |
| Ingest Queue | `nntunev2-ingest-staging` | `nntunev2-ingest-prod` |
| Config KV | `nntunev2-config-staging` | `nntunev2-config-prod` |
| Hostname | `v2-staging.nntune.com` (+ Access; deployment pending) | TBD at launch |
| App Hyperdrive variable | `HYPERDRIVE_ID_STAGING` | `HYPERDRIVE_ID_PROD` |
| App PlanetScale branch | `nntunev2` / `staging` | `nntunev2` / `main` |
| MB Hyperdrive variable | `HYPERDRIVE_ID_MB` (shared mirror) | `HYPERDRIVE_ID_MB` (shared mirror) |
| Migration secret | `PLANETSCALE_MIGRATOR_URL_STAGING` | intentionally deferred to gated workflow |
