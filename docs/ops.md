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
| [#10](https://github.com/TogetherWeOwn/nntunev2/pull/10), ingest | The real `workers/ingest/wrangler.toml` now uses the per-environment queue names with a live `[[queues.consumers]]` stanza in every environment. Do not enable the staging lane until the runbook gate clears (TWO-1660 search intake); see "Operator runbook: enabling staging-mb / staging-ingest" below. |
| `workers/mb/wrangler.toml`, MB art cache | Staging binds `nntunev2-art-staging`; the prod section still names the staging bucket on purpose — the prod override to `nntunev2-art-prod` is owned by the MB art-cache activation (`[[known_drift]]` in `.github/scripts/binding_manifest.toml`). |
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
(`workers/stream/wrangler.toml`, `workers/mb/wrangler.toml`,
`workers/repair/wrangler.toml`, `workers/transcode/wrangler.toml`,
`workers/ingest/wrangler.toml`,
`workers/api/wrangler.toml.example`, `apps/web/wrangler.toml`) — lifecycle is
Cloudflare-side, not a Wrangler field, per the audio bucket sources in
`docs/cache-policy.md` — and the only `lifecycle` string in those manifests is
the scratch-prefix cleanup comment at
`workers/ingest/wrangler.toml:117-123`. "None reported" records the
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
`workers/stream/src/cache.rs:312-316`, within
`workers/stream/src/cache.rs:287-319`; `workers/stream/src/play.rs:528`
falls through to the miss path and re-fetches upstream;
`workers/stream/README.md:157-168` documents HIT, STALE, and MISS). Untimed
`put_track` entries never go stale (`workers/stream/src/cache.rs:175-199`
writes `stored_at_secs: None`, served fresh at
`workers/stream/src/cache.rs:305`).

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

**Routing alignment; the enablement gate is the runbook, not the names.** The
real `workers/ingest/wrangler.toml` uses the per-environment queue names in
the table above, with a live `[[queues.consumers]]` stanza (plus matching
`dead_letter_queue`) in every environment — the consumer goes live the moment
its environment deploys. No queue or message was changed or consumed; do not
enable the staging lane until the "Operator runbook: enabling staging-mb /
staging-ingest" gate below clears (TWO-1660 search intake).

UNDO: API `DELETE /accounts/<acct>/queues/<queue_id>` after authorized
consumer removal and pending-message checks.

### KV namespaces (created 2026-10-13)

| Namespace | ID | Env |
|---|---|---|
| `nntunev2-config-staging` | `1d4f21508a214ebfa8ff7b594989d6f6` | staging |
| `nntunev2-config-prod` | `9664ceed71464521bb6baae2d9374006` | prod |

Shared use: each namespace above backs three bindings (`INGEST_STATE`,
`INGEST_PAYLOADS`, `MB_CACHE`) with disjoint key prefixes
(`ingest:checkpoint:`/`ingest:verify:`, `nzb:sha256:`, `mb:lookup:`/
`mb:release-recordings:`); see the Worker bindings KV row. No separate
namespaces are provisioned; a future split keeps binding names unchanged.

UNDO: API `DELETE /accounts/<acct>/storage/kv/namespaces/<id>` after authorized
consumer removal and data-retention checks.

### DNS (zone `nntune.com`, id `97e268d5c1d540d42545ed8ab386b9c4`)

- `staging.nntune.com` → `AAAA 100::`, proxied, TTL auto (placeholder;
  created by the host as part of the 2026-10-21 cutover).
  Placeholder address (`100::` is reserved discard space): it routes the
  hostname to the edge so the Worker routes below serve it. The
  placeholder stays — routes need the hostname to resolve to the edge,
  unlike a Workers custom domain that would replace it. Provisioning
  does not establish that the application is deployed.
- Staging route split (same host by design: the session cookie is
  SameSite=Lax, so the frontend and the API must be same-site over
  HTTPS): the web Worker owns the `staging.nntune.com/*` catch-all
  (`apps/web/wrangler.toml`) and the api Worker owns the more specific
  `staging.nntune.com/v1/*`, `staging.nntune.com/healthz` and
  `staging.nntune.com/api/test-codes` routes (generated by the
  `staging-api` lane in
  `.github/workflows/deploy.yml`); the most specific route wins. The
  Access app below gates the hostname, so both Workers sit behind it.
- `v2-staging.nntune.com` → `AAAA 100::`, proxied, TTL auto.
  Record ID `97670fbad3e38106c28f71d1df89175e`.
  Superseded staging hostname: delete this record once staging is
  verified serving on `staging.nntune.com` behind Access.
- v1 retired 2026-10-21: the old v1 `staging.nntune.com` records
  (`A 135.148.42.223`, `AAAA`) were deleted and v2 replaces v1 on this
  hostname now. The v1 service is not in use; there is nothing to
  preserve.
- Prod hostname `nntune.com` (decided 2026-10-21, repo variable
  `PRODUCTION_DOMAIN`; no record yet): the apex attaches to the web
  prod Worker after the staging end-to-end run passes on the new
  hostname, with `www` redirecting to the apex (see the production
  seed precondition).

UNDO: API `DELETE /zones/<zone>/dns_records/<record_id>` for the
superseded `v2-staging` placeholder only, after cutover verification;
never delete the live `staging.nntune.com` placeholder or any later
serving record.

### Cloudflare Access (staging)

- App: `NNTune v2 staging (nntunev2-staging)`, self-hosted.
- App ID: `785bf815-f579-4768-a1d2-4de7ce19d086`.
- Domain: `staging.nntune.com` (re-pointed from `v2-staging.nntune.com`
  in the 2026-10-21 cutover; app ID and both policies unchanged);
  session duration `24h`.
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
  present in the repository-secret inventory checked on 2026-10-13
  (re-checked 2026-10-21: still absent, and `STAGING_BASE_URL` plus
  `QA_AUTH_TOKEN` are likewise unset — names only, never values). Referencing
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
- **MusicBrainz lifecycle, updated 2026-11-07:** the former `nntunev2-mb`
  cluster was deleted on 2026-11-06. Its small test seed was not a completed
  MusicBrainz import. App databases and production were untouched. Its MB
  Hyperdrive config was deleted and `HYPERDRIVE_ID_MB` removed; the cleanup
  receipt reports no environment override. Never restore the retired config ID.
- After the snapshot alternative failed its interactive latency criterion,
  the owner permitted a conditional replacement: **PS-5 only, ≤$10/month all-in,
  curated CC0 10-table subset, one branch, no development branch or extra
  replicas**. The 2026-11-07 setup receipt reports one main branch, zero replicas
  and a 50 GiB storage auto-grow cap. It reports no import, new Hyperdrive
  binding, variable restoration or consumer enablement. Provisioning is not
  import completion, serving readiness or runtime activation.
