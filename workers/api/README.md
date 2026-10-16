# nntune-api: accounts, auth, sessions, BYOP vault, library

Rust core + Worker HTTP entry for NNTune v2 accounts/auth (passkeys +
email magic link), signed-cookie sessions, the BYOP credential
vault, and the genre/playlist library. Compiles to native (tests)
and `wasm32` (Cloudflare Worker).

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
  src/library.rs      # genres/playlists: opaque ids, owner-scoped authz
  src/b64.rs          # strict base64 helpers
  tests/              # acceptance + security-regression tests
  wrangler.toml.example  # Worker config template (no secrets)
```

`migrations/0002_auth_vault.sql` plus `0003` (library), `0004` (challenge
purpose), `0005` (atomic rate-limit slots and test leases), and `0006`
(registration-challenge user foreign key) define the Postgres schema
(PlanetScale via Hyperdrive). All queries are parameterized;
single-use claims execute the conditional `UPDATE ... RETURNING` prepared by
the core and require exactly one row before any response.

## Routes (all JSON, `Cache-Control: no-store`)

| Route | Purpose |
|---|---|
| `GET /healthz` | liveness only (no config detail) |
| `POST /v1/auth/passkey/register/start` / `finish` | passkey signup (purpose-bound challenge + emailed-token inbox proof; attestation `none` or packed self-attestation) |
| `POST /v1/auth/passkey/login/start` / `finish` | usernameless passkey login (empty `allow_credentials`; assertion + sign-count clone detection) |
| `POST /v1/auth/magic/start` / `finish` | email magic link (atomic per-email issuance reservation shared with register inbox proof; finish never replaces an existing session for a different account and echoes the signed-in account's own email for display) |
| `POST /v1/session/refresh` / `logout` / `revoke-all` | stored-session rotation/revocation |
| `GET` / `PUT` / `DELETE /v1/vault` | booleans-only summary / encrypted upsert / delete |
| `POST /v1/vault/test` (`/v1/vault/test-connection` compatibility alias) | `{"ok": bool}` only; atomically rate-limited, per-user concurrency lease, bounded pinned-address dial; empty body tests the stored vault, a `{provider, indexer}` envelope tests a candidate without persisting it |

`OPTIONS` on any listed route answers an empty 204. Credentialed CORS
headers (`Access-Control-Allow-Origin`, never `*`) are emitted only when
the request `Origin` exactly equals the configured origin; any other
origin gets none, so cross-origin JSON posts die at preflight. The web
client speaks exactly these routes and bodies: every body-bearing route
is pinned by `workers/api/tests/fixtures/api-web-contract.json` from
both sides — the vitest client suite asserts the exact wire body each
client builder sends, and the Rust handlers suite asserts each route is
served and each fixture wire body deserializes into the handler's
`deny_unknown_fields` body struct. The emailed magic link lands on the
web app's `/auth/magic` page; the webhook payload carries both the link
and the raw token (printed as the code the signup step asks for). That
page never auto-redeems (login CSRF): it scrubs the token from the URL
and waits for an explicit confirm, `magic/finish` refuses to swap an
existing session for a different account without burning the token,
and the provider wizard shows "Signed in as …" before any credential
is typed. A swap-guarded failure always offers sign-out
(`POST /v1/session/logout`), after which the unspent link redeems; it
names the still-signed-in account only when this browser tab knows it
(the display-only sessionStorage label), otherwise it says to sign out
first when signed in elsewhere. A failed sign-out keeps the button and
shows "Could not sign out. Try again." instead of success. Typo-class register failures (wrong emailed code, wrong
invite code) are pre-validated before either single-use claim, so a
corrected retry against the same challenge still enrolls.

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
- **Sessions:** `__Host-nntune-session` (`__Host-` prefix: `Path=/` via the
  pinned `COOKIE_PATH` constant, `Secure`, no `Domain=` by construction);
  `HttpOnly; SameSite=Lax`; `Max-Age` clamped to `MAX_SESSION_SECS`, and the
  immutable `created_at` caps the lifetime at one day even across refreshes.
  Logout is POST-only: `logout_session_atomically` (`/v1/session/logout`)
  revokes the one verified row, and `logout_everywhere_atomically`
  (`/v1/session/revoke-all`) revokes every row for the verified token's user
  — both authenticate via `verify_stored_session` first, then require the
  committed conditional `SessionRevokeStore` UPDATE (owner-bound,
  transition-counted; zero rows fail closed with `Invalid`, store failures
  surface `Storage` with the verified user so the handler returns a
  retryable 5xx without clearing the cookie). The pure
  `revoke_session` / `revoke_all_sessions` helpers only prepare snapshots;
  they revoke nothing server-side. Secret rotation runs a
  bounded dual-accept window — see `docs/security/session-secret-rotation.md`
  (`SessionKeys::rotation` verifies through `verify_stored_session` and
  re-signs with the primary; verify-only previous secret,
  `created_at`-bounded window, single-secret steady state; keys under 32
  bytes fail closed). `verify_session`
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
  `caching.disabled == true` (see `wrangler.toml.example`). The gate
  fails closed without Cloudflare credentials (exit 3); `--allow-skip`
  is only for secret-less CI and MUST NOT be used in real deploys.
  Runtime attestation: the Worker refuses to serve traffic (generic 500,
  same as any misconfiguration) unless `HYPERDRIVE_CACHING_DISABLED=1`
  is set for that env — set it only after the gate passes for that
  Hyperdrive config.
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
  one per-email window. Every auth start (register, login, magic) reserves
  one per-IP slot (30/hour, keyed on the client /64 for IPv6 and the full
  address for IPv4) before any user/challenge row or mail (no
  global slot at start, so exhaustion cannot block challenge issuance),
  and every mail send reserves a per-IP (30/hour) plus global (100/5 min)
  issuance slot in front of the per-email window — distinct-email loops
  stop with a fixed email-independent `429 {"error":"rate_limited"}` on
  `register/start` (never an email-derived stand-in, which would oracle
  taken vs fresh across the denial boundary) and with unchanged response
  shapes on login/magic starts, while victim-list email-bombing stops with
  no further mail. Exhausted budgets fast-fail with one `SELECT count(*)`
  before the slot loop, so denial costs O(1) statements, not one upsert per
  slot. `register_start` also reaps never-completed signups older than
  24 h only when they have no credential, vault, session, unexpired magic
  link, or unexpired registration challenge. In-flight registration
  challenges (including consumed challenges awaiting finish) protect the
  signup until expiry; expired challenges are pruned. Passkey finishes
  reserve 100 lookups per client prefix
  BEFORE any DB read plus 10 attempts per challenge before verification
  (and magic finishes 10 per client prefix, after the exact-Origin pin) per
  5-minute window, so rotating unknown challenge ids cannot fund unbounded
  Pg reads and forged assertions cannot burn unlimited CPU on one
  challenge; legitimate single attempts and typo retries are unaffected.
  Unknown login emails use a keyed stand-in scope, expired challenges are
  pruned, and login options omit `allow_credentials` to avoid leaking
  variable-length credential IDs.
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
- **Library (genres/playlists):** missing caller is 401 before any
  existence check; callers see own + public rows only, invisible ids are
  404 (no oracle), visible-but-foreign mutations are 403. Responses carry
  only the opaque `public_id` as `id` — internal ids and `owner_user_id`
  never serialize. All SQL is parameterized with the owner bound in WHERE;
  SELECT lists exclude the owner column (mutation lookups return only an
  `is_owner` boolean). Lists are keyset pages (`ORDER BY public_id COLLATE
  "C" DESC LIMIT $2` with an opaque cursor over the public id, LIMIT clamped
  to `MAX_PAGE_SIZE`, first page with a NULL cursor). Order is by the opaque
  id, not newest-first: a new row sorts by its id and may land on any page.
  The cursor is compared directly with the ordering key, so a deleted or
  newly private boundary row cannot truncate later pages; an above-max cursor
  replays from the top in both the in-memory pager and the SQL (harmless —
  every row is still visibility-filtered). Playlist lists
  omit `items`, which single reads return. UPDATE/DELETE affecting 0 rows
  maps to 404 — the owner predicate already makes cross-owner writes
  inexpressible.
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
