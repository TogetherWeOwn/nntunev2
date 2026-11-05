# ADR 0008: Accounts, Auth, Sessions, Credential Vault

- **Status:** Accepted (slice 1 implemented; slice 2 shipped 2026-10-19 — see status note)
- **Date:** 2026-10-13
- **Revised:** 2026-11-03 — the credential vault is re-homed from per-user
  BYOP credentials to service pool credentials
  ([ADR 0014](0014-backend-provider-pool.md)). There are no user providers:
  the per-user vault (`provider_vaults`, the `/v1/vault` endpoints and the
  user-facing `test connection` probe) is retired as target behaviour, and
  stored user credentials are purged by a forward migration, never migrated
  into the pool. What survives unchanged is the envelope itself — KEK-versioned
  AES-256-GCM with per-row DEKs, KEK rotation/re-wrap, booleans-only reads and
  the dial-time SSRF guard — now homing `pool_providers.secret_ref` rows
  (AAD owner binding is the pool account id, not a `user_id`; legacy
  user-bound rows are destroyed, not re-bound) and the Subsonic token
  envelopes of ADR 0006. Pool credentials are created and rotated only
  through an admin-only path, decrypted in memory at connection time, and
  never exposed to users, clients or the Subsonic surface. Accounts,
  sessions, passkeys and magic links below are unaffected; the slice-1/2
  implementation statements are historical record of what shipped.

## Status note — 2026-10-19: slice 2 shipped

Slice 2 is implemented at `origin/main`: the Worker HTTP entry
(`workers/api/src/worker_entry.rs:1888`, `pub async fn dispatch`,
reachable from the `#[worker::event(fetch)]` entry in
`workers/api/src/lib.rs`) serves auth traffic over the Hyperdrive
Postgres store, and WebAuthn signature verification is live via
`workers/api/src/webauthn.rs:444` (`verify_attestation`) and
`workers/api/src/webauthn.rs:505` (`verify_assertion`), called from the
passkey handlers. The "no Worker runtime yet" / "follow-up slice"
statements in Context/Decision/Consequences below describe slice-1 time
and are superseded by this note; each carries a pointer back here.

## Context

Accounts need signup/login with passkeys (WebAuthn) plus an email magic-link
fallback, sessions on signed cookies with rotation, and optional invite
codes. At decision time, per-user BYOP credentials (Usenet provider
host/port/TLS/user/pass/max connections, optional Newznab indexer URL + API
key) needed envelope encryption with a per-user DEK and a KEK from the Worker
secret — never logged, never returned after save — plus a `test connection`
endpoint via the stream pool. Since the 2026-11-03 revision above, the
credentials under envelope are the service's own pool accounts (ADR 0014);
no credential is user-supplied. Relational data lives in PlanetScale
Postgres via Hyperdrive. The STRIDE threat model (§3–§4) is the binding
security spec.

## Decision

1. **Slice 1 (this change):** pure-Rust core (`workers/api`: `session`,
   `auth`, `vault`) + Postgres schema (`migrations/0002_auth_vault.sql`),
   native-tested, wasm32-compatible. No Worker runtime imports yet
   (slice-1 time; the Worker entry has since shipped — see status note).
2. **Vault:** AES-256-GCM envelope (per-row DEK — per-user at decision time,
   per pool account since the 2026-11-03 revision; KEK versioned, staging and
   prod never shared); AAD binds the row owner (`user_id` for legacy rows,
   pool account id for pool rows; + KEK version on the wrap);
   reads return booleans only; connection probes return a boolean only;
   audit carries ids/results, never secrets; SSRF guard resolves ALL DNS
   records and rejects non-public ranges (v4+v6), including IPv6 translation
   and transition space; validation returns the vetted target addresses.
   The Worker must revalidate at connection time and pin the dial/fetch to
   that set (no second DNS resolution), retaining the original TLS hostname;
   timeouts are code constants and connection caps are the pool account's
   `max_connections` budget (ADR 0014), not per-user settings.
3. **Sessions:** HMAC-SHA256 cookies (`HttpOnly; Secure; SameSite=Lax`),
   immutable authentication time with a one-day absolute cap, rotation
   counter and server-side row verification (`verify_stored_session`).
   Refresh requires a conditional counter update before the cookie is sent;
   privilege changes revoke the old row and issue a new id after reauthentication.
   Lost-response recovery is opt-in and requires a separately retained,
   client-generated 256-bit retry secret bound to the exact predecessor cookie.
   An atomic row claim may reissue only the immediate successor within 120
   seconds of its first serialized database receipt claim, subject to both
   tokens' expiry and the current row's revocation/counter. Token issuance
   retains the already-validated Worker candidate timestamp, separately from
   the database receipt clock; returned token structure is checked before
   commit. The transaction locks the row before sampling the database claim
   clock; request arrival time is not the deadline or serialization boundary.
   HTTP opt-in also requires the bound-refresh Content-Type marker outside
   the body, so an aborted upload surfaced as empty EOF cannot downgrade the
   operation. Staging deployment verifies the actual binding's required
   receipt schema read-only before deploying; it never applies migrations. Cookie-client adoption additionally requires
   cross-tab single-flight for every cookie writer: all HTTP attempts must
   settle before another rotation or secret disposal. Abort/timeout alone is
   not settlement; uncoordinated older tabs preclude opt-in. Duplicate response
   headers are not generation-safe in a same-name browser cookie jar.
   There is no stale-cookie grace period: ordinary authentication and
   empty-body refresh still reject old counters.
   See `workers/api/README.md` for the protocol and migration ordering.
4. **Magic links:** 256-bit random, SHA-256 at rest, 15-minute expiry,
   one generic failure message. Single-use is a database invariant, not
   an in-memory flag: the atomic entry points require `SingleUseStore`
   conditional UPDATE/RETURNING and exactly one returned row. The Worker
   adapter must commit that claim (with session/signup insertion where
   applicable) before returning success; database wiring was slice 2
   (shipped — see status note).
5. **Passkeys:** challenge lifecycle (RP equals exact host, canonical
   serialized origin; http only on exact loopback dev hosts; 5-minute
   single-use) + credential storage shapes now; attestation/assertion
   signature verification + HTTP/Hyperdrive Worker entry in the follow-up
   slice (shipped in slice 2 — see status note).

## Consequences

- Slice 1 is verifiable today (`cargo test`, clippy, fmt) with zero secret
  handling in CI (synthetic fixtures only).
- The follow-up slice added the Worker HTTP entry, Hyperdrive queries
  (parameterized only), WebAuthn signature verification, and the bounded
  direct-socket `test connection` probe (`SocketDialer`, validated-hostname
  dial (no peer-IP pin: workerd reports the dial string as
  SocketInfo.remote_address) — not routed through the stream pool; all
  shipped in slice 2 — see status note); it re-requests CISO review as a
  vault/auth-touching PR.