- The earlier **$10 combined monthly compute** statement is historical, not a
  current bill. The separate app database has a reported $5/month compute line;
  it is not the replacement mirror's invoice. Mirror estimates of $6.9–8.8/month
  remain estimates. A storage cap does not cap backups, transfer or every
  billable dimension. Record the mirror's first actual invoice and all-in total.
  A larger SKU requires measured PS-5 latency failure and a fresh spend decision.
- The replacement import plan is one manual trusted-host CC0 ETL → validated
  COPY run, with TLS, least privilege and elapsed-time/row-count evidence.
  Native validator source/digest/architecture/ABI provenance and security/release
  acceptance remain prerequisites; no unvalidated COPY or Container fallback.
  The old ~100 GB capacity target and synthetic-fixture-only recreation recipe
  are not this replacement plan. Runtime activation is separately gated; see
  [ADR 0007's amendment](adr/0007-mb-mirror.md#2026-11-07-amendment-retirement-and-conditional-bounded-replacement).
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
- MB migrator/runtime/read-only roles were originally provisioned separately.
  Those receipts predate the former cluster's deletion and do not attest to
  replacement roles. **Replacement MB grants remain unverified here**; neither
  app-isolation results nor the old read-only binding claim prove them. Apply
  the following checks to the replacement through its authorized evidence path
  before import or activation.
- Exact checks still required against the MB database using the authorized
  read-only verification path: establish the database/schema and effective role
  identity without exposing connection details; run a representative `SELECT`
  against a required mirror table; inspect effective table/schema privileges and
  default privileges; and confirm representative `INSERT`/`UPDATE`/`DELETE`,
  sequence-write, and schema/table DDL privileges are denied. A denial must be
  established from the effective role's actual privilege result or a safely
  rolled-back, non-mutating authorization check; do not attempt a write that
  could change data. Sequence probes are not non-mutating: `nextval` consumes
  values even when the surrounding transaction rolls back, so sequence-write
  denial must come from privilege inspection (for example
  `has_sequence_privilege`), never from a rolled-back `nextval` probe.
  No check above was run here, so neither SELECT success nor
  write/DDL denial is claimed.
- Authorized follow-up: the Operator should use the existing secure host-side
  read-only verification path to run and return the checks above, reporting only
  date, environment/database and role labels, check expressions/categories, and
  outcomes. Do not send or record URLs, passwords, tokens, hostnames, or personal
  login identities; do not alter grants or roles. Update this evidence only from
  that direct result, keeping host-reported statements clearly distinguished.

### Staging migrations

Nothing in CI applies `migrations/*.sql`; staging migrations are applied
out-of-band **before** merge, by the migrator role, after board approval for
the DDL. Applies go through `.github/scripts/apply_migration.py`, which
verifies the file bytes against `migrations/manifest.toml` and writes a
`public.schema_migrations(file, sha256, applied_at, applied_by)` row **in
the same transaction** as the migration effects. A single exact outer
`BEGIN; ... COMMIT;` envelope is validated lexically and removed only
in memory: its boundary commands are never executed, and the receipt
still hashes the complete original file. Strings, quoted identifiers,
nested comments and dollar-quoted bodies are distinguished from SQL
boundaries. Nested/multiple/early boundaries, other transaction forms
(including savepoints, rollback and chaining), psql commands, malformed
quotes/comments and unsupported lexical forms refuse before connecting.
`--record-only` does not bypass this check and is **not** a workaround
for unsupported files or a way to certify unexecuted DDL; it remains
only for separately verified, authorized historical backfill.
Every invocation requires `--expect-database <name>`
(matching the `DATABASE_URL` database and checked again inside the
transaction against `current_database()` — a URL pointed at the wrong
database creates and records nothing), and the apply refuses while an
earlier same-target manifest entry is unrecorded, so history cannot be
skipped or reordered. The tracking table's DDL pins its write
privileges to the table owner on every apply — its rows are deploy
proof — and the staging gate fails closed while any other role holds
a write grant on it. The staging deploy's read-only schema gate
(`.github/scripts/staging_api_schema_gate.py`) compares those rows with
the manifest's `app` entries — file name plus sha256 — and fails closed
on a missing, unlisted, sha-mismatched or double-applied migration. That
comparison, not PR text, is the source of truth for what staging has:
merging a migration that staging lacks turns `staging-api` red until it
is applied and recorded.

Every actual runner database child session overrides caller `PGOPTIONS`
with `search_path=public,pg_catalog`, `standard_conforming_strings=on`,
90-second statement/idle-in-transaction and 10-second lock timeouts.
Both sessions use `--no-psqlrc`, verified TLS off localhost and the
expected-database guard inside their transaction. The order is:

1. **Transaction A:** guarded, advisory-locked tracking-table bootstrap
   and table/column ACL enforcement; commits independently, including on an already-recorded
   invocation. These ACL writes are intentional, not a read-only no-op.
2. **Transaction B:** advisory lock, database/history/checksum guards,
   migration effects and append-only receipt, committed atomically per file.

Failure in B does not undo A, nor does a later file undo any successful
prefix of earlier files. The advisory lock covers one file transaction,
**not the whole migration window**. Operators must still arrange the
separately authorized exclusive window and quiesce writers/importers;
this runner does not provide that coordination. SQL effects that are not
transactional in PostgreSQL (for example sequence advancement or external
function side effects) are not made transactional by the runner. No
history deletion, automatic destructive rollback or all-files atomicity
is provided. If a timeout or disconnect leaves commit outcome unknown,
reconcile the exact original-file checksum receipt and expected catalog/
effects using the authorized evidence path before a retry; do not infer
rollback from a missing client acknowledgement or use record-only to
repair it. `--dry-run` proves composition only, never successful apply.
A code merge alone authorizes no live operation. A successor runner needs
fresh exact-source and deployment-evidence review; older runner evidence
is not transferable.

**Trust boundary.** The runner consumes trusted, reviewed migration bytes
bound to a manifest entry; it does not make untrusted SQL safe. The lexical
outer-envelope validator is neither an arbitrary-SQL sandbox nor proof that
session settings stay unchanged: the startup pin of the session settings
above is controlled for the A/B transactions, but opaque routine bodies
(for example function or `DO` bodies) are not generally inspected, so SQL
inside them can still run and alter session state. Approving manifest bytes
therefore means reviewing the SQL's actual behavior, including any session
changes, not just its envelope. PostgreSQL non-transactional effects stay
non-transactional, as described above.

Rows keep the name a migration was **applied under**. When a PR branch
renumbers a file that staging already has, the rename must be
**byte-identical** — change the filename only, not even the header
comment — and its manifest entry records the original name with the
entry's own sha as
`renamed_from = [{ file = "<old>", sha256 = "<same sha>" }]`; the gate
then recognises the old row as that entry's apply. The migrations gate
and staging loader reject an alias whose sha differs from the entry's.
The `migrations-staging` PR check additionally proves each new alias's
actual old Git blob has those bytes: copying today's hash onto a
historical name is not provenance.

Source lineage is separate. A file created, renamed, edited and renamed
again within a PR records every differing-byte edge on its **final**
manifest entry as structured `source_rename_history` tables:

```toml
# Within the final [[migration]] entry:
[[migration.source_rename_history]]
parent = "<full 40-character parent commit ID>"
commit = "<full 40-character child commit ID>"
old_path = "migrations/0001_example.sql"
new_path = "migrations/0002_example.sql"
old_sha256 = "<SHA-256 of the actual old blob at parent>"
new_sha256 = "<SHA-256 of the actual new blob at commit>"
```

These six fields describe source evidence only. They never satisfy an
applied migration or an ordered prior, enter backfill rows, or allow a
history-only name to be applied. Keep byte-identical `renamed_from`
identities separate; do not reclassify existing applied aliases. Both
alias and source metadata are append-only against the pinned common
merge base of the target and PR head. Target-only additions after divergence
are not PR deletions; no merge from the target is required to hide them.
Neither field permits editing/removing a base migration, changing its target,
renumbering it, bypassing numbering/search-path rules, or relaxing hashes.

The secretless PR gate loads its checker and its stdlib-only
`migration_history.py` import from a separate checkout of the event's full
trusted **base SHA**. The PR **head SHA** is checked out with full Git
history under a different root, used only as evidence through `--root`;
no PR setup, scripts or builds run in this job. Python isolated mode (`-I`)
excludes the working directory, `PYTHONPATH` and user-site imports, then
adds only the trusted checker directory. The checker runs outside both
checkouts with no API token in its environment; Git ignores global/system
configuration and disables hooks and filesystem-monitor commands. The
PR tree cannot supply Git's local configuration, which checkout creates.
The checked-out head supplies the manifest while pinned Git objects supply
SQL bytes and ancestry. A PR predating the checker can therefore be
validated without executing its Python files. Missing trusted checker code,
manifest, pinned Git objects or API evidence fails closed: there is no
fallback to PR code and no missing-file pass. This compatibility boundary
does not grant old heads an exception to the manifest or lineage contract.

The secretless PR gate retains **both** coverage sources: complete,
paginated net PR-file mappings and every per-parent Git rename in the
pinned base-to-head ancestry. Net mappings are not invented direct
historical edges. The collector hashes actual old/new blobs and final
bytes, tracks commit/path identities through ordinary intervening edits,
and resolves every edge to exactly one final entry. An ambiguous merge,
independent lineage reusing a ledger name, missing intermediate/tail,
unreachable/forged evidence, invalid path/type, or truncated API/Git
input refuses. New metadata is proved in the PR that adds it; immutable
base metadata retains that proof after squash merge, even though the
original PR commits need not remain ancestors of the new default head.
`migration_history.py` declares hard bounds: 512 commits, 2048 edges,
fewer than 3000 API files, 3000 migration paths per snapshot/diff,
2 MB per input/blob, 8 MB per Git response, 128 MB aggregate Git output,
100000 lineage states, 250000 event references, and 20 seconds per Git
command. Exceeding a bound refuses rather than truncates evidence.

Inside the existing advisory-locked apply transaction, rows under any
historical source name are **deny-only** evidence: any non-permitted
identity refuses before SQL or a ledger insert, including `--record-only`.
Conflicts and duplicate current/alias rows are checked **before** an
already-applied no-op, for the selected entry and earlier same-target
priors. A good current row cannot hide conflicting history. Staging still
requires exactly one current or genuine byte-identical alias row per app
entry; differing historical rows, current-plus-history, duplicate aliases,
unknown rows and missing entries fail exactly as before.

If staging has no historical conflicting row, use the normal authorized
apply/proof route for the exact current bytes. If any conflicting row
exists, stop that apply and obtain a separately reviewed forward-repair
or environment decision. Never reset a database, rewrite/delete ledger
rows, bless changed bytes with `--record-only`, or generate fake backfill
rows to make a gate green. Source metadata is not staging proof or a
release/deployment authorization.

Tracking began with the 2026-11-03 backfill
(`docs/ops/schema-migrations-backfill-2026-11-03-app.sql` and
`-mirror.sql`, generated by
`.github/scripts/schema_migrations_backfill.py` from the frozen
operator-verified staging state). The three pre-merge applies from
#917 and #922 are recorded by the sibling `-premerge-917.sql` /
`-premerge-922.sql` files, applied only when each PR merges (or
equivalently via `apply_migration.py --record-only`), so the gate is
green immediately after the app backfill. A future shared mirror would couple
staging and production reads; the replacement is not activated and these
historical backfill files do not prove its migration state. Its backfill — and
creating the table on production at all — requires a Release Manager decision;
production applies use the same `apply_migration.py` path once their gate opens.

### Admin bootstrap (owner promotion)

Migration 0028 adds `users.role` (`member` default). The admin API can
change roles only for an existing admin, and an admin can never change
their own role through it — so the **first** admin in each environment is
promoted by this reviewed operator step, run by the migrator role, never
self-serve:

**Ordering (do not reorder):** the owner account must exist and be
promoted **before** `INVITES_REQUIRED` is flipped on in that
environment. Once invites are required, signup needs a code, codes are
issued by an admin, and with no admin promoted yet nobody — including the
owner — can sign up. The flip is a separate operator env action taken
last, on the invite-management work, after a first invite can be issued.

Run in one transaction (replace the email; epoch seconds for `at`):

```sql
BEGIN;
-- Precondition: exactly one row, currently a member.
SELECT id, role FROM users WHERE email = '<owner-email>';
-- The audit insert reads from the promotion itself, so no promotion
-- (zero rows updated: already an admin, or no such account) means no
-- audit row — the log can never record a change that did not happen.
-- Exactly one row must be inserted; otherwise ROLLBACK and investigate.
WITH promoted AS (
  UPDATE users SET role = 'admin'
  WHERE email = '<owner-email>' AND role = 'member'
  RETURNING id
)
INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details)
SELECT id, 'user.role_changed', TRUE, 'ops-bootstrap', <epoch-seconds>, id,
       jsonb_build_object('old', 'member', 'new', 'admin', 'via', 'ops-bootstrap')
FROM promoted;
COMMIT;
```

(`user_id` is the promoted account: for this one operator-run change
there is no acting user, and `details.via` records that it was the
bootstrap rather than an admin-API write.) After promotion, the owner
signs in normally and every further role change goes through
`PUT /v1/admin/users/{id}/role`, which writes its own audit row.

### Admin-issued invites

Migration 0033 extends `invite_codes` (surrogate row id, issuer, kind,
optional email binding, required expiry, redemption and revocation
stamps) and adds `users.invited_by`. Signed-in admins manage invites
through the role-gated admin API: `POST /v1/admin/invites` issues one
invite (optionally bound to an email; per-invite expiry 1–30 days, or
the `invite_default_expiry_days` setting), `GET /v1/admin/invites`
(with `/status/{active|expired|redeemed|revoked}` and
`/issuer/{user_id}` filters) lists lifecycle state, and
`POST /v1/admin/invites/{id}/revoke` revokes an unredeemed invite.
The plaintext code is returned exactly once, in the issue response;
only its hash is stored, and audit rows (`invite.issued`,
`invite.redeemed`, `invite.revoked`) reference the invite by row id —
code material is never stored, logged, or audited. A code is consumed
atomically inside signup completion only, so an abandoned signup
leaves it redeemable, a committed revocation beats an in-flight
redemption, and deleting the redeemed account never frees the code.
Every invalid code — unknown, expired, revoked, spent, email-mismatch
or the issuer's own email — gets the same generic signup error.
Flipping `INVITES_REQUIRED` on stays the last operator step, after
the bootstrap above and after a first invite has been issued and
redeemed end-to-end in that environment.

### Hyperdrive bindings

| Config | ID / GitHub variable | Current PostgreSQL database | Intended binding |
|---|---|---|---|
| `nntunev2-staging` | `61c87403f119430bbc3a7f571df47002` / `HYPERDRIVE_ID_STAGING` | `nntunev2_staging` on app `main` | staging app, new runtime role (host-reported) |
| `nntunev2-prod` | `d38a229748f6452bb417c6d33d277a8d` / `HYPERDRIVE_ID_PROD` | `postgres` on app `main` | reserved prod app; clean launch DB deferred |
| `nntunev2-mb` | `1c6460d5112448888031973c87f671bc` **deleted, never reuse** / `HYPERDRIVE_ID_MB` **unset** | retired origin; replacement unbound | unavailable; no activated replacement |

The three-config GET inventory from 2026-10-13 is historical. The 2026-11-06
cleanup supersedes its MB entry: deletion was followed by GET 404, and the
repository variable was removed with no environment override reported. App
staging/prod configs were untouched by that cleanup. Their older metadata
reported a shared app origin with different users/databases, caching enabled
and `origin_connection_limit = 60`; verify current settings at deployment.
A metadata GET is not an ongoing SQL connectivity or privilege test.

A future shared MB read config would couple staging/prod load, cache behavior
and outages. Import must use a separately verified least-privilege importer,
not the read binding, and still competes for the same connection/compute budget.
The recreated cluster has no activated read binding. Do not restore the deleted
ID: a new verified config, tracked binding/manifest alignment and separate
activation acceptance are required before setting `HYPERDRIVE_ID_MB` again.

The two app pools share one PS-5 single-node cluster, alongside direct
migrator/admin connections; they do **not** have independent branch allowances.
Before deploy/import, the owning lanes must validate the combined connection
budget and cache freshness against the actual origin limits. Database CONNECT
isolation does not isolate CPU, connections, storage/I/O, maintenance, or outages,
and the app single-node allocation must not be described as highly available.

App auth reads require cache-disabled Hyperdrive: the API reads sessions,
credentials, users, challenges and vaults with plain `SELECT`s through the
`HYPERDRIVE` binding (`workers/api/src/worker_entry.rs`), which Hyperdrive
caches by default (60s + 15s stale-while-revalidate, never invalidated by
writes). The staging/prod app configs (`HYPERDRIVE_ID_STAGING`,
`HYPERDRIVE_ID_PROD`) MUST be updated with
`wrangler hyperdrive update <id> --caching-disabled` before the API serves
traffic; query caching is intended only for a newly verified MB config, not
for the deleted config. Deploy checklist (with
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
updated estimates. That historical staging bill and the separate app compute
line do not measure the replacement MB mirror. Its first actual invoice must
record all-in compute, storage, backup and transfer charges against the
**≤$10/month** ceiling; modeled mirror costs are not measured billing.

### MB unavailable: deployment and runtime evidence

With `HYPERDRIVE_ID_MB` unset, `.github/workflows/deploy.yml` records
`staging-mb` as `configured=false`, `deployed=false`, `outcome=unconfigured`.
Ingest deploy requires `staging-mb.outputs.deployed == 'true'` in that same
run; otherwise it skips rather than binding an unavailable MB service. This
is not permission to treat transient matching failures as successful ingest.
The unrelated web/API e2e journey remains independent of MB. The static
contract in `e2e/tests/workflows.spec.ts` protects these deployment guards;
a green skipped lane is never deployment or runtime-health evidence.

The 2026-11-06 staging-only runtime receipt reports the MB Worker still deployed
with its deleted `MB_DB` binding, artwork R2 and cache KV retained; MB lookups
remain unavailable. Ingest had no service, queue or Hyperdrive bindings,
including no `MB_API`; both staging ingest queues had zero consumers. This is
a dated observation, not a fresh production inventory or evidence about queued
jobs. No Worker, bucket or KV deletion, MB-only ownership, licence clearance or
zero-cost claim follows. Replacement provisioning does not restore those bindings.

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
| Transcode jobs (DO class) | `TranscodeContainer` | `TranscodeContainer` |
| Stream meter (AE dataset) | `nntune_meter` | `nntune_meter` |
| Ingest meter (AE dataset) | `nntune_ingest_meter` | `nntune_ingest_meter` |
| Stream service (ingest `NNTP_POOL` cross-script target) | `nntune-stream-staging` | `nntune-stream` |
| Repair service (ingest `REPAIR_CONTAINER` target) | `nntune-repair-staging` | `nntune-repair` |
| Transcode service (ingest `TRANSCODE_CONTAINER` target) | `nntune-transcode-staging` | `nntune-transcode` |
| MB service (ingest `MB_API` target; per-env `name` committed in `workers/mb/wrangler.toml:24,:61,:80`) | `nntune-mb-staging` | `nntune-mb-production` |
| Hostname | `staging.nntune.com` (+ Access; web catch-all + api `/v1/*`, `/healthz`, `/api/library` and `/api/test-codes` routes) | `nntune.com` (decided 2026-10-21; apex attach after staging E2E PASS, `www` redirects to apex) |
| App Hyperdrive variable | `HYPERDRIVE_ID_STAGING` | `HYPERDRIVE_ID_PROD` |
| App PlanetScale branch / PostgreSQL database | `nntunev2` / `main` / `nntunev2_staging` | same `main` cluster / currently `postgres`; clean launch DB deferred |
| MB Hyperdrive variable | `HYPERDRIVE_ID_MB` unset; MB unavailable | MB lane not enabled; shared mirror is intended topology only |
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
| `ingest` (`workers/ingest/wrangler.toml`) | `AUDIO_PUBLISH` | R2 | `nntunev2-audio-staging` | `nntunev2-audio-prod` | name-bound, no ID; write-only publication binding (same buckets as `AUDIO_CACHE`), bucket name mirrored in the `AUDIO_BUCKET_NAME` var |
| `mb` (`workers/mb/wrangler.toml`) | `ART_CACHE` | R2 | `nntunev2-art-staging` | `nntunev2-art-prod` | name-bound, no ID; the prod section still names the staging bucket until the MB art-cache activation lands (`[[known_drift]]`) |
| `mb` | `MB_DB` | Hyperdrive | `nntunev2-mb` logical inventory; replacement unbound | intended only, not enabled | `1c6460d5112448888031973c87f671bc` **deleted, never reuse** (stale committed metadata) / `HYPERDRIVE_ID_MB` **unset** |
| `api` (`workers/api/wrangler.toml.example`) | `HYPERDRIVE` | Hyperdrive | `nntunev2-staging` | reserved: do not enable; clean launch DB deferred | staging `61c87403f119430bbc3a7f571df47002` / `HYPERDRIVE_ID_STAGING`; prod `d38a229748f6452bb417c6d33d277a8d` / `HYPERDRIVE_ID_PROD` |
| `api` (`workers/api/wrangler.toml.example`) | `MB_DB` | Hyperdrive | `nntunev2-mb` logical inventory; replacement unbound | intended only, not enabled | `1c6460d5112448888031973c87f671bc` **deleted, never reuse** (stale committed metadata) / `HYPERDRIVE_ID_MB` **unset** |
| `ingest` (`workers/ingest/wrangler.toml`) | `LIBRARY_DB` | Hyperdrive | intended app `nntunev2-staging` | reserved: do not enable; clean launch DB deferred | same IDs / variables as `api`; prod must remain disabled until the clean launch DB is ready |
| `ingest` | `INGEST_QUEUE` (producer) | Queue | `nntunev2-ingest-staging` | `nntunev2-ingest-prod` | staging `c9e6afdea0ca487c8d20f799a052b045`; prod `2bf7e1a0edd347c9aada2afbc0c24ae5` |
| `mb` / `ingest` (shared reserved namespace) | `MB_CACHE` / `INGEST_STATE` / `INGEST_PAYLOADS` | KV | reserved `nntunev2-config-staging` | reserved `nntunev2-config-prod` | staging `1d4f21508a214ebfa8ff7b594989d6f6`; prod `9664ceed71464521bb6baae2d9374006`; all three bindings share the single reserved namespace per env (dedicated namespaces not provisioned). Keys disjoint by prefix: `ingest:checkpoint:`/`ingest:verify:` (`workers/ingest/src/checkpoint.rs`), `nzb:sha256:` (`workers/ingest/src/dispatch.rs:31`), `mb:lookup:`/`mb:release-recordings:` (`workers/mb/src/lib.rs`). Sharing couples quota/blast radius; accepted for staging; split later without code change |

Known config drift (tracked in the R2 reconciliation table, fixed by the
owning lanes, not here): the ingest queue names are aligned — the real
`workers/ingest/wrangler.toml` declares the per-environment producer, consumer
and DLQ names above. Committed declarations do not prove live consumers: the
2026-11-06 staging receipt reported none on either staging queue, and this
update makes no production-consumer claim. The remaining tracked config drift
is the mb prod `ART_CACHE` override (see the `ART_CACHE` row
above and `[[known_drift]]` in `.github/scripts/binding_manifest.toml`). Do
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
| `NNTP_POOL` (cross-script use) | Durable Object | `ingest` → `stream` | script `nntune-stream-staging` | script `nntune-stream` | staging omits it (`workers/ingest/wrangler.toml:195-202`: target `nntune-stream-staging` does not exist); prod `workers/ingest/wrangler.toml:259-262` (per-env resolution follows the stream worker names in `workers/stream/wrangler.toml:43,55`) |
| `METER` | Analytics Engine | `stream` | dataset `nntune_meter` | dataset `nntune_meter` | `workers/stream/wrangler.toml:38-40` (top-level), `:50-52` (staging), `:62-64` (production); code `workers/stream/src/observability.rs:23` |
| `INGEST_METER` | Analytics Engine | `ingest` | dataset `nntune_ingest_meter` | dataset `nntune_ingest_meter` | `workers/ingest/wrangler.toml:136-139` (top-level), `:212-214` (staging), `:272-274` (production); code `workers/ingest/src/metering.rs:16-18` |
| `REPAIR_CONTAINER` | service | `ingest` → `repair` | service `nntune-repair-staging` | service `nntune-repair` | staging omits it (`workers/ingest/wrangler.toml:195-202`: target does not exist); top-level `:141-145`, production `:276-278`; target names `workers/repair/wrangler.toml:15` (prod default), `:50` (staging), `:69` (prod); contract note `:3-6`; code `workers/ingest/src/repair.rs:24` |
| `TRANSCODE_CONTAINER` | service | `ingest` → `transcode` | service `nntune-transcode-staging` | service `nntune-transcode` | staging omits it (`workers/ingest/wrangler.toml:195-202`: target does not exist); top-level `:147-152`, production `:280-282`; target names `workers/transcode/wrangler.toml:15` (prod default), `:51` (staging), `:71` (prod); contract note `:3-9`; code `workers/ingest/src/transcode.rs:95` |
| `MB_API` | service | `ingest` → `mb` | service `nntune-mb-staging` | service `nntune-mb-production` | `workers/ingest/wrangler.toml:154-158` (top-level), `:216-221` (staging), `:284-286` (production); per-env names committed in `workers/mb/wrangler.toml:24` (top-level), `:61` (staging), `:80` (production); code `workers/ingest/src/matching.rs:30` |
| `REPAIR_JOBS` | Durable Object | `repair` | class `RepairContainer` | class `RepairContainer` | `workers/repair/wrangler.toml:33-35` (top-level), `:61-63` (staging), `:80-82` (production); migration tag `v1` `:37-39`; Container class/image/sizing `:23-31`, `:52-60`, `:71-79`; code `workers/repair/src/runtime.rs:33` |
| `TRANSCODE_JOBS` | Durable Object | `transcode` | class `TranscodeContainer` | class `TranscodeContainer` | `workers/transcode/wrangler.toml:34-36` (top-level), `:63-65` (staging), `:83-85` (production); migration tag `v1` `:38-40`; Container class/image/sizing `:23-32`, `:53-62`, `:73-82`; code `workers/transcode/src/runtime.rs:34` |

Notes, all referencing (not changing) owning-lane state:

- The real ingest config carries per-environment values for its
  cross-script and service targets: staging binds only `MB_API` to
  `nntune-mb-staging` (`workers/ingest/wrangler.toml:216-221`) and omits
  `NNTP_POOL`, `REPAIR_CONTAINER` and `TRANSCODE_CONTAINER` (`:195-202`),
  while the top-level and production sections restate the full prod shape
  (`:108-115`, `:141-158`, `:259-286`). Stream, repair and transcode have
  explicit per-env `name` overrides committed in their configs, so their
  per-environment resolution follows the target workers' committed names
  above; mb likewise commits per-env names (`workers/mb/wrangler.toml:24`,
  `:61`, `:80`), which ingest's per-env `MB_API` targets name directly.
- The repair and transcode Workers are not yet deployed: their config
  headers (`workers/repair/wrangler.toml:11-13`,
  `workers/transcode/wrangler.toml:11-13`) require Cloudflare Containers on
  the existing plan (no new spend) with deploy as a later operator action.
  Until then the `REPAIR_CONTAINER` and `TRANSCODE_CONTAINER` target names
  are reserved, not live — which is why the ingest staging section omits
  those bindings: Cloudflare fails a deploy whose service-binding target
  does not exist.
- Deploy order is stream, mb, repair and transcode (once Containers are
  enabled) before ingest. Ingest reaches the per-user `NntpPool` object
  through the cross-script `NNTP_POOL` binding
  (`workers/stream/wrangler.toml:4-8`); its staging section declares only
  the `MB_API` service binding to `nntune-mb-staging`
  (`workers/ingest/wrangler.toml:216-221`, mb-first), re-adding
  `NNTP_POOL`, `REPAIR_CONTAINER` and `TRANSCODE_CONTAINER` per environment
  once each target Worker deploys (see the operator runbook below).

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
SHA, run from `apps/web`) with the `nntune.com` apex custom domain (repo
variable `PRODUCTION_DOMAIN`) attached and `www` redirecting to the apex,
leaving a single 100% deployment. The lane enforces this before
uploading anything: its seed step lists production deployments and refuses
with these instructions when no single-version 100% deployment exists, so a
first run fails with guidance instead of orphaning an uploaded version inside
the promotion action. Re-dispatch the lane after seeding.

### Production Worker secret-shape gate

The production lane refuses any production vars beyond `name` in
`[env.production]` before uploading a version. The gate step
(`.github/workflows/deploy-production.yml:233-241`) runs
`node ci/check-production-secrets-shape.mjs`, which enforces the offline
policy in `ci/check-production-secrets-shape.mjs:1-7`: the web Worker ships
no bindings and needs no secrets, so its production env table must carry
only `name` (current state: `apps/web/wrangler.toml:31-32`). It runs in the
`deploy-production` job after that job checks out the validated promotion
SHA (`.github/workflows/deploy-production.yml:210-214`), not in preflight
(preflight checks out the dispatch tip,
`.github/workflows/deploy-production.yml:84-86`). It is an offline
shape check — no credentials, no network — that reads key names only and
never reads or prints values
(`ci/check-production-secrets-shape.mjs:6-7`).

### Production promotion publishes a CalVer release

After a successful production promotion and its post-promotion smoke, the
deploy job records a promotion receipt (`Record successful production
promotion`, uploaded as the attempt-bound
`production-promotion-<sha>-<run>-<attempt>` artifact) and the `release`
job calls `.github/workflows/release.yml` with the deployed SHA and the
producing run id/attempt. `release.yml` proves the receipt
(`ci/release-request.cjs` + `ci/release-promotion-proof.cjs`: the run must
be a main-branch `workflow_dispatch` of `deploy-production.yml` whose
promotion, smoke and receipt steps all succeeded in order) and then plans
and publishes a calendar version (`vYYYY.MDD.P`, e.g. `v2026.1102.1`)
through the shared `TogetherWeOwn/.github` CalVer releaser (policy
`production-promotion`; notes grouped by `release-please-config.json`
changelog-sections). A re-promote of an already-tagged commit skips green
without a tag. Production therefore only ever deploys on the release path
(`environment: production`, human-gated); push to `main` deploys staging
only (`environment: staging` on both deploy jobs). No other GitHub
environment exists on this repo.

- [ ] Exact-head CI: record the promotion SHA and GitHub Actions run/job
  URLs showing `ci-ok`, `gitleaks` and `pr-lint` green on that SHA.
  CI is `.github/workflows/ci.yml`; full suites and CI gates run in Actions,
  never in a local CI mirror. Reviewers and the Release Manager accept
  these exact-head links as CI evidence, not local full-suite transcripts
  ([verification contract](../CONTRIBUTING.md#evidence-for-review-and-release)).
  `migrations-staging` and `Devin Review` report on pull requests, not on
  `main`: record the PR whose squash commit is the promotion SHA, and that
  they were green on that PR's final head. Answer any advisory CodeRabbit
  findings without waiting for it. Required checks and review/release
  gates are unchanged; this evidence does not authorize production or
  replace the staging, migration and rollback boxes below. A red required
  check is fixed in code or recorded as a flake/quarantine decision with
  a card link, never dismissed.
- [ ] Secret-shape gate: the promotion run's `Require production Worker
  secret shape before upload` step passed on the promotion SHA
  (`.github/workflows/deploy-production.yml:233-241` runs
  `ci/check-production-secrets-shape.mjs`, which refuses any
  `[env.production]` table beyond `name`). Same-checker CI coverage
  (`.github/workflows/ci.yml:824-842`) is not promotion evidence on its
  own; require the deploy-production job's pass on the exact head being
  promoted.
- [ ] Staging deploy evidence: record the successful push-triggered
  `deploy.yml` run for the promotion SHA, and require its `staging-web` job
  conclusion to be `success` (not `skipped`); verify staging health. This
  workflow deploys `apps/web` (`staging-web`) plus the api, mb and ingest
  Worker staging lanes (`staging-api`, `staging-mb`, `staging-ingest`;
  staging only, mb before ingest); it does not deploy stream, repair or
  transcode Workers. `/api/health` returns exactly `{ok, version}` — the public
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
  staging host is served by Worker routes (web catch-all plus api path
  routes, see the DNS section); the `100::` placeholder record stays —
  it routes the hostname to the edge and is not replaced.
- [ ] Staging E2E: `deploy.yml` sequences it — its `post-deploy-e2e` job
  calls `e2e-staging.yml` (reusable workflow) only after the same run's
  `staging-web` and `staging-api` jobs succeed (`needs`, no `always()`;
  a green `staging-api` job may be an unconfigured skip, passed on as
  `api_deploy_outcome`), so a skipped/failed
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
  policy and the QA service-token policy still gate `staging.nntune.com`.
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
  `staging-web` success and staging health. The rerun also redeploys the api,
  mb and ingest Workers through `staging-api`, `staging-mb` and
  `staging-ingest` when those lanes are configured; that is not verified
  Worker rollback, and each Worker needs its own rollback evidence.
  Staging rollback is
  a redeploy, never a data restore; retain the UNDO blocks and consolidation
  receipt for infrastructure-specific rollback.

## Future Worker deploy lanes: required rollback evidence

This section defines evidence only. It does not create a lane.

**Current claim boundary.** `.github/workflows/deploy.yml` deploys
`apps/web` (`staging-web` on push; no per-PR preview) plus the api, mb and
ingest Worker staging lanes (`staging-api`, `staging-mb`, `staging-ingest`
on push, staging only, mb before ingest; each skipped until the Operator
wires its staging variables).
`.github/workflows/deploy-alerts-staging.yml` deploys only the
`nntune-alerts-staging` consumer from `workers/observability`. No other
tracked workflow deploys the remaining backend Workers under `workers/`
(`stream`, `repair`, `transcode`), and
`.github/workflows/e2e-staging.yml` exercises the staging web URL, not a
Worker deploy or rollback. Worker source and Wrangler configs existing in
the repo are not deployment. Therefore **no backend-Worker deploy,
rollback, activation, or staging acceptance is currently claimed**, for any
Worker in any environment, until that Worker's lane exists and its gates
pass on the exact head being promoted — for the api Worker that requires a
`staging-api` job conclusion `success`, successful **executed** `Hyperdrive
cache-disabled gate`, `Verify actual staging migration schema (read-only)` and
`Deploy api to staging` steps, and the job output
`deployed=true` (`outcome=deployed`) on the promotion run; for the mb Worker
that requires a `staging-mb` job conclusion `success`, a successful
**executed** `Deploy mb to staging` step, and `deployed=true`
(`outcome=deployed`); for the ingest Worker that requires a `staging-ingest`
job conclusion `success`, successful **executed** `Hyperdrive cache-disabled
gate` and `Deploy ingest to staging` steps, and `deployed=true`
(`outcome=deployed`). Each lane's `source_sha` output must match the
promotion SHA and the deploy's `BUILD_SHA` argument (the Workers' deployed
`build_sha` log field carries it; a missing or invalid value renders as
`"unknown"`, never evidence).
A missing-config job can conclude `success` while reporting
`deployed=false`, `outcome=unconfigured`, and an empty `source_sha`; it is
not deploy evidence. The names-only deployment summary also reports failed
or cancelled paths without attributing a deployed source SHA. This marker
alone proves neither health, rollback nor full staging acceptance.
The web checklist above is unchanged by this section and remains in
force alongside the per-Worker evidence table below. The web frontend's
production lane (`.github/workflows/deploy-production.yml`, disabled by
default until the launch hostname decision:
`.github/workflows/deploy-production.yml:6-9`) already exists and is not a
future lane; its secret-shape gate is recorded in the staging checklist
above, not in this evidence table.

**Evidence a future Worker staging lane must retain**, per Worker and per
environment, linked from that promotion's deploy record before rollback can
be called accepted:

| # | Retain | Acceptance bar |
|---|---|---|
| 1 | Deployed Worker and version | Worker (script) name and the version reported by the deploy tooling for the version now serving, plus the environment deployed to. |
| 2 | Source revision | The exact commit SHA that version was built from. Where the Worker exposes a deployed-build identity, record it and require it to match this SHA; without one, do not claim same-build evidence (the web checklist above verifies this via `GET /api/build`; a Worker with no such surface cannot claim it). |
| 3 | Deploy run | URL of the workflow run (or equivalent executed deploy record) that produced item 1, with the Worker deploy job's conclusion `success`. A `skipped` job, a skipped gate, or overall workflow success without that job succeeding is not deploy evidence. For the API Worker, retain successful executed `Hyperdrive cache-disabled gate`, `Verify actual staging migration schema (read-only)` and `Deploy api to staging` step outcomes plus `staging-api` outputs `deployed=true`, `outcome=deployed`, and `source_sha` matching item 2. For the MB Worker, retain the successful executed `Deploy mb to staging` step outcome plus `staging-mb` outputs `deployed=true`, `outcome=deployed`, and `source_sha` matching item 2. For the ingest Worker, retain successful executed `Hyperdrive cache-disabled gate` and `Deploy ingest to staging` step outcomes plus `staging-ingest` outputs `deployed=true`, `outcome=deployed`, and `source_sha` matching item 2. A config-skipped `success` does not qualify. Record gate results by name and outcome only. |
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

## Operator runbook: enabling staging-mb / staging-ingest

**Unavailable as of 2026-11-07 — do not execute the sequence below.** The
former MB config is deleted; the conditional replacement is not an imported,
ready or activated mirror. Keep `HYPERDRIVE_ID_MB` unset and the ingest enable
gate disabled. See the [dated lifecycle inventory](#planetscale-postgres) and
[ADR 0007 amendment](adr/0007-mb-mirror.md#2026-11-07-amendment-retirement-and-conditional-bounded-replacement).

Before this sequence becomes executable, require validated import/readiness
and separately accepted activation. A reviewed, coordinated replacement-ID
change must align `workers/mb/wrangler.toml`, API MB metadata in
`workers/api/wrangler.toml.example`, `.github/scripts/binding_manifest.toml`,
relevant binding/deploy contracts and the Hyperdrive inventory **before any
variable wiring**. Both API and MB deploy assertions pin the manifest ID;
a new variable alone fails parity, while the retired ID targets no config.
Do not invent an ID, restore the retired one or provision a resource from this
runbook. This documentation change neither updates bindings nor accepts
activation. All existing intake, security and release gates still apply.

Operator-owned actions only. No secrets in the repo or the workflow: Worker
secrets are operator-set in Cloudflare, and the lanes below only ever read
secret *names* (the `CLOUDFLARE_*` credentials stay in repo secrets). The
`staging-mb` and `staging-ingest` lanes exist in
`.github/workflows/deploy.yml` but must stay operator-disabled — no push
deploys either Worker — until every step below clears, in order:

1. **TWO-1660 gate (ingest only).** Do not enable `staging-ingest` until
   TWO-1660 closes search intake. The lane reads the public repo variable
   `INGEST_STAGING_ENABLED`, disabled by default — unset, or any value but
   `true`, skips every downstream step with a notice, so the live consumer
   can never deploy before the Operator acts. Leave it unset until intake
   closes, then set it to `true`. The `[[queues.consumers]]` stanza in
   `workers/ingest/wrangler.toml` is live, so the consumer goes live the
   moment the lane first deploys; a search job admitted before the search
   boundary is wired would burn its retry budget against the unwired
   boundary and quarantine. Once the replacement prerequisites above clear,
   `staging-mb` may deploy first — ingest needs it (`needs: [gate, staging-mb]`,
   since `MB_API` targets `nntune-mb-staging`).
2. **After replacement alignment and activation acceptance only, wire the
   staging variables, mb first.** Set the public repo variables (names only,
   never values) `HYPERDRIVE_ID_MB` then `HYPERDRIVE_ID_STAGING`, matching
   the newly verified active inventory. The deleted MB ID is historical,
   never a value to wire; until the coordinated change lands, stop here. Each lane pins its variable against
   `.github/scripts/binding_manifest.toml` before generating its staging
   config and fails the job on mismatch, so a mistyped variable blocks the
   deploy instead of deploying the wrong binding. Set
   `INGEST_STAGING_ENABLED` to `true` only after step 1 clears — wiring the
   two Hyperdrive variables alone never enables `staging-ingest` (the api
   lane already needs `HYPERDRIVE_ID_STAGING` wired).
3. **Confirm Worker secrets are operator-set in Cloudflare (names only,
   never values).** The lanes deploy with `--var BUILD_SHA` (public) only.
   `wrangler secret put` each of the following before enabling the lanes:
   - ingest staging (`nntune-ingest-staging`): `INGEST_CALLER_SECRET`
     (required; every route fails closed without it — code
     `workers/ingest/src/caller_auth.rs`, `workers/ingest/src/lib.rs`).
   - ingest production shape (`nntune-ingest`, parity only, not deployed by
     this card): same `INGEST_CALLER_SECRET`, operator-set when production
     activates.
   - api worker (staging and prod, `workers/api`): the same value
     `INGEST_CALLER_SECRET` (grant minting fails closed without it — code
     `workers/api/src/ingest_grant.rs`); it must match the ingest value.
   - mb staging (`nntune-mb-staging`) and production shape: no Worker
     secrets required (only public vars `BUILD_SHA`, `ENVIRONMENT`).
   Public vars are not secrets: `BUILD_SHA` (lane-supplied `--var`) and
   ingest `AUDIO_BUCKET_NAME` (`[vars]`). Record secret *presence* by name
   only, never values.
4. **Read-only live-inventory verification (no writes).** Before enabling
   the lanes, confirm the live staging inventory still matches
   `.github/scripts/binding_manifest.toml` and the tables above: queues
   `nntunev2-ingest-staging` and `nntunev2-ingest-dlq-staging`, R2 buckets
   `nntunev2-ingest-scratch-staging` / `nntunev2-audio-staging` /
   `nntunev2-art-staging`, Hyperdrive ids behind `HYPERDRIVE_ID_MB` and
   `HYPERDRIVE_ID_STAGING`, and KV namespace ids
   `1d4f21508a214ebfa8ff7b594989d6f6`. Read-only GETs/listings only; if any
   live id differs, stop and reconcile the manifest/inventory first — do not
   enable the lanes.
5. **Deploy and verify.** Push to `main` and require, on that run, the
   `staging-mb` job then the `staging-ingest` job to conclude `success`
   with outputs `deployed=true`, `outcome=deployed`, and `source_sha`
   equal to the push SHA (which must also equal the deploy's `BUILD_SHA`
   argument and the Workers' `build_sha` log field — `"unknown"` is never
   evidence). An ingest run reporting `outcome=disabled` (enable gate unset)
   or `outcome=mb-not-deployed` (mb skipped unconfigured) is a skip, not a
   deploy — do not treat it as evidence; wire the missing piece and push
   again. Then verify, read-only: the Cloudflare dashboard lists
   deployments for `nntune-mb-staging` and `nntune-ingest-staging`; the
   queue consumer is attached to `nntunev2-ingest-staging`; no
   `*.workers.dev` URL serves either Worker (`workers_dev = false`, no
   routes); mb answers over the `MB_API` service binding (ingest staging
   `MB_API` targets `nntune-mb-staging`, never public HTTP). Confirm the
   `nntune-alerts-staging` tail consumer receives both Workers' structured
   records; the deployment summary alone proves neither health nor staging
   acceptance.
6. **Configure the one staging read account in the pool registry.** Write
   against the merged pool-registry admin surface if it has landed;
   otherwise mark this step explicitly blocked on TWO-1643 and do not
   proceed with a manual workaround. Credentials are operator-entered only,
   never in repo, logs, or cards.
7. **Re-add the omitted bindings later.** Once `nntune-stream-staging`,
   `nntune-repair-staging` and `nntune-transcode-staging` deploy, re-add
   `NNTP_POOL`, `REPAIR_CONTAINER` and `TRANSCODE_CONTAINER` to the ingest
   staging section with the per-environment targets, remove the
   corresponding `absent_in` entries from
   `.github/scripts/binding_manifest.toml`, and re-run binding-parity
   before the next staging deploy.
8. **Rollback is a redeploy, never a data restore.** To roll back a Worker,
   re-run the known-good push's `deploy.yml` run (**Re-run all jobs**;
   never `workflow_dispatch` on an old SHA — the lanes are push-only and
   would skip) and retain the per-Worker evidence in the table above. A
   rerun redeploys every configured lane; that is not verified per-Worker
   rollback unless each Worker's own outcome evidence is recorded.
