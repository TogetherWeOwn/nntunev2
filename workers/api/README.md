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
purpose), `0005` (atomic rate-limit slots and test leases), `0006` (account
deletion), `0007` (registration-challenge user foreign key), `0008`
(database-enforced challenge purpose: CHECK plus an immutability trigger),
`0009`–`0012` (metered admission ledger, atomic claim lease, claim-epoch
fencing, queued replay window), `0013` (session-bound reauth purpose),
`0015_kek_rotation` (vault `rewrapped_at` marker + `kek_version`
index, see `docs/security/kek-rotation.md` §2) and `0018`
(staging-only `qa_issued_codes` outbox for the test-code seam below)
define the app-target Postgres schema (PlanetScale via Hyperdrive;
targets per `migrations/manifest.toml`). The mirror-target `0001`
(MusicBrainz mirror base), `0014` (import resume cursors + dead-letter
queue), `0016` (release-to-track membership) and `0017` (replication
packet ledger) serve the mb mirror, not this worker (see
`workers/mb/README.md`). CI applies every migration to a throwaway
Postgres and runs `tests/sql/*.sql` against it
(`tests/run_migration_tests.sh`, `api-migrations` job in
`.github/workflows/ci.yml`). All queries are parameterized;
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
| `POST /v1/session/reauthenticate/start` / `finish` | current-session-bound passkey proof and atomic single-session replacement |
| `GET` / `PUT` / `DELETE /v1/vault` | booleans-only summary / encrypted upsert / delete |
| `POST /v1/vault/test` (`/v1/vault/test-connection` compatibility alias) | `{"ok": bool}` only; atomically rate-limited, per-user concurrency lease, bounded pinned-address dial; empty body tests the stored vault, a `{provider, indexer}` envelope tests a candidate without persisting it |
| `POST /api/test-codes` (staging-only seam, no `OPTIONS` arm — see below) | returns the issued signup code for an `example.test` mailbox without minting; fail-closed 404/401 outside the staging posture |

`OPTIONS` on any listed route except the staging-only `POST /api/test-codes`
seam answers an empty 204. Credentialed CORS
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

### Test-connection routing boundary

- The current path is a direct dial from the api Worker:
  `vault_test_connection` in `workers/api/src/handlers.rs` resolves all
  records, validates the provider config, then calls the injected
  `Dialer::probe` on the vetted target (`workers/api/src/handlers.rs`),
  and the Worker entry wires that dialer to `SocketDialer`
  (`workers/api/src/worker_entry.rs`).
- The dial targets only the vetted IP literals (at most
  `MAX_DIAL_ADDRESSES` in `workers/api/src/vault.rs`), never a second
  DNS lookup (`workers/api/src/handlers.rs`,
  `workers/api/src/worker_entry.rs`); the per-address and total
  deadlines come from `DIAL_TIMEOUT_MS` / `TOTAL_TIMEOUT_MS` in
  `workers/api/src/vault.rs`, with per-user concurrency capped by
  `MAX_TEST_CONCURRENCY` in `workers/api/src/vault.rs`.
- TLS fails closed on hostname-only certificates: `probe_one` in
  `workers/api/src/worker_entry.rs` dials the IP literal with the
  runtime socket, so the runtime validates the certificate against the
  dialed address and a hostname-only provider certificate returns
  `ok:false` instead of bypassing the pin
  (`workers/api/src/worker_entry.rs`).
- Stream-pool routing is not yet in place: the per-user `NntpPool`
  Durable Object and its probe/body lanes live in `workers/stream`
  (`workers/stream/src/lib.rs`, `workers/stream/src/runtime.rs`,
  `workers/stream/wrangler.toml`), and test-connection does not call
  the pool binding (`workers/ingest/src/pool_probe.rs`); pool
  integration is future work, not started here.
- Boundary ownership: scoped-credential behavior is owned by its own
  lane and port-allowlist hardening by the SSRF-guard lane
  (`ALLOWED_NNTP_PORTS` in `workers/api/src/vault.rs`, threat-model §3
  in `docs/security/threat-model.md`); this note describes the routing
  boundary only and changes no vault, dial, TLS, pool, or credential
  behavior.

### Staging-only test-code seam (not part of the production surface)

Shipped by the staging-seam build slice — this note records guardrails
only, not implementation work. The normative security note is
`docs/security/threat-model.md` §14 (see also row 1 of
`docs/m1-exit-traceability.md`). No route, auth, migration, or credential
behavior changes here.

- **Seam path:** `POST /api/test-codes` (`QA_TEST_CODES_PATH`, served by
  `qa_test_codes` in `src/handlers.rs`). The E2E pins the same path as
  `testCodes` in `e2e/helpers/contract.ts` — a rename must change both
  sides together — and reads it via `retrieveStagedSignupCode` in
  `e2e/helpers/accounts.ts`. The path is deliberately absent from
  `is_known_path` (`src/handlers.rs`), so unlike every row above it has
  no `OPTIONS` 204 arm and is not enumerable from production.
