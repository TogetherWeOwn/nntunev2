# Own-device presence session adapter

## Authority

- The [presence core](../../workers/api/src/realtime_presence.rs) accepts
  heartbeats and exposes account-local order and a caller flag in snapshots.
- Verify the signed session token on every operation to locate the candidate
  owner and login. Revalidate against the current authoritative Postgres row
  via the cache-disabled auth path.
- In one read-committed transaction, lock the live, non-deleting user with
  `FOR NO KEY UPDATE`, then load the checkpoint and fetch the caller and
  eligible peers by owner and explicit IDs, in ID order with `FOR SHARE`.
  Sample server time after lock waits and reject time below the durable
  account watermark. The proof supplies fixed server times.
- The core invokes `verify_stored_session` for the caller, checking login,
  owner, creation time, rotation, expiry and revocation. A missing or invalid
  caller fails the operation. A missing or invalid peer is offline. Hold locks
  through commit. Query, lock or commit failures fail closed.

## Sizing

- Each login has an independent sequence through refresh. Duplicate and lower
  frames leave the deadline at the earlier of the accepted heartbeat plus
  45 seconds and its credential expiry. Snapshots also honor current peer expiry.
- The 32 slots include offline replay markers. Existing logins can advance
  at capacity. The 33rd login waits until a marker reaches the authenticated
  deadline, `created_at + 86,400 seconds`. This is a registration bound
  for the rolling window, including revoked and missing sessions.
- A committed `sessions.revoked = TRUE` row matching owner, login and creation
  time is durable evidence of revocation. Logout, account-wide revoke and
  reauthentication write that evidence. A rolled-back revoke, missing row,
  old cookie expiry or refresh counter change cannot justify reclaim. This
  boundary retains even revoked markers. Early reclaim would require revocation
  to remain effective for that login through its deadline, including recovery.
- A heartbeat reads one session row and a snapshot at most 33 in one batch.
  The proof uses four statements plus transaction control. A fixed-width
  checkpoint needs 1,728 bytes of entry data (32 × 54 bytes), plus account,
  order, watermark and storage overhead. The fixture caps JSON at 8 KiB.

## Evidence and wiring

The [disposable PostgreSQL fixture](../../workers/api/src/realtime_presence_postgres_tests.rs)
uses the auth and deletion migrations, two synthetic accounts, signed
logins, and the production single-session revoke SQL across connections.

Before wiring, qualify the transactional checkpoint, erasure, idle cleanup,
post-lock clock and runtime budget. Fence lost checkpoints until their logins
are invalidated or past deadline. Qualify lock ordering with account-wide
revokes, which can acquire session locks without the user lock.
