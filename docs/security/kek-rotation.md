# KEK Rotation + Versioning (F-VAULT-02)

Owner: CISO · Status: design (normative for the vault implementation;
the vault PR may not merge until the CISO gate verifies this design at
its exact head — see `threat-model.md` §8).

## 0. Goal

A single unversioned KEK in a Worker secret cannot rotate without
downtime or mass re-encryption, so it never rotates. This design makes
rotation routine: **versioned KEKs, version-pinned decrypt, online
re-wrap, per-environment key sets.** No downtime, no mass payload
re-encryption, no step that destroys the only copy of a live key.

Routine rotation here is **key aging, not incident response**: re-wrap
alone does NOT contain a KEK exposure (the same DEK still decrypts
today's payload for anyone holding the old KEK + an old snapshot). A
suspected or confirmed KEK exposure follows the compromise path in
§10 — never routine rotation alone.

## 1. Envelope layout

Per-user data-encrypting key (DEK), KEK-encrypted; credentials encrypted
under the DEK. Re-wrap re-encrypts only the DEK (small, constant-time
per row) — the credential payload is never re-encrypted during rotation.

```text
KEK[v] (Worker secret, 256-bit, never in repo/logs)
  └─ wraps DEK[user] (random 256-bit per user, generated at vault create)
       └─ encrypts credential payload (host/port/TLS/user/pass,
           max-connections, optional indexer URL + key)
```

Primitives: **AES-256-GCM** everywhere (96-bit random nonce per
encryption; `aes-gcm` crate, works on `wasm32` + native). Associated
data binds the row. The single source of truth is the code —
`workers/api/src/vault.rs` (`dek_aad`, `payload_aad`); the re-wrap (§6)
and compromise re-key (§10) jobs MUST call `vault::rewrap_dek` /
`vault::encrypt_vault` rather than rebuild AAD by hand:

```text
AAD(dek-wrap) = "v1|dek|<user_id>|<kek_version>"  (dek_aad)
AAD(payload)  = "v1|vault|<user_id>"              (payload_aad)
```

## 2. Schema (PlanetScale Postgres via Hyperdrive)

Merged reality (`migrations/0002_auth_vault.sql` +
`migrations/0015_kek_rotation.sql`): the table is `provider_vaults`,
blobs are base64 `TEXT` (Worker-friendly — no binary column binding
through Hyperdrive), and migration 0015 adds the rotation marker and
version index:

```sql
CREATE TABLE IF NOT EXISTS provider_vaults (
  user_id            TEXT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  kek_version        INTEGER NOT NULL,   -- version that wraps dek_wrapped
  dek_wrapped_nonce  TEXT NOT NULL,      -- base64 12-byte GCM nonce for the wrap
  dek_wrapped        TEXT NOT NULL,      -- base64 DEK encrypted under KEK[kek_version]
  payload_nonce      TEXT NOT NULL,      -- base64 12-byte GCM nonce for the payload
  payload            TEXT NOT NULL,      -- base64 credentials encrypted under DEK
  indexer_configured BOOLEAN NOT NULL DEFAULT FALSE,
  updated_at         BIGINT NOT NULL,
  rewrapped_at       BIGINT NULL       -- migration 0015: NULL = never re-wrapped
);
-- Migration 0015 (landed): rewrapped_at + version index.
-- ALTER TABLE provider_vaults ADD COLUMN IF NOT EXISTS rewrapped_at BIGINT NULL;
-- CREATE INDEX IF NOT EXISTS idx_vault_kek_version ON provider_vaults (kek_version);
```

Rules:

- `kek_version` is written on every insert and every re-wrap. It is the
  **only** version signal the decrypt path trusts (never inferred).
- Nonces are unique per encryption (CSPRNG). Never reused, never logged.
- Parameterized queries only. No plaintext credential column, now or as a
  migration backfill — migrations carry ciphertext only.
- Since migration 0015, `rewrapped_at` is the authoritative re-wrap
  marker: the §6 conditional UPDATE sets it (with `updated_at`) on every
  re-wrap, and `NULL` means the row has never been re-wrapped. Re-wrap
  passes still track pass-level progress in `rotation_runs` (§6);
  `updated_at` alone is no longer used as the re-wrap signal.

## 3. KEK provisioning and versioning

Provisioning has two modes (`wrangler.toml.example`, parsed by
`kek_set_from_lookup` in `workers/api/src/worker_entry.rs`):

- Legacy single-key mode (slice 1, still supported): one KEK per
  environment — `VAULT_KEK_BASE64` (32 random bytes, unpadded base64url,
  43 chars, decodes via `nntune_api::b64::decode`) plus
  `VAULT_KEK_VERSION` (the integer version stamped on every row; call it
  `L`). Active if and only if
  `VAULT_KEK_CURRENT` is unset; per-version secrets are ignored in this
  mode, so staging `V<L>` ahead of cutover never changes serving. A
  blanked `VAULT_KEK_CURRENT` (present-but-blank — empty, as a
  dashboard "clear this var" leaves behind, or whitespace-only from a
  stray paste) is not "unset": it refuses to start rather than falling
  back to legacy.
- Versioned mode: `VAULT_KEK_CURRENT` (a **non-secret** config var,
  e.g. `VAULT_KEK_CURRENT=2` in wrangler `vars`, not `secrets`,
  replacing `VAULT_KEK_VERSION` at cutover) names the write version,
  and the current write key is the `VAULT_KEK_V{current}` secret. Every
  other provisioned per-version secret is a retained prior for
  mixed-version reads. Legacy vars are ignored in this mode.

- At first cutover, the live legacy key is re-provisioned as a **separate
  per-version Worker secret**: `VAULT_KEK_V<L>` (same bytes as the legacy
  `VAULT_KEK_BASE64`, where `L` is the env's live legacy
  `VAULT_KEK_VERSION`), then `VAULT_KEK_V<L+1>`, … for later rotations —
  each 32 random bytes, unpadded-base64url
  at rest (`openssl rand 32 | basenc --base64url | tr -d '='`, 43 chars,
  decodes via `nntune_api::b64::decode`; generated inside the private
  operator secrets store, provisioned via `wrangler secret put` or the
  dashboard). Cutover is one deploy setting `V<L>` + `VAULT_KEK_CURRENT=<L>`
  alongside the legacy pair; the legacy `VAULT_KEK_BASE64` /
  `VAULT_KEK_VERSION` are deleted once V<L> reads green. Never assume
  `L=1`: an env whose legacy version is not 1 must cut over at its own
  live version, or every pre-existing row goes unreadable post-cutover.
- Discovery window: the `Env` API offers no key listing, so the Worker
  probes `VAULT_KEK_V<N>` for N in `CURRENT ± 8`
  (`KEK_VERSION_WINDOW`); anything outside stays unprovisioned, and
  reads for it fail closed per-row like any unknown version. The
  ceremony's live set (`{N-1, N}` steady state, `{N, N+1}` pre-staged)
  always fits inside.
- The Worker build **reads** every provisioned version within the
  `CURRENT ± 8` discovery window (current + up to 16 retained priors;
  fewer near the `u32` bounds) through `Config::kek_by_version` in
  `workers/api/src/handlers.rs`, so rows at mixed versions decrypt
  during phases 1–2, but **writes only** `VAULT_KEK_CURRENT`. There is
  no separate three-prior runtime cap. Retirement still requires the
  row-count and backup/PITR-horizon proof in §5 invariant 3; moving a
  version outside the window is not a substitute for that proof.
  A half-provisioned versioned config (missing, blank, or invalid
  current key; blank or unparsable `CURRENT`; blank or invalid retained
  prior) refuses to start rather than serving the wrong write version.
- Staging and production have **independent key sets generated
  independently** — never copy a KEK value between environments, never
  reuse a version number scheme as a shared secret. A rotation ceremony
  names its environment up front and touches exactly one.
- No KEK, DEK, nonce, or credential material in code, tests, fixtures,
  logs, error bodies, Analytics Engine, KV, R2, or Queue payloads.
  Fixtures use synthetic creds from the NNTP test-server corpus.

## 4. Encrypt / decrypt (version-pinned decrypt)

Encrypt (vault create/update — always the current version):

```rust
// pseudocode (Rust, aes-gcm)
let v = env("VAULT_KEK_CURRENT").parse::<u32>()?;
let kek = kek_by_version(v)?;                       // from Worker secret VAULT_KEK_V{v}
let dek = if existing { unwrap(row)? } else { random_32() };
let dek_nonce = random_12();
let dek_wrapped = aes_gcm_encrypt(kek, dek_nonce, dek_aad(user, v), dek)?;
let payload_nonce = random_12();
let ct = aes_gcm_encrypt(dek, payload_nonce, payload_aad(user), plaintext)?;
upsert(user, v, dek_wrapped, dek_nonce, ct, payload_nonce)
```

Decrypt (version-pinned — this is what makes rotation downtime-free:
rows at any retained version decrypt with their own version's key):

```rust
// pseudocode
fn unwrap(row) -> Result<DEK> {
    // Single candidate: the AAD binds row.kek_version, so only that
    // version's key can pass authentication. There is no fallback loop
    // and no "mid-re-wrap" state to straddle — the re-wrap UPDATE (§6)
    // is one conditional statement, so a row is always wholly at one
    // version. Every failure below returns the identical generic error.
    let kek = kek_by_version(row.kek_version).map_err(|_| generic("vault unavailable"))?;
    let dek = aes_gcm_decrypt(kek, row.dek_nonce,
                              dek_aad(row.user_id, row.kek_version),
                              row.dek_wrapped).map_err(|_| generic("vault unavailable"))?;
    Ok(dek)
}
```

Notes:

- AAD pins `row.kek_version`, so a ciphertext swapped between versions
  or users fails authentication (AO — tampering mitigation).
- Unknown `kek_version` (e.g. from a future write seen by an old
  release during rollout), a missing key, a bad tag, and "no row" all
  return the same generic error — never the version list, never which
  step failed.
- Non-owner callers get the same response for "no vault" as "not yours"
  (IDOR rule from the threat model); version errors must not create an
  existence oracle either.

## 5. Rotation ceremony (zero downtime, four phases)

| Phase | Action | Why it is safe |
|---|---|---|
| 1. Pre-stage | Generate KEK[N+1] in the operator store; `wrangler secret put VAULT_KEK_V{N+1}` on **one** environment; deploy Worker that **reads** N+1 but still **writes** N | New key present, nothing uses it yet; old release still fully functional |
| 2. Flip writes | Set `VAULT_KEK_CURRENT=N+1` (vars-only deploy). New/updated rows wrap under N+1; reads serve N and N+1 | Single config change; rollback = flip the var back, no re-encrypt |
| 3. Re-wrap (§6) | Background job re-wraps every `kek_version < N+1` row (DEK only) | Online; user traffic unaffected; idempotent, resumable |
| 4. Retire | When `SELECT count(*) WHERE kek_version < N+1` = 0 for one full re-wrap window **and** staging has baked **and** the retire-after date has passed (see invariants), delete `VAULT_KEK_V{N-1}` from the Worker (keep N as the single prior) — but KEEP the retired key material in the operator store until then | Old key destroyed only after proof nothing needs it — including restores |

Invariants (CISO gate checks these on the vault PR):

1. At least two KEK versions are provisioned from the first vault
   migration — day-one schema **and** day-one version-pinned decrypt
   code, not a follow-up.
2. The old KEK is never deleted before the re-wrap-complete query reads
   zero **twice** (two consecutive job passes).
3. Retire is tied to backup retention: backups, point-in-time windows,
   and database branches taken before the re-wrap still hold rows at
   version N-1, and restoring any of them after the delete would leave
   those rows undecryptable. So the retire-after date recorded in the
   version table is the LATEST of (re-wrap complete + one window,
   oldest restorable backup/PITR horizon expiry); retired key material
   stays in the operator store until that date. A restore dated before
   the re-wrap runs a re-wrap pass before writes reopen (§7).
4. Rollback at any point before phase 4 is "flip `VAULT_KEK_CURRENT`
   back + redeploy" — documented in the runbook, rehearsed on staging.
5. Staging rotates first, bakes ≥24h, then prod. Never both at once.

## 6. Re-wrap job

Runner: Cloudflare Workflow (cron) or Queue consumer in `workers/api`
(Rust). Paginated, idempotent, resumable — serverless-safe (no
in-memory cursors). No live re-wrap runner is implemented yet (§8);
this section is the normative contract a future runner must follow.

```sql
-- claim a batch (cursor = last user_id, never OFFSET on a hot table)
SELECT user_id, kek_version, dek_wrapped, dek_wrapped_nonce
  FROM provider_vaults
 WHERE kek_version < $1 AND user_id > $2
 ORDER BY user_id LIMIT 100;
```

Per row:

1. `DEK = decrypt(KEK[row.kek_version], dek_wrapped)` (version-pinned §4).
2. `dek_wrapped' = encrypt(KEK[current], fresh_nonce, dek_aad(user, current), DEK)`
   via `vault::rewrap_dek` (never rebuild AAD by hand).
3. Conditional write — a compare-and-swap on the captured envelope,
   not on user/version alone. A vault PUT replaces the whole envelope
   (fresh DEK and payload) at the same KEK version, so a version-only
   guard would still match the replacement row and a re-wrap computed
   from the captured envelope would install its old DEK over the newer
   payload, leaving that payload undecryptable. The claim SELECT above
   already captures `dek_wrapped` and `dek_wrapped_nonce`; bind those
   captured values (`$captured_wrapped`, `$captured_wrapped_nonce`)
   alongside the user and the captured version (`$old`):
   ```sql
   UPDATE provider_vaults
      SET kek_version = $new, dek_wrapped = $w,
          dek_wrapped_nonce = $n, updated_at = $now,
          rewrapped_at = $now
    WHERE user_id = $u AND kek_version = $old
      AND dek_wrapped = $captured_wrapped
      AND dek_wrapped_nonce = $captured_wrapped_nonce;
   ```
   Exactly one affected row is success: it rewrites only the wrap
   fields, so `payload` and `payload_nonce` are preserved untouched.
   **Zero affected rows is a lost claim, never successful progress**
   and never a reason to retry the same write unconditionally: the
   stored envelope is no longer the captured one (a same-version PUT
   or another re-wrap got there first), the stale re-wrap is
   discarded, and the row is reloaded and — if it still needs
   re-wrapping — retried against the current envelope within the
   bounded pass (§6 job properties). A lost claim is not counted in
   `rows_done`; overwriting the newer envelope with the stale result
   is forbidden.
4. On failure (a per-row error, as distinct from a lost claim above):
   count, continue (per-row errors never abort the pass;
   poison rows surface in the dead-letter metric, never with key bytes).

Inspect migration and re-wrap progress by KEK version with this count-only
query; it reports no user identifiers or ciphertext:

```sql
SELECT kek_version, COUNT(*) AS row_count
  FROM provider_vaults
 GROUP BY kek_version
 ORDER BY kek_version;
```

Job properties:

- Batch ≤100 rows, ≤1 concurrent pass per environment (distributed lock
  via a `rotation_state` row or Workflow singleton).
- Progress table (`rotation_runs`: id, env, from_version, to_version,
  cursor, rows_done, rows_failed, started/finished) — resumable after
  Worker eviction.
- Metrics to Analytics Engine: rows scanned/rewrapped/failed, pass
  duration — counts only, never ids-in-bulk, never key material.
- Audit log per pass (who/when/env/from→to/counts), not per row.
- Dry-run mode (`limit N, no writes`) for staging rehearsal.

## 7. Runbook (operator copy-paste)

> **NOT EXECUTABLE end to end — do not run the steps below in an
> incident.** The versioned KEK map HAS landed — `Config::kek_by_version`
> + `Config::with_prior_keks` (`workers/api/src/handlers.rs`) over
> `VAULT_KEK_V<N>` per-version secrets + the `VAULT_KEK_CURRENT`
> write-version var (`kek_set_from_lookup`,
> `workers/api/src/worker_entry.rs`), with legacy single-key
> compatibility — so pre-staging a version and flipping the write
> version (steps 3–4 mechanics) are implemented. The re-wrap job (§6
> runner: no Cloudflare Workflow/Queue consumer is implemented, §8)
> and the `rotation_runs` progress table have NOT landed, so no
> rotation can be carried through re-wrap/retire/close (steps 5–7):
> routine KEK rotation still cannot be executed. On suspected or
> confirmed KEK exposure, report it through GitHub's private
> vulnerability reporting (see `SECURITY.md`); §10 is design-only and
> has no runner. Today's executable containment is: legacy mode —
> replace `VAULT_KEK_BASE64`/`VAULT_KEK_VERSION` with a fresh single
> KEK so rows pinned to the exposed version fail closed; versioned
> mode — remove the exposed `VAULT_KEK_V<N>` from the Worker (material
> stays in the operator store) with `VAULT_KEK_CURRENT` on a live
> version, so those rows fail closed the same way. In both modes treat
> all vaulted credentials as exposed and force credential re-entry —
> never the steps below (the §10 per-row re-key still needs the runner
> that has not landed).

### Rotation executability today (Worker code as merged)

| Rotation | Executable today? | Worker env vars (grep proof) |
|---|---|---|
| Session secret (routine window) | ✅ Yes — dual-accept window per `docs/security/session-secret-rotation.md` | `SESSION_SECRET_BASE64`, `SESSION_SECRET_PREVIOUS_BASE64`, `SESSION_SECRET_ROTATION` — all read in `workers/api/src/worker_entry.rs` |
| Session secret (compromise) | ✅ Yes — single-secret cutover + `UPDATE sessions SET revoked = TRUE` (§6 of that doc) | same vars as above |
| KEK (routine re-wrap, §§5–6 below) | ❌ No — re-wrap job and `rotation_runs` not implemented (versioned KEK map + `CURRENT` flip landed; steps 5–7 still need the runner) | `VAULT_KEK_V<N>` + `VAULT_KEK_CURRENT` read in `worker_entry.rs` (`kek_set_from_lookup`), `kek_by_version` in `handlers.rs`, legacy pair still supported; `rotation_runs` appears nowhere in code or migrations (**not implemented**) |
| KEK (compromise re-key, §10) | ❌ No runner — incident path is design until the §8 gate items land | versioned map as above; per-row re-key still needs the runner (**not implemented**) |
| Native REST pepper | ⚠️ Reissue only — no rotation window; rotating the pepper means provisioning a new pair and reissuing credentials (rows pinned to the old `pepper_version` fail closed) | single `NATIVE_REST_PEPPER_BASE64` + `NATIVE_REST_PEPPER_VERSION` pair in `worker_entry.rs`; no second pair / previous-pepper var (**not implemented**) |

Preconditions: staging bake ≥24h on the new version; backup verified
(ciphertext restores); two operators aware (one executes, one can flip
back); rotation window announced.

```text
1. Confirm env: echo $ENV (staging|prod — exactly one; abort on ambiguity)
2. Generate:  openssl rand 32 | basenc --base64url | tr -d '='   # unpadded base64url, inside operator store only
3. Pre-stage: wrangler secret put VAULT_KEK_V<N+1> --env $ENV
              deploy reader release; verify /health + one test-connection
4. Flip:      set VAULT_KEK_CURRENT=N+1 in wrangler vars; deploy
              verify: create+read one staging vault row shows kek_version=N+1
5. Re-wrap:   trigger Workflow pass (dry-run first on staging);
              watch rotation_runs until rows_failed=0 and
              SELECT count(*) FROM provider_vaults WHERE kek_version < N+1; → 0
              re-run once more; require 0 twice
6. Retire:    record the retire-after date in the version table (latest
              of re-wrap-complete + one window, oldest restorable
              backup/PITR horizon expiry); only after that date:
              wrangler secret delete VAULT_KEK_V<N-1> --env $ENV (keep N
              as prior; retired material stays in the operator store
              until the retire-after date)
              deploy; verify reads still green
7. Close:     audit-log entry (env, N→N+1, counts, operator, date,
              retire-after);
              update this doc's version table below
ROLLBACK (any time before step 6): set VAULT_KEK_CURRENT back, redeploy,
              confirm reads green. No data migration to undo.
RESTORE (backup/PITR/branch predating the re-wrap): run a re-wrap
              pass over the restored rows BEFORE reopening writes, so no
              row stays pinned to a retired version.
```

Version table (update on every rotation):

| Env | Current | Priors retained | Last rotated | Retire-after (retention) | Notes |
|---|---|---|---|---|---|
| staging | — | — | — | — | fill at first rotation |
| prod | — | — | — | — | fill at first rotation |

## 8. What the vault PR must show (CISO gate checklist)

Slice-1 (merged: `migrations/0002_auth_vault.sql` + `workers/api` vault
core) already shows:

- [x] `kek_version` on every row from migration 1; base64 `TEXT` blobs
      (`provider_vaults`, not `vault_rows` — see §2).
- [x] Encrypt writes current only (`VAULT_KEK_VERSION`); decrypt is
      version-pinned per row (`kek_version` + AAD) with generic errors
      and no existence oracle; `vault::rewrap_dek` re-wraps DEK-only.
- [x] Staging and prod KEK sets independently generated (operator
      attestation, never values).
- [x] `gitleaks` + `ci-ok` + `pr-lint` green; no secret/key/credential
      in code, tests, fixtures, logs, or error bodies.

Follow-up gate items (the vault/HTTP slice PR may not merge until the
CISO gate verifies each at its exact head):

- [x] Follow-up migration: `rewrapped_at` + `kek_version` index (§2)
      — `migrations/0015_kek_rotation.sql`; the §6 re-wrap UPDATE sets
      `rewrapped_at`, and per-version progress counts are documented
      in §6.
- [x] `kek_by_version` selector over provisioned versions so mixed-version
      rows decrypt during phases 1–2; first rotation cuts over to
      `VAULT_KEK_V<N>` + `VAULT_KEK_CURRENT` (§3) —
      `Config::kek_by_version` + `Config::with_prior_keks`
      (`workers/api/src/handlers.rs`), versioned/legacy env parsing
      (`kek_set_from_lookup`, `workers/api/src/worker_entry.rs`),
      legacy single-key mode preserved.
- [ ] Re-wrap job present, idempotent, resumable, with poison-row
      handling and count-only telemetry; re-wrap UPDATE binds the new
      AAD (§6 step 2).
- [ ] Runbook §7 followed on staging (var flip + dry-run re-wrap pass);
      rollback rehearsed (flip-back deploy).
- [ ] Retire-after date recorded and honored (backup/PITR-aware, §5
      invariant 3); retired material retained in the operator store.
- [ ] Compromise path §10 implemented and drill-documented (it is a
      gate item, not a non-goal).

## 9. Non-goals

- HSM / external KMS integration (a later card may move KEKs; the
  version column and re-wrap path survive that move unchanged).
- Automatic time-based rotation (rotations are explicit ceremonies until
  the job has production history).
- Cross-environment replication (explicitly forbidden — see §3).

## 10. Compromise rotation (incident response) — DESIGN ONLY, no runner

> **DESIGN ONLY — no runner.** Steps 1–2 below need the re-wrap
> runner, which does not exist in Worker code or migrations (the
> versioned KEK map — `VAULT_KEK_V<N>` + `VAULT_KEK_CURRENT` — HAS
> landed, §7). On suspected or confirmed exposure, report through
> GitHub's private vulnerability reporting (see `SECURITY.md`) and
> apply today's executable containment from §7 (fresh single KEK in
> legacy mode, or remove the exposed `VAULT_KEK_V<N>` with
> `VAULT_KEK_CURRENT` on a live version, so exposed-version rows fail
> closed; treat vaulted credentials as exposed, force re-entry).

Routine re-wrap (§§5–6) rotates the WRAPPING, not the data key: anyone
holding an exposed KEK[N] plus any pre-rotation snapshot still unwraps
the same DEK that decrypts today's payload. On suspected or confirmed
KEK exposure, do NOT run a routine rotation and call it contained.
Instead:

1. Provision KEK[M] (fresh version, one environment at a time) and flip
   `VAULT_KEK_CURRENT=M` so no new row touches the exposed version.
2. Per row, in ONE conditional UPDATE guarded by the same
   captured-envelope compare-and-swap as §6 (`user_id`, captured
   `kek_version`, captured `dek_wrapped` and `dek_wrapped_nonce` —
   never a timestamp-only guard): decrypt the payload with the OLD DEK,
   generate a NEW random DEK, re-encrypt the payload under the new DEK,
   wrap the new DEK under KEK[M] with `dek_aad(user, M)` (via
   `vault::encrypt_vault`, never hand-built AAD), bump
   `kek_version` to M. Zero affected rows is a lost claim as in §6:
   a concurrent vault update replaced the captured envelope (possibly
   at the same KEK version and within the same `updated_at` tick), so
   the stale re-key is discarded, the row is reloaded and re-driven
   against the current envelope, and the newer envelope is never
   overwritten.
3. Treat provider and indexer credentials in ANY pre-rotation snapshot,
   backup, replica, or branch as EXPOSED: affected users re-enter (or
   rotate at the provider/indexer) their credentials; the old rows are
   never trusted again.
4. Audit-log entry (env, exposed version → M, rows re-keyed, operator,
   date); post-incident review tracks why the KEK left its boundary.
5. Retire the exposed version only under the §5 invariant-3 retention
   rule — restores of older snapshots still need it to run THEIR
   compromise re-key.

Drill: rehearse steps 1–2 on staging (dry-run + live pass) before the
vault PR merges, so the path exists before it is ever needed.
