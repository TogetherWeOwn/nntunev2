# Offsite backup runbook (encrypted library)

Proven-restore companion to `docs/database-restore-runbook.md`. That
document owns the forward-fix-vs-restore policy; this one owns the
**offsite copies**: what is backed up, where it lands, how long it is
kept, who can touch it, and how a restore is proven before anyone
relies on it.

Standing rule, repeated because it matters: no NZB bytes, message-IDs,
subjects, file names, segment lists, account hostnames/usernames, keys,
nonces or article references appear in the repo, in PRs, on cards, or
in drill evidence. Evidence carries timestamps, object counts and
hashes only.

## 1. Scope

Every backup covers the database **and** the KEK keyring together. A
database backup without the keyring that can open its envelope columns
is a pile of opaque bytes; a keyring backup without the database is a
pile of keys with nothing to open. The unit of restore is the pair.

Covered material:

- Envelope tables (`track_sources`, `track_media`, `provider_vaults`,
  `providers`, `cache_objects`): `pg_dump` in the
  `--format=directory --compress` shape the operator stores under
  `offsite/<env>/db/`, plus per-table row counts recorded alongside.
- Wrapped per-file keys: they travel inside the envelope columns
  (`nzb_ref_dek_wrapped`, …); there is no separate per-file-key
  export. If a row is in the dump, its wrapped DEK is in the dump.
- Keyring coverage manifest (`offsite/<env>/keyring/manifest.json`):
  KEK **versions and their active date ranges only** — never key
  material. It answers "which KEK versions does this backup need?"
  (`keyring.versions` in the bundle manifest mirrors it).
- KEK material itself: operator-store → R2 path
  `offsite/<env>/keyring/v<V>.sealed`, sealed to the operator's
  offline recovery key, one object per KEK version. KEK values never
  appear in Worker secrets dumps, logs, cards or evidence.

Explicitly out of scope: Usenet article bodies (re-fetchable from the
provider pool while NZB references survive), derived caches
(rebuildable), and anything under the MetaBrainz NC-licensed feeds
(never ingested — see the repo guardrails).

## 2. Where it lands

R2 paths only; no Worker binds the archive buckets (the CI archive
guard stays green — copies are written by the operator S3 path, not by
Workers):

- `offsite/staging/db/<YYYY-MM-DD>/…` — staging dumps.
- `offsite/staging/keyring/manifest.json`, `offsite/staging/keyring/v<V>.sealed`.
- `offsite/prod/db/<YYYY-MM-DD>/…`, `offsite/prod/keyring/…` — same
  shape once the prod launch DB exists (deferred; see `docs/ops.md`).

## 3. Retention and access

- Daily DB dumps: 30-day rolling.
- Monthly DB snapshots (first-of-month): 12-month rolling.
- Keyring objects: retained per `docs/security/kek-rotation.md`
  invariant 3 — retire-after is the **latest** of (re-wrap-complete +
  one rotation window) and (oldest restorable backup / PITR horizon
  expiry). In practice: a KEK version's sealed object outlives the
  newest backup that still references it, then one window more.
  Retired material stays in the operator store; it is never deleted on
  rotation day.
- Access: operator role only (break-glass documented in the operator
  store, not here). Reads and writes are logged by the operator
  account; there is no shared credential and no copy of KEK material
  outside the operator store and the live Worker secrets.

## 4. The drill (repeatable)

Two legs. The synthetic leg runs anywhere, including CI; the staging
leg needs the staging lane and a posted fixture and is operator-run.

### 4a. Synthetic leg — `backup-drill synthetic`

```sh
python3 scripts/cargo_cache.py run -- build -p nntune-backup
./target/debug/backup-drill synthetic --bytes 209053 --tool TWO-1772 > /tmp/drill-evidence.json
```

What it does (`crates/nntune-backup/src/drill.rs`): seals a random
fixture (ephemeral KEK → random per-file media key → rclone-crypt
ciphertext via `nntune-core` → v2 envelope row), writes the bundle,
wipes every drill-owned copy (KEK, media key, derived — the caller's
fixture buffer is kept only for the final hash comparison), restores
the bundle into an isolated directory,
re-verifies it there, opens the envelope with the recovered keyring
only, decrypts the full plaintext through two ranged reads
(article-window shaped), and compares SHA-256. Exit 0 prints the
evidence JSON; exit 2 means the drill failed; exit 1 is a usage/IO
error. Temp dirs are removed afterwards — the drill proves restore
from the bundle, it does not keep one.

