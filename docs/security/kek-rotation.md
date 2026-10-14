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
data binds the row:

```text
AAD(dek-wrap) = "nntune-vault/v1" || user_id || kek_version
AAD(payload)  = "nntune-vault/v1" || user_id
```

## 2. Schema (PlanetScale Postgres via Hyperdrive)

```sql
CREATE TABLE vault_rows (
  user_id            TEXT PRIMARY KEY,
  kek_version        INTEGER NOT NULL,          -- version that wraps dek_wrapped
  dek_wrapped        BYTEA NOT NULL,            -- DEK encrypted under KEK[kek_version]
  dek_nonce          BYTEA NOT NULL,            -- 12-byte GCM nonce for the wrap
  payload_ciphertext BYTEA NOT NULL,            -- credentials encrypted under DEK
  payload_nonce      BYTEA NOT NULL,            -- 12-byte GCM nonce for the payload
  created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
  rewrapped_at       TIMESTAMPTZ NULL,          -- last rotation re-wrap time
  CONSTRAINT kek_version_positive CHECK (kek_version >= 1)
);
CREATE INDEX vault_rows_kek_version_idx ON vault_rows (kek_version);
```

Rules:

- `kek_version` is written on every insert and every re-wrap. It is the
  **only** version signal the decrypt path trusts (never inferred).
- Nonces are unique per encryption (CSPRNG). Never reused, never logged.
- Parameterized queries only. No plaintext credential column, now or as a
  migration backfill — migrations carry ciphertext only.

## 3. KEK provisioning and versioning

- Each KEK version is a **separate Worker secret**: `VAULT_KEK_V1`,
  `VAULT_KEK_V2`, … — each 32 random bytes, base64-encoded at rest
  (`openssl rand -base64 32`, generated inside the private operator
  secrets store, provisioned via `wrangler secret put` or the dashboard).
- The write version is a **non-secret** config var, e.g.
  `VAULT_KEK_CURRENT=2` (wrangler `vars`, not `secrets`).
- The Worker build knows how to **read** every provisioned version
  (current + retained priors, ≤3 retained) but **writes only**
  `VAULT_KEK_CURRENT`.
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
let dek_wrapped = aes_gcm_encrypt(kek, dek_nonce, aad_wrap(user, v), dek)?;
let payload_nonce = random_12();
let ct = aes_gcm_encrypt(dek, payload_nonce, aad_payload(user), plaintext)?;
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
                              aad_wrap(row.user_id, row.kek_version),
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
in-memory cursors).

```sql
-- claim a batch (cursor = last user_id, never OFFSET on a hot table)
SELECT user_id, kek_version, dek_wrapped, dek_nonce
  FROM vault_rows
 WHERE kek_version < $1 AND user_id > $2
 ORDER BY user_id LIMIT 100;
```

Per row:

1. `DEK = decrypt(KEK[row.kek_version], dek_wrapped)` (version-pinned §4).
2. `dek_wrapped' = encrypt(KEK[current], fresh_nonce, aad_wrap(user, current), DEK)`.
3. Conditional write (optimistic concurrency — safe under retries):
   ```sql
   UPDATE vault_rows
      SET kek_version = $new, dek_wrapped = $w, dek_nonce = $n,
          rewrapped_at = now(), updated_at = now()
    WHERE user_id = $u AND kek_version = $old;
   ```
4. On failure: count, continue (per-row errors never abort the pass;
   poison rows surface in the dead-letter metric, never with key bytes).

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

Preconditions: staging bake ≥24h on the new version; backup verified
(ciphertext restores); two operators aware (one executes, one can flip
back); rotation window announced.

```text
1. Confirm env: echo $ENV (staging|prod — exactly one; abort on ambiguity)
2. Generate:  openssl rand -base64 32   # inside operator store only
3. Pre-stage: wrangler secret put VAULT_KEK_V<N+1> --env $ENV
              deploy reader release; verify /health + one test-connection
4. Flip:      set VAULT_KEK_CURRENT=N+1 in wrangler vars; deploy
              verify: create+read one staging vault row shows kek_version=N+1
5. Re-wrap:   trigger Workflow pass (dry-run first on staging);
              watch rotation_runs until rows_failed=0 and
              SELECT count(*) FROM vault_rows WHERE kek_version < N+1; → 0
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

- [ ] Schema has `kek_version` (+ nonces, `rewrapped_at`) from migration 1.
- [ ] Encrypt writes current only; decrypt implements §4 version-pinned
      decrypt with generic errors and no existence oracle.
- [ ] Two KEK versions provisioned in staging at review time; rotation
      phases 1–3 demonstrated (var flip + dry-run re-wrap pass).
- [ ] Re-wrap job present, idempotent, resumable, with poison-row
      handling and count-only telemetry; re-wrap UPDATE binds the new
      AAD (§6 step 2).
- [ ] Runbook §7 followed on staging; rollback rehearsed (flip-back deploy).
- [ ] Retire-after date recorded and honored (backup/PITR-aware, §5
      invariant 3); retired material retained in the operator store.
- [ ] Compromise path §10 implemented and drill-documented (it is a
      gate item, not a non-goal).
- [ ] Staging and prod KEK sets independently generated (operator
      attestation, never values).
- [ ] `gitleaks` + `ci-ok` + `pr-lint` green; no secret/key/credential in
      code, tests, fixtures, logs, or error bodies.

## 9. Non-goals

- HSM / external KMS integration (a later card may move KEKs; the
  version column and re-wrap path survive that move unchanged).
- Automatic time-based rotation (rotations are explicit ceremonies until
  the job has production history).
- Cross-environment replication (explicitly forbidden — see §3).

## 10. Compromise rotation (incident response)

Routine re-wrap (§§5–6) rotates the WRAPPING, not the data key: anyone
holding an exposed KEK[N] plus any pre-rotation snapshot still unwraps
the same DEK that decrypts today's payload. On suspected or confirmed
KEK exposure, do NOT run a routine rotation and call it contained.
Instead:

1. Provision KEK[M] (fresh version, one environment at a time) and flip
   `VAULT_KEK_CURRENT=M` so no new row touches the exposed version.
2. Per row, in ONE conditional UPDATE keyed on (`user_id`,
   `kek_version`, `updated_at`): decrypt the payload with the OLD DEK,
   generate a NEW random DEK, re-encrypt the payload under the new DEK,
   wrap the new DEK under KEK[M] with `aad_wrap(user, M)`, bump
   `kek_version` to M. Retry-safe via the same claim-and-conditional-write
   pattern as §6 (a concurrent vault update changes `updated_at` and
   loses the race on purpose — it is re-driven, never overwritten).
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