- **`example.test`-only scope:** only a well-formed synthetic mailbox
  passes `is_example_test_mailbox` (`src/handlers.rs`: exactly one `@`,
  reserved `example.test` domain matched case-insensitively; subdomains
  and suffix lookalikes do not match). The E2E mints such mailboxes with
  `uniqueTestEmail` (`e2e/helpers/contract.ts`, used from
  `e2e/helpers/accounts.ts`). Any other address answers the
  indistinguishable unknown-route 404.
- **Never mints:** `qa_test_codes` (`src/handlers.rs`) is read-only — it
  creates no users, challenges, or magic rows — and returns the captured
  issuance as `{"magic_token": ...}` only while unexpired, else 404. The
  E2E side (`retrieveStagedSignupCode` in `e2e/helpers/accounts.ts`)
  requires a nonblank `magic_token` and never fabricates a code.
- **Capture-side gate:** magic issuance stores the raw token in the
  migration 0018 outbox (`qa_issued_codes`) only when `QA_CODE_SEAM` is
  armed and the recipient is an `example.test` mailbox
  (`src/handlers.rs`) — the deliberate narrow exception to the §4
  "plaintext token never stored" invariant; see
  `docs/security/threat-model.md` §14.
- **Gating:** armed ONLY by the explicit staging flag
  (`Config::qa_code_seam` via `with_qa_code_seam` in `src/handlers.rs`,
  wired from `QA_CODE_SEAM` in `src/worker_entry.rs`) plus the staged
  bearer (`Config::qa_auth_token` via `with_qa_auth_token`, compared
  constant-time by `qa_bearer_ok` in `src/handlers.rs` against the
  `Authorization: Bearer <QA_AUTH_TOKEN>` header). Flag off or bearer
  unstaged serves the seam path as the plain unknown-route 404; a wrong
  bearer is a names-only 401. Production never sets the flag or stages
  the bearer, proven by the `qa_seam_absent_without_the_staging_flag`
  gate test (`src/handlers.rs`). The E2E returns null without
  `QA_AUTH_TOKEN` and treats the seam as required infrastructure once it
  is set (`e2e/helpers/accounts.ts`).

## Security contract (threat-model §3–§4)

