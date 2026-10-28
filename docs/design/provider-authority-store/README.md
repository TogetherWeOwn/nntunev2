# Provider authority store: inert records and transaction contract

**NON-AUTHORITATIVE / NOT INSTALLED.** Definition and model only. Nothing here is
a migration, an API, a writer integration or proof of readiness.

## Files

- `schema.sql`: definition-only tables for the origin and plan records, stable
  step identity, JTI/reservation/lease, replay nonce, fence/tombstone and the
  secret-free audit outbox. It lives outside `migrations/` on purpose and must
  never be referenced by a migration runner, workflow, test harness or include.
- `transaction-contract.json`: every transaction and all 32 writers of the
  accepted contracts, each marked `integrated: false`, with the shared lock/CAS
  key set, the snapshot, the linearization point and the post-unwrap and
  post-audit rereads. The outbox is not the synchronous fence.
- `crates/nntune-core/src/provider_authority_store.rs`: a deterministic,
  detached model (not declared in `lib.rs`, no caller). It is reachable only
  from the acceptance adapter under `tests/support/`.

## Uniqueness and replay scopes (recorded, not guessed)

| Record | Key |
| --- | --- |
| Mapping | `(env, authorization_id)`, `(env, play_intent_id)`, `(env, playback_request_id)`, each unique and mutually distinct |
| Step identity | `(env, parent_id, step_id)` content, `(env, scope_id, step_id)` connection |
| JTI | `(env, issuer, jti)` |
| Replay nonce | `(env, caller, kid, kv, nonce)` |
| Tombstone | `(env, parent_id)`, `retain_until >= closed_at + 86400` |

## What is not proven

Real PostgreSQL syntax, locking and races, installed privileges, any real writer
participation, deployment and runtime readiness. The model only shows the
intended ordering with a single in-memory record.
