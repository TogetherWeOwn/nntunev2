# nntune-api: accounts, auth, sessions, BYOP vault

Rust core + Worker HTTP entry for NNTune v2 accounts/auth (passkeys +
email magic link), signed-cookie sessions, and the BYOP credential
vault. Compiles to native (tests) and `wasm32` (Cloudflare Worker).

## Layout

```text
workers/api/
  src/lib.rs          # crate root + Worker fetch event
  src/session.rs      # HMAC-signed cookies: issue / verify / rotate
  src/auth.rs         # magic links, passkey challenge lifecycle, invite codes
  src/vault.rs        # envelope encryption, validation, SSRF guard, redaction
  src/webauthn.rs     # attestation/assertion verification (ES256 + EdDSA)
  src/handlers.rs     # framework-agnostic HTTP handlers over store traits
  src/worker_entry.rs # Hyperdrive Postgres, DoH DNS, socket dialer, mailer
  src/encoding.rs     # base64url helpers
  tests/              # acceptance + security-regression tests
  wrangler.toml.example  # Worker config template (no secrets)
```

`migrations/0002_auth_vault.sql` plus `0004` (challenge purpose) and `0005`
(atomic rate-limit slots and test leases) define the Postgres schema
(PlanetScale via Hyperdrive). All queries are parameterized; single-use claims execute
the conditional `UPDATE ... RETURNING` prepared by the core and require
exactly one row before any response.

## Routes (all JSON, `Cache-Control: no-store`)

| Route | Purpose |
|---|---|
| `GET /healthz` | liveness only (no config detail) |
| `POST /v1/auth/passkey/register/start` / `finish` | passkey signup (purpose-bound challenge + emailed-token inbox proof; attestation `none` or packed self-attestation) |
| `POST /v1/auth/passkey/login/start` / `finish` | usernameless passkey login (empty `allow_credentials`; assertion + sign-count clone detection) |
| `POST /v1/auth/magic/start` / `finish` | email magic link (atomic per-email issuance reservation shared with register inbox proof) |
| `POST /v1/session/refresh` / `logout` / `revoke-all` | stored-session rotation/revocation |
| `GET` / `PUT` / `DELETE /v1/vault` | booleans-only summary / encrypted upsert / delete |
| `POST /v1/vault/test-connection` | `{"ok": bool}` only; atomically rate-limited, per-user concurrency lease, bounded pinned-address dial |

Generic errors only: auth `401 {"error":"check your email and try
again"}`, vault `400 {"error":"vault unavailable"}`, internal
`500 {"error":"internal error"}`. Cookie-authenticated mutations also
require an exact `Origin` header match.

Known limits of this slice: attestation `x5c` chains reject (no CA
trust store yet); test-connection dials the vetted IP literal, so a
provider certificate issued only for the hostname fails closed as
`ok:false` on TLS ports; magic-link delivery needs the operator's
`EMAIL_WEBHOOK_URL` (without it links are issued but not delivered, and
passkey signup cannot complete its inbox-proof step);
test-connection is a direct bounded dial, not yet routed through the
stream pool. Staging deploy (worker-build plumbing + per-env wrangler
config from secrets) is a follow-up slice.

## Security contract (threat-model §3–§4)

- **Envelope encryption:** per-user random 256-bit DEK, wrapped by the KEK
  (Cloudflare Worker secret, versioned). AES-256-GCM, random 96-bit nonces,
  AAD binds `user_id` (+ KEK version for the wrap). Row-swap between users
  fails to decrypt. Rotation via `rewrap_dek` without touching ciphertext.
- **Never logged, never returned:** reads return booleans only
  (`redacted_summary`); `test connection` returns a boolean, never provider
  banners; audit events carry ids/results only — there is no field for
  secret material anywhere in the API or schema.
- **Sessions:** `HttpOnly; Secure; SameSite=Lax`; immutable `created_at`
  caps the lifetime at one day even across refreshes. `verify_session`
  checks only signatures/time and is not request authentication. Require
  `verify_stored_session` against the current `sessions` row on every
  request: owner, expiry, counter and revocation must match. Refresh must
  conditionally update the matching row/counter and commit before sending
  the cookie; a superseded cookie then fails. Privilege changes require
  reauthentication, revoking the old row and issuing a new session id.
