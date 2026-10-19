# Database restore runbook and forward-fix-vs-restore policy

Staging-first, evidence-recorded procedure for a PlanetScale Postgres
database restore. Docs only: this runbook authorizes no restore by itself
and changes no code or migration.

## 0. Scope and authority

- **In scope:** the app database (`nntunev2_staging` on cluster `nntunev2` /
  branch `main`) and the shared MB mirror (`postgres` on cluster
  `nntunev2-mb` / branch `main`). Current topology: `docs/ops.md`
  PlanetScale Postgres section and environments matrix.
- **Out of scope:** running an actual restore, touching migrations, deploys,
  live provider calls, secret use, new spend, key rotation. A restore that
  needs any of those is a separate authorized action, not covered by reading
  this page.
- **Authorization.** A restore executes only on an explicit written
  authorization naming environment, source point-in-time, and target, held
  by the Operator (host executor). The author of this runbook does not
  self-authorize. Required approvers before any step under §3 runs:
  Operator plus one second reviewer. Production restore additionally needs
  the gated production workflow — this runbook alone never authorizes prod
  writes, prod migration, or clean-launch-database creation (see
  `docs/ops.md` data-plane-guarded checklist).
- **Execution.** Only the Operator runs provider-side steps, from the
  authorized host path, using the existing secure wiring
  (`PLANETSCALE_MIGRATOR_URL_STAGING`, Hyperdrive bindings). No database
  URL, password, token, hostname, account ID, environment value, or
  personal login identity is printed, copied into a ticket, or committed.
  Evidence records carry the operational fields §6 requires (names, dates,
  query expressions, migration numbers, counts, cutover mechanism/time,
  version identifiers, commit SHAs, run URLs) and outcomes — and nothing
  sensitive (see the prohibited list in §6).
- **Claim boundary.** PlanetScale console/CLI behavior below is documented
  from this repository's host receipts, not from a live provider session in
  this workspace. Every platform fact this author could not verify against
  a live receipt is tagged **[UNVERIFIED — confirm in console]** at first
  use and collected in §7. Do not quote an untagged platform claim as
  verified.

## 1. Definitions (reconciled policy)

Three distinct operations. They are not interchangeable:

| Operation | What it does | Governing rule |
|---|---|---|
| **Redeploy rollback** | Re-serves a prior known-good build (web rerun; Worker redeploy/activation). Touches code only. | `docs/ops.md:504`: staging rollback is a redeploy, **never a data restore**. `docs/ops.md:540-545`: a Worker rollback restores Worker code only; **rollback is never a data restore**. |
| **Forward fix** | Repairs schema or data with a NEW migration / NEW write on top of current state. History is append-only. | `migrations/README.md:9`: migrations are forward-only — correct a deployed schema with a new migration, never by editing history. Enforced by `migrations_gate.py` `check_drift` (`applied-edited`, `applied-removed`). |
| **Data restore** | Replaces or re-seeds database contents from a backup, point-in-time (PITR) snapshot, or branch **[UNVERIFIED — confirm in console: exact PlanetScale Postgres restore primitive names and whether restore is in-place, branch-clone, or dump-reload]**. | This runbook only. A restore never rewrites migration history (§4). |

**Policy, in one paragraph:** the default response to a bad migration or
bad data is a forward fix (new migration, compensating write, and/or
redeploy rollback of the code). A data restore is reserved for
data-loss or data-corruption cases where forward repair cannot
reconstruct the data (see decision table). A restore never substitutes
for a redeploy rollback, and a redeploy rollback never substitutes for a
restore — `docs/ops.md:504` and `:540-545` forbid conflating them in
either direction. After any restore, schema convergence is still
forward-only (§4).

### Decision table