Evidence shape (`DrillEvidence`, pinned by
`tests/drill_evidence.rs`): `tool`, `bundle_format_version`, `env`
(`"synthetic"`), `started_at_unix`, `finished_at_unix`, per-table row
counts, `blob_bytes`, `cipher_blocks`, `keyring_versions` (numbers
only), `plaintext_bytes`, `plaintext_sha256`, `restored_sha256`,
`hash_match`, `bundle_manifest_sha256`. No key, nonce, ciphertext,
wrap, article or NZB-adjacent field exists on the struct, so
serialization cannot leak one.

### 4b. Staging leg — restore into an isolated environment

1. Operator provisions an isolated staging restore target (fresh
   database + scratch disk; no traffic, no pool binding).
2. Operator copies the newest `offsite/staging/db/…` dump and the
   keyring objects for exactly the versions in that backup's
   manifest. Anything else fails closed at step 4.
3. Restore the dump; run the KEK re-wrap pass first if the backup
   predates a rotation (`docs/security/kek-rotation.md`,
   `docs/database-restore-runbook.md`).
4. Verify every `track_sources` envelope row opens — authentication
   only, no plaintext is printed. `track_sources` is the wrapped
   per-file-key table that gates library recovery; provider credential
   envelopes use a different table/AAD binding and are verified by
   their own open path when first used post-restore. Presence of every
   backed-up table is still covered by the dump restore plus the
   per-table row counts recorded in step 5.
   ```sh
   umask 077
   SCRATCH="$(mktemp -d)"
   trap 'shred -u "$SCRATCH/rows.jsonl" 2>/dev/null; rm -rf "$SCRATCH"' EXIT
   # One JSON object per line via row_to_json: the verifier parses
   # JSONL, not the tab-delimited text a bare COPY (SELECT …) emits
   # (every such row would fail closed as Shape). Key names must match
   # the verifier's row shape exactly.
   psql "$RESTORE_DSN" -t -A -c "COPY (SELECT row_to_json(t) FROM (SELECT 'global' AS user_id, \
     track_id::text AS row_key, \
     nzb_ref_envelope_version AS envelope_version, \
     nzb_ref_payload_format AS payload_format, nzb_ref_kek_version AS kek_version, \
     encode(nzb_ref_dek_wrapped_nonce,'hex') AS dek_wrapped_nonce_hex, \
     encode(nzb_ref_dek_wrapped,'hex') AS dek_wrapped_hex, \
     encode(nzb_ref_payload_nonce,'hex') AS payload_nonce_hex, \
     encode(nzb_ref_encrypted,'hex') AS ciphertext_hex \
     FROM track_sources) t) TO STDOUT" > "$SCRATCH/rows.jsonl"
   BACKUP_DRILL_KEK_V1="$(op read …)" BACKUP_DRILL_KEK_V2="$(op read …)" \
     ./target/debug/backup-drill verify --rows "$SCRATCH/rows.jsonl" > "$SCRATCH/verify-report.json"
   ```
   Post-migration-0027 notes: `track_sources` has no `user_id` column
   (one global slot per `track_id`), no `row_key` column, and the
   ciphertext column is `nzb_ref_encrypted`. `'global'` is the sealer's
   tagged global `owner_scope` per ADR 0002 and `track_id::text` the
   `row_key`; both must equal the seal-time AAD inputs (pinned where
   the pipeline seals) — any mismatch fails closed as failed rows,
   never as a silent pass. The export lives in a `0700` scratch dir,
   never world-readable `/tmp` files, and is shredded on exit.
   KEKs arrive as 64 hex chars via environment (operator-store read,
   never a file, never a log); exit 2 on any failure, including zero
   rows. The report carries `rows`, `ok`, `failed` and
   `keyring_versions` only.
5. Decrypt one posted fixture track end to end from the restored
   material only (fetch its articles through the normal ranged path
   against the isolated target, decrypt, compare SHA-256 with the
   pre-posting hash recorded at ingest). Record: restore timestamp,
   dump date, object/row counts, KEK versions used, both hashes, and
   the match. Never the article references, keys, or plaintext.
6. Tear the isolated target down.

## 5. Gate evidence for the pointer-switch (TWO-1769)

The real-catalogue switch stays behind its explicit admin/config gate
until **both** drill legs are recorded on TWO-1772: the synthetic
evidence JSON (timestamps, counts, `hash_match: true`) and the staging
leg record (§4b step 5). The Security Engineer reviews backup scope
plus both evidence sets before the card closes; the gate keys on that
review, not on this document.
