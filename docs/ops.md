# NNTune v2 operations

Provisioned resource metadata for staging + prod, as of 2026-10-16.
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

### R2 buckets (core buckets created 2026-10-13; scratch metadata verified 2026-10-17)

Read-only Cloudflare R2 metadata GETs on 2026-10-17 confirmed both reported
scratch buckets exist and each has one enabled object-expiration rule:
`expire-ingest-scratch-after-7-days`, prefix `ingest-scratch/`, age
`604800` seconds (seven days), action `deleteObjectsTransition`. The staging
and production bucket names match the inventory below. No lifecycle rule on
either bucket expires objects outside that prefix. The observed archive-bucket
lifecycle is only the enabled `Default Multipart Abort Rule`, age `604800`
seconds, with no prefix condition; it aborts incomplete multipart uploads and
does not expire stored objects.

The provisioned layout is **one bucket per concern per environment**, not one
shared bucket. Object key prefixes describe cache scope *within* those buckets;
they do not replace the environment boundary. Worker configs and cache/MusicBrainz
ADRs must use the exact names below. There is no bucket named `nntunev2-art`.
The MB documentation/example references are reconciled here; remaining
integration prerequisites on sibling branches are:

| Owning PR | Required reconciliation |
|---|---|
| [#2](https://github.com/TogetherWeOwn/nntunev2/pull/2), cache ADR | Physical audio/art buckets are separate per environment; prefixes live inside them. |
| [#14](https://github.com/TogetherWeOwn/nntunev2/pull/14), scaffold | Replace placeholders only when activating the correct consumer bindings. |
| [#10](https://github.com/TogetherWeOwn/nntunev2/pull/10), ingest | The checked-in example still uses generic `nntune-ingest-jobs` and unsuffixed `nntune-ingest-dlq`, with its consumer commented out. Align producer/consumer queue names and the DLQ route to the per-environment inventory below before enabling a consumer. |
| `workers/mb/wrangler.toml.example`, MB art cache | The `ART_CACHE` template names only `nntunev2-art-staging`; the prod deploy must override it with `nntunev2-art-prod` when the binding is activated. |
| [#11](https://github.com/TogetherWeOwn/nntunev2/pull/11), E2E | Supply the two pending Access client secrets before enabling the staging URL; see the Access section. |

Their owners must coordinate these fixes at the next permitted review round and
recheck actual bindings before deployment. This runbook does not change those
branches or certify a deploy. Scratch storage is separate from archive buckets.
The two scratch buckets are isolated by environment, and their live lifecycle
rules were verified on 2026-10-17 as described above. Read-only Worker metadata
showed no active production NNTune Worker deployment; the active staging
`nntune-web-staging` version has no R2 bindings, and no ingest Worker is
currently deployed. Thus neither archive bucket is bound as scratch storage by
a deployed NNTune Worker. This check inspects Worker configuration only; it did
not enumerate bucket objects or rule out out-of-band S3/API clients. Do not use
archive buckets as scratch storage.

| Bucket | Env | Intended consumer |
|---|---|---|
| `nntunev2-audio-staging` | staging | stream Worker audio cache |
| `nntunev2-audio-prod` | prod | stream Worker audio cache |
| `nntunev2-art-staging` | staging | MusicBrainz Worker artwork cache |
| `nntunev2-art-prod` | prod | MusicBrainz Worker artwork cache |
| `nntunev2-archive-staging` | staging | reserved archive capacity; no Worker consumer configured |
| `nntunev2-archive-prod` | prod | reserved archive capacity; no Worker consumer configured |
| `nntunev2-ingest-scratch-staging` | staging | ingest repair scratch; live `ingest-scratch/` object expiry verified 2026-10-17 at 604800 seconds |
| `nntunev2-ingest-scratch-prod` | prod | ingest repair scratch; live `ingest-scratch/` object expiry verified 2026-10-17 at 604800 seconds |

The archive buckets were requested at provisioning time. They are not evidence
of an implemented archival pipeline and must not be bound to a Worker by default.
Do not create unsuffixed replacement buckets or delete these resources to reconcile
an example config; align the examples with this inventory instead.

UNDO (decommissioning reference, not an instruction to execute):
`DELETE /accounts/<acct>/r2/buckets/<name>`, one per bucket, only after authorized
consumer removal and data-retention checks. Never delete cached/archive data as
part of a documentation fix.

#### Audio-bucket R2 lifecycle and track-pointer TTL (reported 2026-10-19)

Reported R2 lifecycle rules per audio bucket, from checked-in manifests plus
prior observations in this runbook's R2 section above. The scratch 7-day
`ingest-scratch/` rule and the archive `Default Multipart Abort Rule` recorded
in `docs/ops.md` are the only live bucket-lifecycle observations in this
runbook, so no live bucket-lifecycle GET is recorded here for either audio
bucket:

| Bucket | Reported R2 lifecycle rules |
|---|---|
| `nntunev2-audio-staging` | none reported — no object-expiry or multipart-abort rule recorded for this bucket in `docs/ops.md` or `docs/cache-policy.md`; `workers/stream/wrangler.toml:47-49` declares only the staging `AUDIO_CACHE` binding |
| `nntunev2-audio-prod` | none reported — same evidence as staging; `workers/stream/wrangler.toml:34-36,59-61` declare only the prod `AUDIO_CACHE` bindings |

No `lifecycle` field appears in the checked-in Worker/app Wrangler manifests
(`workers/stream/wrangler.toml`, `workers/mb/wrangler.toml.example`,
`workers/repair/wrangler.toml`, `workers/ingest/wrangler.toml.example`,
`workers/api/wrangler.toml.example`, `apps/web/wrangler.toml`) — lifecycle is
Cloudflare-side, not a Wrangler field, per the audio bucket sources in
`docs/cache-policy.md` — and the only `lifecycle` string in those manifests is
the scratch-prefix cleanup comment at
`workers/ingest/wrangler.toml.example:129`. "None reported" records the
verified absence of any reported rule for these two audio buckets
(`workers/stream/wrangler.toml:47-49,59-61`); it does not assert the live
Cloudflare-side state, which remains unverified for the audio buckets.
Physical-bucket and binding alignment stays with the owning lanes per the
reconciliation table in `docs/ops.md` (R2 section above); this subsection
records lifecycle status only and changes no bucket, binding, TTL, or header
behavior.

Timed `track/<id>` pointer freshness is application behavior, distinct from R2
lifecycle per the TTL section of `docs/cache-policy.md`: entries serve fresh
for `CACHE_FRESH_TTL_SECS = 300` (`workers/stream/src/cache.rs:33`), then
stale for `CACHE_SWR_WINDOW_SECS = 3_600` (`workers/stream/src/cache.rs:38`)
while owing background revalidation, then hard-miss past 3,900 s of age
(`get_track_at` returns `None` past fresh + SWR at
`workers/stream/src/cache.rs:250-258`, within
`workers/stream/src/cache.rs:229-261`; `workers/stream/src/play.rs:502-527`
falls through to the miss path and re-fetches upstream;
`workers/stream/README.md:157-168` documents HIT, STALE, and MISS). Untimed
`put_track` entries never go stale (`workers/stream/src/cache.rs:127-147`
writes `stored_at_secs: None`, served fresh at
`workers/stream/src/cache.rs:246-247`).

### Queues (metadata GET-verified 2026-10-17)

Read-only Cloudflare Queues API list/get and consumer-list responses on
2026-10-17 confirmed the reported DLQ names and IDs. Both queues were created
2026-10-16. The API reported `settings.message_retention_period = 345600`
seconds (4 days) for each; this is the observed setting, not an assumption from
the default. Cloudflare documents four days as the default DLQ retention when
there is no active consumer ([DLQ documentation](https://developers.cloudflare.com/queues/configuration/dead-letter-queues/)); both API responses also reported zero consumers. A consumer-list GET for each queue returned an empty list.

| Queue | ID | Env | Observed retention | Consumers |
|---|---|---|---|---|
| `nntunev2-ingest-staging` | `c9e6afdea0ca487c8d20f799a052b045` | staging | 345600 seconds (4 days) | 0 |
| `nntunev2-ingest-prod` | `2bf7e1a0edd347c9aada2afbc0c24ae5` | prod | 345600 seconds (4 days) | 0 |
| `nntunev2-ingest-dlq-staging` | `e7ab1d4b284f44f094c5fe62f9bf96a5` | staging DLQ | 345600 seconds (4 days) | 0 |
| `nntunev2-ingest-dlq-prod` | `e9d0a9f5549249df9eebaa5c00951328` | prod DLQ | 345600 seconds (4 days) | 0 |

**Routing discrepancy; stop before configuration changes.** The only checked-in
ingest Wrangler configuration is `workers/ingest/wrangler.toml.example`. Its
producer and commented consumer use `nntune-ingest-jobs`; its DLQ producer and
commented `dead_letter_queue` use the unsuffixed `nntune-ingest-dlq`. The
consumer block remains commented, and this example does not distinguish staging
from production. These values do not match the environment-specific queue names
in the table above. No queue or message was changed or consumed; do not enable
or alter routing until the owning integration lane reconciles this discrepancy.

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
- **E2E wiring is pending:** [PR #11](https://github.com/TogetherWeOwn/nntunev2/pull/11)
  consumes `CF_ACCESS_CLIENT_ID` and `CF_ACCESS_CLIENT_SECRET`. Neither name is
  present in the repository-secret inventory checked on 2026-10-13. Referencing
  the QA token's resource ID above does not provide its client credentials.
  Before enabling `STAGING_BASE_URL` for that workflow, its owner must arrange
  authorized secure injection and verify both credentials are available to the
  job. Do not bypass Access, publish values, or rotate the shared token for this
  docs correction; credential distribution remains a separate gated operation.

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

The host executor's **2026-10-13 ~19:40Z consolidation receipt supersedes** the
original separate staging branch and the earlier PS-10/PS-40 allocations.
Sizing, billing, branch deletion, and SQL privilege tests below are host-reported,
not independently checked against PlanetScale by this documentation update.
Existing TWO databases were not touched.

- Organization: `togetherweown`; region: AWS `us-east-1`; Postgres `18.6` as
  recorded at initial provisioning.
- `nntunev2` / `main`: shared **PS-5 single-node** cluster. Staging now uses the
  PostgreSQL database **`nntunev2_staging`** on this branch, not a separate branch.
- The separate PlanetScale `staging` branch was deleted by the host executor.
  The host reports it was empty (zero tables) and required no data migration;
  this update did not independently inventory schema objects. The latest receipt records
  that removed PS-DEV branch as $5/month, correcting the earlier $0 statement.
- `nntunev2-mb` / `main`: separate MusicBrainz cluster, now reported as $5/month
  compute. The latest receipt does not specify its final SKU/replica count;
  do not carry forward the earlier PS-10 CPU/RAM/HA claims as current facts.
- Monthly cluster compute: **$10** ($5 app main, including staging, + $5 MB).
  Metered storage, backups, and egress are additional; $10 is not an all-in bill.
- The ~100 GB MB import remains a capacity target, not already-imported data.
  Initial storage was gp3 autoscale 10 GB–4 TB; verify current storage, connection
  allowance, and import performance on the reduced allocation before importing.
- **Launch remains gated:** prod will get its own clean PostgreSQL database on
  the main cluster, with resize/split assessed then. The current prod Hyperdrive
  still targets `postgres`; it is not evidence that the clean launch database
  has been created or approved for migration. Do not enable the production app
  binding or write application data to this current `postgres` target. Before
  launch use, the authorized workflow must create the clean database, apply and
  re-test its grants/isolation, and verify the prod config is repointed to it.

### Database roles and migrator wiring

The latest host receipt reports database-level app isolation: staging roles
cannot CONNECT to the prod database and prod roles cannot CONNECT to staging;
negative cross-connect tests returned `permission denied`. It also reports
runtime = CRUD without DDL, read-only = SELECT only, and migrator owns schema
`public`. These are **host-tested app-role properties**, not fresh SQL tests by
this documentation update, nor evidence of separate compute/failure domains.

- Staging uses new runtime/migrator roles for `nntunev2_staging` on `main`.
  The old staging-branch role names and URL-file inventory are superseded;
  use the authorized secure wiring, not credentials from the deleted branch.
  Exact replacement role names were not included in the receipt, and this
  update does not attest to old credential revocation or secure-store retirement.
- Repo secret `PLANETSCALE_MIGRATOR_URL_STAGING` was updated to the new migrator
  URL. GitHub name/update-time metadata was independently re-listed:
  **2026-10-13 `19:37:40Z`**. No secret value was read.
- **Production migrator secret intentionally not set.** Its deployment remains
  deferred to the gated production workflow. This is not a missing-credentials
  blocker and does not authorize a production migration or creation of the
  clean launch database.
- The migration owner must preserve and re-test the role matrix as schemas and
  tables are added and whenever the clean launch database is created, including
  default privileges, runtime DDL denial, read-only
  write denial, and bidirectional cross-database CONNECT denial. The earlier
  seven-role `select 1` probe predates consolidation and is not current proof.
- MB migrator/runtime/read-only roles were provisioned separately. The latest
  app-isolation receipt does not prove MB read-only enforcement. On 2026-10-17,
  this agent attempted to locate an authorized read-only SQL executor through the
  Paperclip connection catalog; both searches timed out. No direct SQL session or
  host-side verification receipt was available in this workspace. Accordingly,
  **the MB app role's live grants remain unverified**; the older receipt's claim
  that the MB binding uses a read-only role is host-reported only.
- Exact checks still required against the MB database using the authorized
  read-only verification path: establish the database/schema and effective role
  identity without exposing connection details; run a representative `SELECT`
  against a required mirror table; inspect effective table/schema privileges and
  default privileges; and confirm representative `INSERT`/`UPDATE`/`DELETE`,
  sequence-write, and schema/table DDL privileges are denied. A denial must be
  established from the effective role's actual privilege result or a safely
  rolled-back, non-mutating authorization check; do not attempt a write that
  could change data. No check above was run here, so neither SELECT success nor
  write/DDL denial is claimed.
- Authorized follow-up: the Operator should use the existing secure host-side
  read-only verification path to run and return the checks above, reporting only
  date, environment/database and role labels, check expressions/categories, and
  outcomes. Do not send or record URLs, passwords, tokens, hostnames, or personal
  login identities; do not alter grants or roles. Update this evidence only from
  that direct result, keeping host-reported statements clearly distinguished.

### Hyperdrive bindings

| Config | ID / GitHub variable | Current PostgreSQL database | Intended binding |
|---|---|---|---|
| `nntunev2-staging` | `61c87403f119430bbc3a7f571df47002` / `HYPERDRIVE_ID_STAGING` | `nntunev2_staging` on app `main` | staging app, new runtime role (host-reported) |
| `nntunev2-prod` | `d38a229748f6452bb417c6d33d277a8d` / `HYPERDRIVE_ID_PROD` | `postgres` on app `main` | reserved prod app; clean launch DB deferred |
| `nntunev2-mb` | `1c6460d5112448888031973c87f671bc` / `HYPERDRIVE_ID_MB` | `postgres` on MB `main` | shared read-oriented MB mirror |

All three configs independently GET-resolved on 2026-10-13 after consolidation.
IDs are unchanged and match the repo variables set at `18:59:44Z`. Staging's
config was modified at `19:37:25Z`; its database is `nntunev2_staging`. Prod and MB
metadata retain their original `18:58:55Z` and `18:58:57Z` modification times. Fresh
metadata confirms app staging/prod share an origin host but use different origin
users and databases. A metadata GET is not an ongoing SQL connectivity or
privilege test; no hostnames, usernames, or credentials need publishing here.

There are **three** configs, not separate MB staging/prod configs. The MB origin
uses its read-only role per the original host receipt, subject to the MB grant
check above. The MB import uses its migrator role, not this read-oriented binding.
Staging and prod MB reads share one MB config/cluster: load, cache behavior, and
outages are coupled. The import competes for that same MB connection/compute
allowance even though it uses a separate role.

All three configs still report caching enabled and `origin_connection_limit = 60`.
The two app pools now share one PS-5 single-node cluster, alongside direct
migrator/admin connections; they do **not** have independent branch allowances.
Before deploy/import, the owning lanes must validate the combined connection
budget and cache freshness against the actual origin limits. Database CONNECT
isolation does not isolate CPU, connections, storage/I/O, maintenance, or outages, and the
app single-node allocation must not be described as highly available.

App auth reads require cache-disabled Hyperdrive: the API reads sessions,
credentials, users, challenges and vaults with plain `SELECT`s through the
`HYPERDRIVE` binding (`workers/api/src/worker_entry.rs`), which Hyperdrive
caches by default (60s + 15s stale-while-revalidate, never invalidated by
writes). The staging/prod app configs (`HYPERDRIVE_ID_STAGING`,
`HYPERDRIVE_ID_PROD`) MUST be updated with
`wrangler hyperdrive update <id> --caching-disabled` before the API serves
traffic; the MB mirror config may stay cached. Deploy checklist (with
`CLOUDFLARE_API_TOKEN`/`CLOUDFLARE_ACCOUNT_ID` exported):
`workers/api/check_hyperdrive_cache.sh --config-id $HYPERDRIVE_ID_STAGING`
(and prod) must report `caching.disabled=true`. The gate fails closed
(exit 3) without credentials; `--allow-skip` is only for secret-less CI
and MUST NOT be used in real deploys. The Worker additionally refuses to
serve traffic without `HYPERDRIVE_CACHING_DISABLED=1` for that env — set
it only after the gate passes for that Hyperdrive config (runtime
attestation, fail-closed generic 500 otherwise).
Template and contract:
`workers/api/wrangler.toml.example`, `workers/api/README.md`.

UNDO: consolidation is not reversed by reusing the deleted branch's URLs or by
blindly restoring the old PS-40 resize command. The authorized host executor's
consolidation receipt records per-step rollback; consult it with current data,
backup/retention requirements, credential-version fences, and spend authority.
No resize, split, role change, branch recreation, deletion, or credential rotation
is authorized here. Decommissioning would require consumer removal and the
applicable data-retention gate before deleting configs, roles, or databases.

## Cost basis

Public list prices and cost-as-function-of-usage formulas for Workers,
R2, KV, Queues, Analytics Engine and PlanetScale Postgres are in
[`docs/cost-basis.md`](cost-basis.md) (checked 2026-10-17). The first
measured staging bill (2026-10-13 → 2026-10-17) is recorded in section 10
of that document; plug later staging volumes into the same formulas for
updated estimates.

## Environments matrix

Binding names are chosen in Worker configs; resource names must match this table.

| Concern / intended consumer | staging | prod |
|---|---|---|
| R2 audio / stream Worker | `nntunev2-audio-staging` | `nntunev2-audio-prod` |
| R2 artwork / MB Worker | `nntunev2-art-staging` | `nntunev2-art-prod` |
| R2 archive / reserved, no consumer | `nntunev2-archive-staging` | `nntunev2-archive-prod` |
| Ingest scratch (bucket existence and 7-day lifecycle verified 2026-10-17) | `nntunev2-ingest-scratch-staging` (`ingest-scratch/`; 604800-second expiry) | `nntunev2-ingest-scratch-prod` (`ingest-scratch/`; 604800-second expiry) |
| Ingest Queue | `nntunev2-ingest-staging` | `nntunev2-ingest-prod` |
| Ingest DLQ / no consumer observed (Cloudflare metadata GET 2026-10-17; 345600 seconds observed, matching documented 4-day default with no active consumer) | `nntunev2-ingest-dlq-staging` (`e7ab1d4b284f44f094c5fe62f9bf96a5`) | `nntunev2-ingest-dlq-prod` (`e9d0a9f5549249df9eebaa5c00951328`) |
| Config KV | `nntunev2-config-staging` | `nntunev2-config-prod` |
| Stream NNTP pool (DO class) | `NntpPool` | `NntpPool` |
| Repair jobs (DO class) | `RepairContainer` | `RepairContainer` |
| Stream meter (AE dataset) | `nntune_meter` | `nntune_meter` |
| Ingest meter (AE dataset) | `nntune_ingest_meter` | `nntune_ingest_meter` |
| Stream service (ingest `NNTP_POOL` cross-script target) | `nntune-stream-staging` | `nntune-stream` |
| Repair service (ingest `REPAIR_CONTAINER` target) | `nntune-repair-staging` | `nntune-repair` |
| MB service (ingest `MB_API` target; Wrangler default-suffixed, no per-env `name` committed) | `nntune-mb-staging` | `nntune-mb-production` |
| Hostname | `v2-staging.nntune.com` (+ Access; deployment pending) | TBD at launch |
| App Hyperdrive variable | `HYPERDRIVE_ID_STAGING` | `HYPERDRIVE_ID_PROD` |
| App PlanetScale branch / PostgreSQL database | `nntunev2` / `main` / `nntunev2_staging` | same `main` cluster / currently `postgres`; clean launch DB deferred |
| MB Hyperdrive variable | `HYPERDRIVE_ID_MB` (shared mirror) | `HYPERDRIVE_ID_MB` (shared mirror) |
| Migration secret | `PLANETSCALE_MIGRATOR_URL_STAGING` | intentionally deferred to gated workflow |

## Binding parity check

CI job `binding-parity` (part of `ci-ok`) discovers Wrangler TOML, JSON and
JSONC configs recursively under `workers/` and `apps/`, then diffs mapped TOML
configs against this inventory for staging and prod. JSON/JSONC configs are
reported when unmapped and fail closed if mapped until parser support is added.
Required bindings per Worker are mapped
in `.github/scripts/binding_manifest.toml`, and
`python3 .github/scripts/binding_parity.py` fails on a missing binding,
staging/prod skew, or a resource name absent from the matrix or tables
above, naming the Worker, environment and binding. Resource-name drift
that predates the value-alignment work (see the reconciliation table
near the top of this document) is recorded as exact-match
`[[known_drift]]` manifest entries; delete each entry when its
wrangler value is aligned — the check fails on a stale entry. Changing
the binding values themselves, and KV key parity, are separate lanes
and out of scope for this check.

## Worker bindings

Intended consumer bindings per environment. Resource names must match the
tables above. Where a committed config still carries a stale name, the
reconciliation table in the R2 section governs: align the config to this
table, never the reverse. IDs below are non-credential binding metadata;
no database URLs or secret values are recorded here. Non-storage bindings
(Durable Object, Analytics Engine, service) are out of scope of
this table; they are inventoried with per-environment values and committed
config sources in the Non-storage bindings inventory below.

| Worker (config source) | Binding | Type | staging | prod | ID / variable |
|---|---|---|---|---|---|
| `stream` (`workers/stream/wrangler.toml`) | `AUDIO_CACHE` | R2 | `nntunev2-audio-staging` | `nntunev2-audio-prod` | name-bound, no ID |
| `mb` (`workers/mb/wrangler.toml.example`) | `ART_CACHE` | R2 | `nntunev2-art-staging` | `nntunev2-art-prod` | name-bound, no ID |
| `mb` | `MB_DB` | Hyperdrive | `nntunev2-mb` (shared mirror) | same | `1c6460d5112448888031973c87f671bc` / `HYPERDRIVE_ID_MB` |
| `api` (`workers/api/wrangler.toml.example`) | `HYPERDRIVE` | Hyperdrive | `nntunev2-staging` | reserved: do not enable; clean launch DB deferred | staging `61c87403f119430bbc3a7f571df47002` / `HYPERDRIVE_ID_STAGING`; prod `d38a229748f6452bb417c6d33d277a8d` / `HYPERDRIVE_ID_PROD` |
| `ingest` (`workers/ingest/wrangler.toml.example`) | `LIBRARY_DB` | Hyperdrive | intended app `nntunev2-staging` | reserved: do not enable; clean launch DB deferred | same IDs / variables as `api`; prod must remain disabled until the clean launch DB is ready |
| `ingest` | `INGEST_QUEUE` (producer) | Queue | `nntunev2-ingest-staging` | `nntunev2-ingest-prod` | staging `c9e6afdea0ca487c8d20f799a052b045`; prod `2bf7e1a0edd347c9aada2afbc0c24ae5` |
| `mb` / `ingest` (at activation) | `MB_CACHE` / `INGEST_STATE` / `INGEST_PAYLOADS` | KV | reserved `nntunev2-config-staging` | reserved `nntunev2-config-prod` | staging `1d4f21508a214ebfa8ff7b594989d6f6`; prod `9664ceed71464521bb6baae2d9374006`; assigned at activation, never by default |

Known config drift (tracked in the R2 reconciliation table, fixed by the
owning lanes, not here): `workers/ingest/wrangler.toml.example` still names
`nntune-ingest-jobs` for the producer and commented consumer, and the unsuffixed
`nntune-ingest-dlq` for the DLQ producer and commented `dead_letter_queue`.
Neither environment is represented by these values. Align producer, consumer,
and DLQ names to the per-environment inventory before enabling the configs. Do
not add a nested `[[queues.consumers.dead_letter_queue]]` table: Wrangler
rejects that shape, so it will not configure dead-letter routing.

### Non-storage bindings inventory (DO, Analytics Engine, service)

Every non-storage binding with its owning worker, per-environment value,
and committed config source (file with line ranges; code constants where
they pin the name). No secret values and no uncommitted IDs are recorded
here: Analytics Engine datasets and service/DO names below are all
committed in the cited Wrangler configs. The Environments matrix above
carries the same per-environment values; this table adds the owning
worker and the exact config source for each row.

| Binding | Type | Owning worker | staging | prod | Committed config source |
|---|---|---|---|---|---|
| `NNTP_POOL` (class declaration) | Durable Object | `stream` | class `NntpPool` | class `NntpPool` | `workers/stream/wrangler.toml:22-24` (top-level), `:44-46` (staging), `:56-58` (production); migration tag `v1` `:26-28`; code `workers/stream/src/runtime.rs:44`, `workers/stream/src/routes.rs:14` |
| `NNTP_POOL` (cross-script use) | Durable Object | `ingest` → `stream` | script `nntune-stream-staging` | script `nntune-stream` | `workers/ingest/wrangler.toml.example:105-112` (pins the prod `script_name`; per-env resolution follows the stream worker names in `workers/stream/wrangler.toml:43,55`) |
| `METER` | Analytics Engine | `stream` | dataset `nntune_meter` | dataset `nntune_meter` | `workers/stream/wrangler.toml:38-40` (top-level), `:50-52` (staging), `:62-64` (production); code `workers/stream/src/observability.rs:23` |
| `INGEST_METER` | Analytics Engine | `ingest` | dataset `nntune_ingest_meter` | dataset `nntune_ingest_meter` | `workers/ingest/wrangler.toml.example:124-127`; code `workers/ingest/src/metering.rs:16-18` |
| `REPAIR_CONTAINER` | service | `ingest` → `repair` | service `nntune-repair-staging` | service `nntune-repair` | `workers/ingest/wrangler.toml.example:129-133` (pins the prod service name); target names `workers/repair/wrangler.toml:15` (prod default), `:50` (staging), `:69` (prod); contract note `:3-6`; code `workers/ingest/src/repair.rs:19` |
| `MB_API` | service | `ingest` → `mb` | service `nntune-mb-staging` | service `nntune-mb-production` | `workers/ingest/wrangler.toml.example:135-140` (single `service = "nntune-mb"`, needs env split); target top-level `name = "nntune-mb"` `workers/mb/wrangler.toml.example:3` with no per-env `name` in `:28-39` (staging) or `:41-52` (production), so Wrangler publishes `nntune-mb-staging` / `nntune-mb-production`; code `workers/ingest/src/matching.rs:27` |
| `REPAIR_JOBS` | Durable Object | `repair` | class `RepairContainer` | class `RepairContainer` | `workers/repair/wrangler.toml:33-35` (top-level), `:61-63` (staging), `:80-82` (production); migration tag `v1` `:37-39`; Container class/image/sizing `:23-31`, `:52-60`, `:71-79`; code `workers/repair/src/runtime.rs:122` |

Notes, all referencing (not changing) owning-lane state:

- The committed ingest example carries single (non-env-split) values for
  its cross-script and service targets (`script_name = "nntune-stream"`,
  `service = "nntune-repair"`, `service = "nntune-mb"`). Stream and repair
  have explicit per-env `name` overrides committed in their configs, so
  their per-environment resolution follows the target workers' committed
  names above; mb has no per-env `name` (`workers/mb/wrangler.toml.example:3`
  only, none in `:28-39` or `:41-52`), so its per-environment values above
  are the Wrangler default-suffixed names and ingest's single
  `service = "nntune-mb"` needs the same env split. Wiring the env split is
  owned by the integration lane, like the ingest queue-name drift recorded
  in the R2 reconciliation table and the paragraph above.
- The repair Worker is not yet deployed: its config header
  (`workers/repair/wrangler.toml:11-13`) requires Cloudflare Containers on
  the existing plan (no new spend) with deploy as a later operator action.
  Until then the `REPAIR_CONTAINER` target names are reserved, not live —
  and ingest cannot deploy with the committed example until the repair
  target exists, because Cloudflare fails a deploy whose service-binding
  target does not exist.
- Deploy order is stream, mb, and repair (once Containers are enabled)
  before ingest. Ingest reaches the per-user `NntpPool` object through the
  cross-script `NNTP_POOL` binding (`workers/stream/wrangler.toml:4-8`),
  and it declares unconditional `[[services]]` bindings to `nntune-repair`
  and `nntune-mb` (`workers/ingest/wrangler.toml.example:129-140`; mb-first
  is also stated at `:135-137`). Ingest cannot deploy with the committed
  example until those service targets exist.

## Staging promotion checklist

Promotion is `main` HEAD → staging; production promotion is a later gated
lane. Complete every box on the exact head being promoted.

### Production seed precondition (one-time)

The production lane (`.github/workflows/deploy-production.yml`) promotes
through the shared gradual-promote action, whose automatic-rollback target is
the latest production deployment and which refuses unless that deployment is
a single Worker version at 100%. There is currently no active production
NNTune Worker deployment, so the very first promotion cannot satisfy that
precondition on its own. Before the first promotion, an authorized executor
must seed production once: make one full deploy of the web frontend to
production (for example `wrangler deploy --env production` from the launch
SHA, run from `apps/web`) with the launch custom domain (`PRODUCTION_DOMAIN`)
attached, leaving a single 100% deployment. The lane enforces this before
uploading anything: its seed step lists production deployments and refuses
with these instructions when no single-version 100% deployment exists, so a
first run fails with guidance instead of orphaning an uploaded version inside
the promotion action. Re-dispatch the lane after seeding.

- [ ] Exact-head CI: `ci-ok`, `gitleaks` and `pr-lint` green on the promotion
  SHA, plus Paperclip Review 5/5 on that head (merge policy in
  `CONTRIBUTING.md`). A red required check is fixed in code or recorded as
  a flake/quarantine decision with a card link, never dismissed.
- [ ] Staging deploy evidence: record the successful push-triggered
  `deploy.yml` run for the promotion SHA, and require its `staging-web` job
  conclusion to be `success` (not `skipped`); verify staging health. This
  workflow deploys `apps/web` (`staging-web`) plus the api Worker staging
  lane (`staging-api`, staging only); it does not deploy stream, MB, ingest
  or repair Workers. `/api/health` returns exactly `{ok, version}` — the public
  package version, never a build identifier
  (`apps/web/src/routes/api/health/+server.ts`, pinned by
  `apps/web/tests/smoke.spec.ts`) — so a health response alone never proves
  which SHA is serving. Deployed-build identity lives on the dedicated
  surface `GET /api/build`, which returns exactly `{sha}`: the 40-char
  lowercase hex head baked at build time (`BUILD_SHA`/`GITHUB_SHA` via the
  `__BUILD_SHA__` define in `apps/web/vite.config.ts`), or the explicit
  string `"unknown"` when the build carried no identity
  (`apps/web/src/lib/build.ts`, `apps/web/src/routes/api/build/+server.ts`).
  Claim same-build evidence only when staging `/api/build` returns the
  promotion SHA itself (40-hex match; `"unknown"` is never evidence). The
  deploy owner replaces the placeholder `100::` record through a Workers
  custom domain.
- [ ] Staging E2E: `deploy.yml` sequences it — its `post-deploy-e2e` job
  calls `e2e-staging.yml` (reusable workflow) only after the same run's
  `staging-web` job succeeds (`needs`, no `always()`), so a skipped/failed
  deploy cannot produce a full-journey PASS and no separate dispatch is
  needed or accepted as evidence. Scheduled/manual `e2e-staging` runs are
  synthetic monitoring only (the full-journey job is skipped there).
  Record the deploy run and the `post-deploy-e2e` result, and call it
  same-build evidence only when staging `GET /api/build` returns the
  deploy SHA itself (a source SHA alone is not build identity; see the
  box above).
  Pre-configuration skips do not count once staging is configured; the
  workflow fails closed without both Access client credentials.
- [ ] Access intact: the `NNTune v2 staging` Access app, its owner-OTP allow
  policy and the QA service-token policy still gate `v2-staging.nntune.com`.
  No bypass, no shared-token rotation, and both `CF_ACCESS_CLIENT_*`
  credentials verified available to the job through authorized injection
  (names only, never values).
- [ ] Data plane guarded: no production migration, no clean-launch-database
  creation, and no resize, split, role change, or credential rotation as part
  of promotion. Staging serves `nntunev2_staging` through its runtime roles;
  the prod binding stays reserved.
- [ ] Rollback pointer recorded: record the last good staging SHA and its
  successful push-triggered `deploy.yml` run, with `staging-web` conclusion
  `success` (not `skipped`), before promoting. To accept rollback evidence,
  also require the rollback rerun's `staging-web` job conclusion to be `success`
  (not `skipped`); overall workflow success and staging health alone are
  insufficient. To roll back `apps/web`, open that Actions run and choose
  **Re-run all jobs**; do not use `workflow_dispatch` on the old SHA because
  `staging-web` is push-only and will be skipped. Verify the rerun's
  `staging-web` success and staging health. This workflow does not
  deploy Workers, which need their own rollback evidence. Staging rollback is
  a redeploy, never a data restore; retain the UNDO blocks and consolidation
  receipt for infrastructure-specific rollback.

## Future Worker deploy lanes: required rollback evidence

This section defines evidence only. It does not create a lane.

**Current claim boundary.** `.github/workflows/deploy.yml` deploys
`apps/web` (`staging-web` on push, `preview-web` on pull requests) plus the
api Worker staging lane (`staging-api` on push, staging only, skipped until
the Operator wires the staging variables). No other tracked workflow
deploys the backend Workers under `workers/` (`ingest`, `stream`, `mb`,
`repair`), and
`.github/workflows/e2e-staging.yml` exercises the staging web URL, not a
Worker deploy or rollback. Worker source and Wrangler configs existing in
the repo are not deployment. Therefore **no backend-Worker deploy,
rollback, activation, or staging acceptance is currently claimed**, for any
Worker in any environment, until that Worker's lane exists and its gates
pass on the exact head being promoted — for the api Worker that means a
`staging-api` job conclusion `success` (not `skipped`) on the promotion
run. The web checklist above is unchanged by this section and remains in
force alongside the per-Worker evidence table below.

**Evidence a future Worker staging lane must retain**, per Worker and per
environment, linked from that promotion's deploy record before rollback can
be called accepted:

| # | Retain | Acceptance bar |
|---|---|---|
| 1 | Deployed Worker and version | Worker (script) name and the version reported by the deploy tooling for the version now serving, plus the environment deployed to. |
| 2 | Source revision | The exact commit SHA that version was built from. Where the Worker exposes a deployed-build identity, record it and require it to match this SHA; without one, do not claim same-build evidence (the web checklist above verifies this via `GET /api/build`; a Worker with no such surface cannot claim it). |
| 3 | Deploy run | URL of the workflow run (or equivalent executed deploy record) that produced item 1, with the Worker deploy job's conclusion `success`. A `skipped` job, a skipped gate, or overall workflow success without that job succeeding is not deploy evidence. Any pre-deploy gate the lane requires — for the API Worker, the Hyperdrive cache-disabled check described in `deploy.yml` — records its pass/fail result here, by name and outcome only. |
| 4 | Prior known-good version | Recorded **before** the new deploy: the previously serving Worker's name, version and source SHA, its own successful deploy run (item 3 bar), and its last passing health result. A first deploy has no prior known-good version, so rollback acceptance cannot be claimed for it; removing or disabling a first deployment is a separate gated action, not defined here. |
| 5 | Rollback action and run | Which mechanism was used (redeploy of the prior known-good source revision, or activation of the prior version, whichever the lane implements), the target it restored (item 4's version and SHA), and the URL of the rollback run with the rollback/deploy job's conclusion `success` (not `skipped`). |
| 6 | Post-rollback health result | The lane's health/readiness check for that Worker, run after item 5, with its result and run URL recorded. Where a deployed-build identity exists, it must match the rollback target; a green rollback run or a bare `ok` response without that identity does not by itself prove the prior version is serving. |

A Worker rollback restores Worker code only. If the deploy being rolled
back also changed bindings, Durable Object migrations, queue consumers or
routing, database migrations, or stored data, the evidence is incomplete
until it also states, for each such change, that the restored version is
compatible with it or records that change's own rollback. Rollback is never
a data restore.

Evidence records contain names, version identifiers, commit SHAs, and run
or health-check URLs only. Do not record account IDs, tokens, credentials,
database URLs, environment values, or personal login identities in them.