| Situation | Action |
|---|---|
| Bad deploy, data intact | Redeploy rollback per `docs/ops.md` staging checklist / Worker-lane evidence table. No restore. |
| Bad migration applied (wrong column, bad default), data recoverable in place | Forward fix: new migration correcting it (`migrations/README.md:9`). No restore. |
| Bad data written by app bug, rows reconstructable from logs/queues/sources | Forward fix: compensating write or re-ingest. No restore. |
| Data deleted, corrupted, or encrypted-badly with no reconstructable source (failed migration destroyed rows, operator error dropped objects, storage-level corruption) | Restore under this runbook, then forward-converge (§4). |
| MB mirror (`nntunev2-mb`) damaged or stale | Prefer rebuild from the upstream MusicBrainz source over PITR **[UNVERIFIED — confirm in console: MB dataset size (~100 GB target per `docs/ops.md`) may make branch-restore cheaper than full re-import; Operator confirms per incident]**. PITR only if the source rebuild path is down. |
| Unsure whether data is reconstructable | Treat as forward-fix lane first; open the restore authorization in parallel but do not execute §3 until reconstruction is ruled out in writing. |

## 2. Before anything: triage and freeze (no writes)

1. Declare the incident: environment (`staging` only, unless the gated
   prod path is invoked), database (`nntunev2_staging` vs MB `postgres`),
   first-seen time, symptom, suspected cause. Staging restores stay on
   staging; prod data is never touched from a staging authorization —
   except the MB lane (§3.6): the single shared `nntunev2-mb` config
   (`HYPERDRIVE_ID_MB`) serves both staging and prod MB reads
   (`docs/ops.md` PlanetScale Postgres section and environments matrix),
   so an MB cutover affects both readers together and needs an
   authorization naming both.
2. Stop the bleeding without destroying evidence: halt writers that touch
   the affected database (pause the owning deploy/import lane; do not
   delete configs, roles, or databases — decommissioning needs the
   data-retention gate per `docs/ops.md` UNDO blocks). Record what was
   paused, when, by whom.
3. Snapshot the present: record current branch/database, the applied
   migration head from the target database's authoritative
   per-database applied-migration record (not the `migrations/manifest.toml`
   highest number — the manifest is a repository file list, not per-database
   applied state), serving build SHAs (`GET /api/build` for web; Worker
   version records per `docs/ops.md` future-lanes table), and a read-only
   row-count/sample of the damage. Per `docs/adr/0002-data-model.md`
   the per-database migration runner and its applied-history record are
   an unimplemented prerequisite: until that runner/record lands, the
   applied head cannot be established authoritatively and §§3.4/5 are
   blocked. This snapshot is the baseline the post-restore verification
   (§5) diffs against.
4. Rule out forward fix in writing: state why a new migration or
   compensating write cannot reconstruct the data, with the row/sample
   evidence from step 3. If it can, close the restore lane and work the
   forward-fix lane.
5. Obtain the §0 authorization (environment, source point-in-time, target)
   and record it with approver names and date. No §3 step runs without it.

## 3. Restore step order (Operator executes)

> Each step records its evidence (§6) before the next begins. Abort and
> hold on any failed check; a failed check is never bypassed.

1. **Confirm source and retention.** Confirm the backup / PITR snapshot
   covering a point-in-time *before* the damage exists and its exact
   timestamp. Retention horizon, snapshot cadence, and PITR window for
   PlanetScale Postgres are **[UNVERIFIED — confirm in console: at time of
   writing no live retention receipt is on file; do not assume any RPO]**.
   If the needed point-in-time predates the oldest restorable snapshot,
   stop: the restore lane is closed, work forward reconstruction, and
   record the gap.
2. **Restore to a NEW target, never in place.** Restore the snapshot to a
   new database/branch (e.g. a restore branch or `*_restore_<date>`
   database on the same cluster) **[UNVERIFIED — confirm in console:
   exact PlanetScale Postgres restore-target primitive and naming rules]**.
   The live database keeps serving (or stays paused per §2) until cutover;
   in-place overwrite is forbidden because it destroys the §2 snapshot and
   removes the abort path.
3. **Verify the restored copy read-only.** Against the NEW target only,
   using the authorized read-only path: confirm the snapshot timestamp
   predates the damage, row counts/samples match the expected pre-damage
   state, and schema objects are present. Record date, database/branch and
   role labels, check expressions, and outcomes — never URLs or secrets
   (same reporting shape as the `docs/ops.md` MB grant follow-up).