- **Envelope encryption:** per-user random 256-bit DEK, wrapped by the KEK
  (Cloudflare Worker secret, versioned). AES-256-GCM, random 96-bit nonces,
  AAD binds `user_id` (+ KEK version for the wrap). Row-swap between users
  fails to decrypt. Rotation via `rewrap_dek` without touching ciphertext.
  (Sources: `workers/api/src/vault.rs` — `encrypt_vault`, `decrypt_vault`,
  `dek_aad`/`payload_aad`, `rewrap_dek`; KEK from `VAULT_KEK_BASE64` +
  `VAULT_KEK_VERSION` in `workers/api/src/worker_entry.rs`; marker/index in
  `migrations/0015_kek_rotation.sql`, design `docs/security/kek-rotation.md` §1–§2.)
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
  the cookie; a superseded cookie then fails. Vault credential writes
  (`PUT /v1/vault`) and deletes (`DELETE /v1/vault`) require a freshness
  window of 600 seconds (10 minutes) on the immutable authentication time
  (`created_at`, never `issued_at`, so rotation cannot restore freshness):
  sessions authenticated outside the window are rejected with the documented
  `reauth_required` error code (401). Vault summary reads and boolean
  connection tests retain ordinary session authentication.
  `POST /v1/session/reauthenticate/start` authenticates the existing cookie
  with exact-Origin CSRF protection and stores a `reauth` passkey challenge
  bound to that session's user. `/finish` requires the same active session,
  that same-user challenge, and a verified assertion from a credential owned
  by that user. It consumes the challenge and advances the credential counter,
  then uses `reauthenticate_session` with the verified credential user id.
  The store conditionally revokes exactly the old row and inserts the new row
  in one transaction; zero-row claims, conflicts, insert errors, and commit
  errors return without a replacement cookie. The cookie is emitted only
  after commit. `login_finish` and `magic_finish` remain ordinary sign-in and
  issue independent sessions without revoking existing rows.
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
  per-IP slots — the 30/hour budget plus the configured short-window burst
  as one all-or-none pair (`auth.start_ip` + `auth.start_ip_burst`, defaults
  30/hour + 10/60s), keyed on the client /64 for IPv6 and the full
  address for IPv4 — before any user/challenge row or mail (no
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
  signup until expiry; expired challenges are pruned. Issuing a magic link
   prunes that user's expired magic rows, so live accounts stay bounded. Passkey finishes
  reserve 100 lookups per client prefix
  BEFORE any DB read plus 10 attempts per challenge before verification
  (and magic finishes 10 per client prefix, after the exact-Origin pin) per
  5-minute window, so rotating unknown challenge ids cannot fund unbounded
  Pg reads and forged assertions cannot burn unlimited CPU on one
  challenge; legitimate single attempts and typo retries are unaffected.
  Unknown login emails use a keyed stand-in scope, expired challenges are
  pruned, and login options omit `allow_credentials` to avoid leaking
  variable-length credential IDs. (Sources: `workers/api/src/handlers.rs` —
  `admit_auth_start`, `send_magic_link_if_allowed`, `rate_limit_ip_key`,
  `throttled` (429 `{"error":"rate_limited"}`), `RATE_LIMIT_EXHAUSTED_SQL`,
  finish-budget consts; `workers/api/src/rate_limit.rs` defaults +
  `RateLimitConfig::from_vars`; atomic claim/pair in
  `workers/api/src/worker_entry.rs` (`claim_rate_limit_slot`,
  `reserve_rate_limit_pair`); tables in
  `migrations/0005_atomic_rate_limits_and_leases.sql`.)
- **Passkeys:** RP ID equals the exact origin host (cross-subdomain RP
  suffixes are intentionally unsupported). Configuration must be a canonical
  serialized origin: https, or http with the host exactly `localhost`,
  `127.0.0.1` or `::1` for development, no userinfo/path/query/fragment.
  The handlers first verify WebAuthn signatures (attestation
  `none`/packed-self, assertion ES256/EdDSA, RP-hash and exact
  origin/challenge pinning, monotonic `sign_count` clone detection), then
  atomically consume the five-minute challenge through
  `consume_challenge_atomically`, and only then authenticate/create the
  session. A failed verification never consumes a challenge. RP policy:
  **user verification
  is required** (authenticatorData UV flag, plus `userVerification:
  "required"` requested by the web client) because the passkey is the only
  primary factor and login is usernameless; a presence-only tap never
  authenticates, and authenticators without PIN/biometric are refused.
  The enrollment `sign_count` in a (unauthenticated) `none` attestation is
  never stored: new credentials start at 0 and the first signed assertion
  sets the baseline. Backup-eligibility (BE/BS) bits are not recorded:
  nothing branches on synced-vs-device-bound credentials yet.
- **Audit `source_ip`:** taken from `cf-connecting-ip` only if it parses as
  one IP literal (stored in canonical form); anything else, or a missing
  header, becomes `unknown` (`vault::sanitize_source_ip`, applied at
  capture and again in `audit_event`), so audit rows and rate-limit keys
  never carry control characters or markup.
- **Dependencies:** `Cargo.lock` is committed. CI builds and tests with
  `--locked`, and the Wrangler build command passes `--locked` through
  `worker-build`, so deploys resolve the audited dependency set; crypto
  crates stay caret ranges in `Cargo.toml` and move only via a reviewed
  lockfile change (Dependabot).
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
  This worker performs no indexer fetches; indexer URLs are validated at
  save/test time only (`IndexerConfig::validate`). Indexer fetches must
  forbid redirects (`INDEXER_ALLOW_REDIRECTS = false`, threat-model
  F-INGEST-01). No live indexer fetch transport exists yet;
  `workers/ingest/src/indexer_fetch.rs` is a test-only policy model whose
  3-hop redirect following is an unresolved discrepancy (see
  `workers/ingest/README.md`). Test-connection attempts atomically
  reserve shared-store rate slots, acquire one of two per-user expiring
  leases, dial at most four addresses with a 5-second per-address timeout,
  and stop the complete probe at 15 seconds. (Sources:
  `workers/api/src/vault.rs` — `host_shape_ok`, `ip_is_forbidden`,
  `resolve_public_target`, `ProviderConfig::validate`,
  `authorize_pool_dial`, `ALLOWED_NNTP_PORTS`, `MAX_DIAL_ADDRESSES`,
  `DIAL_TIMEOUT_MS`/`TOTAL_TIMEOUT_MS`, `MAX_TEST_CONCURRENCY`,
  `INDEXER_ALLOW_REDIRECTS`; pinned dial in
  `workers/api/src/worker_entry.rs` (`SocketDialer::probe`, `probe_one`);
  slots/leases in `workers/api/src/handlers.rs` (`vault_test_connection`)
  and `workers/api/src/worker_entry.rs`
  (`reserve_rate_limit_pair`, `acquire_test_connection_lease`); tables in
  `migrations/0005_atomic_rate_limits_and_leases.sql`.)

## Test

```sh
cargo test --manifest-path workers/api/Cargo.toml
cargo clippy --manifest-path workers/api/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path workers/api/Cargo.toml -- --check
```

Fixtures use synthetic credentials only (`news.example.com`, `203.0.113.7`).