- **Hyperdrive query caching:** the `HYPERDRIVE` binding MUST point to a
  Hyperdrive config with query caching disabled (`--caching-disabled`).
  Session, credential, user, challenge and vault reads are plain
  parameterized `SELECT`s; Hyperdrive caches such reads by default
  (`max_age` 60s + 15s `stale_while_revalidate`, never invalidated by
  writes) and names sessions/authentication as the case needing a
  separate cache-disabled config. With caching on, `logout`/`revoke-all`
  (`UPDATE sessions SET revoked = TRUE`) is invisible for up to ~75s, a
  refreshed cookie fails against the cached `rotation_counter`, a second
  hardware-key login fails the conditional `sign_count` advance on stale
  data, and a repeated `register/start` can see a cached user miss. Writes
  (`INSERT/UPDATE/DELETE ... RETURNING`, including rate-limit slots and
  test-connection leases) are never cached. Deploy gate:
  `./check_hyperdrive_cache.sh --config-id <id>` asserts
  `caching.disabled == true` (see `wrangler.toml.example`).
- **Magic links:** 256-bit random tokens, SHA-256 hash at rest, 15-minute
  expiry. HTTP callers must use `consume_magic_link_atomically`, not the
  pure record-preparation helper. Magic links, passkey challenges and
  invites use `SingleUseStore`: one parameterized conditional UPDATE,
  requiring exactly one returned row and a committed transaction before
  any session/signup response. Zero rows, multiple rows and store errors
  fail closed. Never SELECT then unconditionally persist a used flag.
  All failures return one generic message (no user-enumeration oracle).
- **Abuse budgets:** passkey starts, magic-link issuance and test-connection
  attempts reserve fixed slot rows through an atomic `INSERT .. ON CONFLICT`
  before side effects; magic-link and registration inbox-proof sends share
  one per-email window. Unknown login emails use a keyed stand-in scope,
  expired challenges are pruned, and login options omit `allow_credentials`
  to avoid leaking variable-length credential IDs.
- **Passkeys:** RP ID equals the exact origin host (cross-subdomain RP
  suffixes are intentionally unsupported). Configuration must be a canonical
  serialized origin: https, or http with the host exactly `localhost`,
  `127.0.0.1` or `::1` for development, no userinfo/path/query/fragment.
  Five-minute challenges must be consumed through
  `consume_challenge_atomically`, and `webauthn.rs` signature verification
  (attestation `none`/packed-self, assertion ES256/EdDSA, RP-hash and
  exact origin/challenge pinning, monotonic `sign_count` clone
  detection) must finish before authentication. The handlers enforce
  both, in that order, on every ceremony.
- **SSRF guard:** provider hosts are hostile input. Shape check (never a
  URL), resolve ALL DNS records, reject loopback/link-local/private/
  unique-local/multicast/reserved (v4+v6); literal IPs checked directly.
  IPv6 is limited to native global unicast minus transition/special-use
  space: NAT64, mapped/translated IPv4, 6to4, Teredo, site-local, discard,
  ORCHID and documentation ranges are rejected conservatively.
  `validate` checks every DNS answer, rejects if any is forbidden, and
  returns at most four vetted addresses in a `ValidatedTarget`. Revalidate
  at every dial/fetch and pin the transport to that address set without
  another DNS lookup; retain the original hostname for TLS SNI/certificate
  checks and HTTP Host. Save-time validation is not a fetch authorization.
  Indexer fetches forbid redirects. Test-connection attempts atomically
  reserve shared-store rate slots, acquire one of two per-user expiring
  leases, dial at most four addresses with a 5-second per-address timeout,
  and stop the complete probe at 15 seconds.

## Test

```sh
cargo test --manifest-path workers/api/Cargo.toml
cargo clippy --manifest-path workers/api/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path workers/api/Cargo.toml -- --check
```

Fixtures use synthetic credentials only (`news.example.com`, `203.0.113.7`).