4. **Forward-converge the schema (never rewrite history).** Diff the
   restored copy's applied-migration head (from the per-database
   applied-migration record, §2 step 3) against the live head. Re-apply
   only manifest entries whose `target` matches the restored database
   (`app` for `nntunev2_staging`, `mirror` for the MB `postgres`) that the
   snapshot predates, as NEW forward applications on the restored copy, in
   manifest order, through that database's own migrator role — never by
   editing, renumbering, or deleting migration files
   (`migrations/README.md:9`; `applied-edited` / `applied-removed` would
   fail the gate). Abort on any entry with a missing or mismatched target
   (the manifest interleaves targets — e.g. 0001/0014/0016/0017 are
   `mirror` — so unfiltered re-application would run mirror migrations
   against the app database and vice versa, which
   `docs/adr/0002-data-model.md` forbids). If the damage was caused by a
   bad migration, include its forward correction in this pass. Blocked
   until the per-database runner/record prerequisite (§2 step 3) lands.
5. **Vault/KEK pass before writes reopen.** Restored rows may pin older
   `kek_version` values. Per `docs/security/kek-rotation.md` §5 invariant
   3 and §7 RESTORE: confirm the KEK versions the restored rows need are
   still provisioned, and run the re-wrap pass over restored rows BEFORE
   reopening writes so no row stays pinned to a retired version. The
   re-wrap runner and the versioned KEK map (`VAULT_KEK_V<N>`
   per-version secrets + `VAULT_KEK_CURRENT` write-version var +
   `rotation_runs` progress table) are **not implemented**
   (`docs/security/kek-rotation.md` §7 NOT EXECUTABLE banner, §8 gate
   items; the Worker reads only `VAULT_KEK_BASE64` + `VAULT_KEK_VERSION`):
   a restore of a vault-bearing (app) database is blocked until they
   land. Retired key material stays in the operator store until the
   retire-after date (latest of re-wrap-complete + one window, oldest
   restorable backup/PITR horizon expiry) — a restore never justifies
   deleting key material early.
6. **Cutover (authorized promotion, not a redeploy).** Only after steps
   3–5 are recorded green: repoint the serving Hyperdrive binding to the
   restored-and-converged target through the authorized host path —
   app database: `HYPERDRIVE_ID_STAGING` (`nntunev2-staging` config; prod
   `HYPERDRIVE_ID_PROD` stays reserved) — or promote/rename the restore
   branch per the provider primitive **[UNVERIFIED — confirm in console:
   whether PlanetScale Postgres cutover is branch-promote, rename, or
   connection-string repoint, and whether Hyperdrive caches the old
   origin]**. The `PLANETSCALE_MIGRATOR_URL_*` secret is migration wiring
   only, never a serving-traffic cutover mechanism. MB lane is different:
   there are no separate MB staging/prod configs — staging and prod MB
   reads share the single `nntunev2-mb` config (`HYPERDRIVE_ID_MB`,
   `docs/ops.md` PlanetScale Postgres section and environments matrix) —
   so an MB cutover switches both readers at once: it needs an
   authorization naming both staging and prod readers (a staging-only
   authorization is insufficient), and staging-first does not apply to MB
   until separate MB configs exist. App lane stays staging-first always;
   the prod app binding stays reserved until the gated production
   workflow runs. Never reuse credentials from a deleted branch (cf.
   `docs/ops.md` consolidation UNDO).
7. **Health and E2E.** Verify staging health, deployed-build identity
   (`GET /api/build` 40-hex match where applicable), and the same-run
   post-deploy E2E bar from the `docs/ops.md` staging checklist. For
   Workers, retain the per-Worker evidence rows (version, SHA, run URL,
   post-rollback health) — a green restore run without the serving-identity
   match is not acceptance.
8. **Retain the old target until the bake window passes.** Keep the
   pre-restore database/branch (and its §2 snapshot record) for at least
   one full verification window (staging bake ≥24h by analogy with the
   KEK rotation bake rule) before any deletion, and then only through the
   data-retention gate. Deletion is a separate authorized action.

## 4. Why a restore never breaks forward-only

A restore moves *data* back in time; it must not move *history* back.
`migrations_gate.py check_drift` compares repository migration files and
manifest entries against the base ref only (git `ls-tree`/`show`) and
fails on `applied-edited` (contents/target/checksum changed) or
`applied-removed` (migration missing) — it never reads database state, so
a green gate proves nothing about whether the restored database was
converged. Restoring a snapshot that predates migration N and then
continuing as if N never existed would leave the database behind the
repository history the gate enforces. The §3.4 forward-convergence pass
exists to prevent exactly that: the restored copy re-applies every
missing same-target migration forward, so `manifest.toml` stays strictly
increasing and checksums intact. Blocked until the per-database
runner/record prerequisite (§2 step 3) lands — there is currently no
authoritative applied record for the gate or this runbook to converge
against.
If the restored copy cannot converge forward (e.g. a later migration is
incompatible with older data), stop and escalate — do not edit the old
migration files to force convergence.

## 5. Post-restore verification (acceptance bar)

- Snapshot timestamp predates the damage; pre/post row counts and samples
  reconcile (expected losses/gaps listed explicitly, none hand-waved).
- Applied-migration head on the live target (per the per-database
  applied-migration record, §2 step 3) equals the pre-incident head plus
  any forward corrections; `migrations_gate.py` green on the head
  (repository-side check only — it does not prove database state, §4).
- KEK re-wrap complete (app database only; blocked until the §3.5
  re-wrap runner prerequisite lands): `SELECT count(*) FROM
  provider_vaults WHERE kek_version < <VAULT_KEK_CURRENT>` reads 0 twice
  (two consecutive passes, current = the `VAULT_KEK_CURRENT` write
  version), per the rotation invariant; retire-after date re-recorded
  against the restore date. For progress by version, the count-only
  grouped query in `docs/security/kek-rotation.md` §6 (no identifiers
  or ciphertext).
- Staging health green, `GET /api/build` matches the serving SHA where
  applicable, post-deploy E2E recorded with run URL.
- Old target retained; deletion gated, not yet executed.

## 6. Evidence record (copy per incident)

| Field | Value |
|---|---|
| Date / environment / database | |
| Authorization (approvers, scope) | names + date only |
| Damage snapshot (§2.3: migration head, SHAs, row counts) | |
| Forward-fix ruled out because | |
| Source snapshot timestamp / retention check (§3.1) | |
| New restore target name (§3.2) | name only, no URL/secret |
| Read-only verification queries + outcomes (§3.3) | expressions + outcomes only |
| Forward-convergence list (migrations re-applied, §3.4) | numbers + manifest head |
| KEK re-wrap result (counts ×2, §3.5) | |
| Cutover record (binding repoint/promote, §3.6) | mechanism + time |
| Health + E2E run URLs (§3.7) | URLs to runs only |
| Old-target retention + deletion gate (§3.8) | |

Store the completed record with the incident. Evidence records carry the
table's operational fields above (names, dates, query expressions,
migration numbers, counts, cutover mechanism/time, version identifiers,
commit SHAs, run URLs) — never account IDs, tokens, credentials,
database URLs, environment values, or personal login identities (same
prohibited set as `docs/ops.md:547-549`).

## 7. Unverifiable platform facts (must confirm per incident)

These are explicitly NOT claimed by this runbook. The Operator confirms
each from the live PlanetScale console/billing/receipt at restore time
and records the answers in the §6 record:

1. PlanetScale Postgres backup cadence, PITR window, retention horizon,
   and RPO for clusters `nntunev2` and `nntunev2-mb`.
2. The exact restore primitive (branch restore, PITR clone, dump/reload)
   and its target-naming rules.
3. Whether cutover is branch-promote, rename, or connection repoint, and
   any Hyperdrive/pool caching of the old origin.
4. Per-cluster storage/backup/egress metering for the restore month
   (`docs/cost-basis.md` carries formulas, not the incident's meter
   readings; $10 compute receipt excludes metered terms).
5. Connection/compute headroom on the shared PS-5 single-node app cluster
   during a restore (app staging/prod pools plus migrator/admin share one
   allowance per `docs/ops.md:279-285`; single-node is not HA).
6. MB mirror restore-vs-rebuild cost break-even at the incident's data
   size (restore may still lose to source rebuild for the ~100 GB import
   target).
