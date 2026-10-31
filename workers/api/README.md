# nntune-api: Subsonic, accounts, auth, sessions, BYOP vault, library

Rust core + Worker HTTP entry for NNTune v2 accounts/auth (passkeys +
email magic link), signed-cookie sessions, the BYOP credential
vault, and the genre/playlist library, with a Subsonic-compatible
Worker entry on `/rest/*`. Compiles to native (tests)
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
  src/ledger.rs       # per-play stream ledger: validated row intents, monthly folds, SQL
  src/b64.rs          # strict base64 helpers
  tests/              # acceptance + security-regression tests, including ledger property tests
  wrangler.toml.example  # Worker config template (no secrets)
```

`migrations/0002_auth_vault.sql` plus `0003` (library), `0004` (challenge
purpose), `0005` (atomic rate-limit slots and test leases), `0006` (account
deletion), `0007` (registration-challenge user foreign key), `0008`
(database-enforced challenge purpose: CHECK plus an immutability trigger),
`0009`–`0012` (metered admission ledger, atomic claim lease, claim-epoch
fencing, queued replay window), `0013` (session-bound reauth purpose),
`0015_kek_rotation` (vault `rewrapped_at` marker + `kek_version`
index, see `docs/security/kek-rotation.md` §2), `0018`
(staging-only `qa_issued_codes` outbox for the test-code seam below), `0019`
(native REST credentials),
`0020_stream_play_ledger.sql` (per-play stream ledger: validated row intents,
monthly folds, replay-safe parameterized SQL; NOT the ADR daily `usage_ledger`),
and `0021`–`0025` (billing, library artifacts, Subsonic clients, session
refresh retry and claim clock) define the app-target Postgres schema (PlanetScale via Hyperdrive;
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
| `POST /v1/credentials` | mint a native REST credential (session-only, fresh-session step-up; secret rendered once in the 201 body, never stored or listed; rejects zero TTL and scope lists longer than the defined vocabulary with 422 `validation_failed`) |
| `GET /v1/credentials` | list the caller's native REST credentials (session-only, ordinary session; ids and metadata only, no secrets) |
| `DELETE /v1/credentials/{id}` | revoke one credential by id (session-only, fresh-session step-up; single-segment id, idempotent) |
| `POST /api/test-codes` (staging-only seam, no `OPTIONS` arm — see below) | returns the issued signup code for an `example.test` mailbox without minting; fail-closed 404/401 outside the staging posture |
| `POST /api/test-accounts` (staging-only seam, no `OPTIONS` arm — see below) | mints a fresh `example.test` account and returns `{"email", "session"}` (the session cookie value) in a 201; same gates and fail-closed 404/401 as `/api/test-codes` |

`OPTIONS` on any listed route (and the scope-map-served native paths, never
the staging-only `POST /api/test-codes` and `POST /api/test-accounts` seams) answers an empty 204. Credentialed CORS
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
stream pool. Staging deploy rides the `staging-api` lane in
`.github/workflows/deploy.yml` (generated per-env wrangler config from
public repo variables, worker-build plumbing, `--var BUILD_SHA` wiring);
Worker secrets and the staging variable values are Operator-wired, and the
lane skips until they are.

### Session refresh retries without a stale-cookie grace period

An empty body or `{}` on `POST /v1/session/refresh` keeps the strict one-shot contract:
commit the conditional counter advance, then send `Set-Cookie`. A stale
cookie is never accepted for ordinary authentication or unbound refresh.
Clients needing lost-response recovery must send the body-independent header
`Content-Type: application/vnd.nntune.session-refresh-bound+json` and JSON
`{"retry_key":"<unpadded base64url of 32 cryptographically random bytes>"}`.
The same exact `Origin` and signed session cookie are still required. The HTTP
transport rejects a retry key without this marker, and rejects marked requests
with missing, empty, truncated or malformed JSON before any session mutation.
Thus a lengthless upload abort hidden by the runtime as empty EOF cannot
silently downgrade a marked operation to an unbound empty-body refresh.

- Generate a **new 256-bit random secret before each logical refresh** and
  retain it privately for that operation. Never derive it from the cookie,
  user id, timestamps, or a public request id. Never log it, put it in URLs,
  or persist it to localStorage. The server checks encoding/length, not
  randomness: clients must use a CSPRNG (e.g. `crypto.getRandomValues`).
- On a lost response or retryable 5xx, retry with the **same** secret and
  predecessor cookie. The server atomically binds a digest of both to the
  rotation. The original cookie alone, or a different secret, cannot recover.
  Ordinary endpoints still reject the old cookie immediately.
- Recovery only reissues the immediate successor; it never rotates again,
  extends expiry, resets authentication age, or reopens a revoked session.
  The window is **strictly less than 120 seconds** from the first serialized
  PostgreSQL receipt claim (`refresh_claimed_at`) and is never renewed by retry.
  Signed-token issuance (`refresh_issued_at`) retains the validated Worker
  candidate's clock; it is not replaced by the database clock. This separates
  receipt age from token structure under cross-system clock skew. The adapter
  validates candidate and returned token structure before committing.
  PostgreSQL locks the row before sampling its fresh claim clock, then rechecks
  both token expiries and the receipt deadline in that transaction; time spent
  waiting for that lock counts against the existing recovery window. The predecessor must also still
  pass its signed-token lifetime checks. Another rotation, logout,
  revoke-all, reauthentication, or account deletion closes recovery.
- **Cross-tab single-flight is mandatory for cookie clients.** Serialize all
  session-cookie mutations across every tab/window sharing the cookie jar,
  including bound/unbound refresh, sign-in, logout, and reauthentication.
  Hold one same-origin Web Lock (or equivalent shared coordinator) for the
  entire logical operation, including delivery of response headers and all
  retries. No hedged or overlapping live requests: a delayed duplicate's
  `Set-Cookie` can otherwise overwrite a newer generation. This protocol
  does not make same-name browser cookies generation-safe.
- Retry only after the previous HTTP attempt has fully settled and cannot
  deliver further cookie-mutating headers. A timeout or `AbortController.abort()`
  alone is **not** proof of that boundary. If the transport cannot establish
  settlement, do not start another cookie mutation; retain the operation's
  recovery state and fail closed until it can. All contexts must use the
  coordinator; one tab's in-memory mutex is not sufficient.
- After success **and settlement of every outstanding attempt**, discard the
  secret and release the coordinator before starting the next logical
  operation. After the window or either token's expiry, require a new sign-in
  under that same coordinator; never accept a stale cookie. If the browser
  already installed the successor cookie before reporting a network failure,
  a settled retry can perform another ordinary authenticated rotation; it
  cannot recover a different session or bypass revocation.
- Success stays `200 {"ok":true}` with the secure HttpOnly cookie; secrets
  and digests never appear in response bodies or audit fields. All reads
  and writes use the current server-side row. Only the digest, original token
  issuance and original database claim timestamp are stored, in one bounded
  receipt slot per session.

Apply app migrations `0024_session_refresh_retry.sql` and
`0025_session_refresh_claim_clock.sql` **before** deploying this Worker.
Existing receipts without a claim clock cannot recover; no history is backfilled.
The staging deployment now fails closed unless its read-only schema gate verifies
all three nullable receipt columns and their exact types on the actual database
identified by the deployed Hyperdrive origin. It reuses the existing staging
migrator secret, checks private target identity, and never applies a migration.
Because the Worker resolves unqualified `sessions` through its runtime role,
the gate also requires that no other non-system `sessions` relation exists
(including indexes, partitioned indexes and composite relations), so any role
search path that resolves the name resolves the checked public table. Disposable
PostgreSQL CI executes this exact catalog query against positive, shadow and
system/temp-exception fixtures; inverted-exclusion and omitted-kind mutants
must fail verdict assertions, not just SQL-source checks.
The direct schema connection requires `sslmode=verify-full` (also the default
when omitted); `require` and weaker modes reject before connecting. The staging
job installs/checks the system CA bundle, passed explicitly as `PGSSLROOTCERT`,
so both the certificate chain and database hostname are verified. Existing
secret URLs with `sslmode=require` need an authorized configuration update;
this code does not read or rewrite the deployed secret.
Missing configuration, target mismatch, query failure or absent schema prevents
both deployment and a positive deployment marker. Applying staging migrations
remains the authorized database executor's action, not an implicit CI operation.
Unsupported/unmigrated runtime storage fails closed with 5xx, never an
unbound recovery. No current web caller automatically opts into this
protocol; adoption requires both the per-operation secret and the shared
single-flight coordinator above. Do not opt in while older tabs or clients
can still write this cookie without participating in that coordinator.
The security decision is deliberate: no blanket previous-token allowance,
no weakened replay tests, and no recovery from possession of a stale
cookie alone. Stealing both the predecessor cookie and its separately
retained retry secret allows recovery inside the bounded window; protect
both as credentials.

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

### Inline magic-link delivery deadline

After admission and token storage, registration/magic-start wait at most five
seconds for one mail send. Expiry cancels the send future; the Worker mailer
attaches an AbortSignal and aborts its fetch when that future is dropped
(including cancellation of the enclosing request). Fast success and failure
still return immediately. Delivery errors and timeouts remain ignored, with
the same generic public envelope and no retry, replacement token, or refund
of the per-IP/global/per-address admission slots. A timeout does not revoke
the issued token: abort cannot retract a request already accepted by the
provider. Synthetic staging-code capture still happens before delivery.

This bounds only the admitted send, not preceding storage operations, and
is not a delivery guarantee. Native pending-mail tests prove cancellation of
the Rust future; wasm compilation alone does not prove runtime fetch abort.

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
- **Mint seam:** `POST /api/test-accounts` (`QA_TEST_ACCOUNTS_PATH`,
  served by `qa_test_accounts` in `src/handlers.rs`; read by
  `tryMintTestAccount` in `e2e/helpers/accounts.ts`) runs the same flag,
  bearer and `example.test` gates in the same order, then creates a new
  user plus a stored session row and returns `{"email", "session"}` with
  201. `session` is the `__Host-nntune-session` cookie value; the API
  authenticates sessions by cookie only. No passkey, vault or magic row is
  created. Only the default plan is minted (`plan` absent or `"free"`);
  any other plan, and an address that already has an account, is a fixed
  422 `validation_failed` (no route enforces plan caps yet, so a capped
  account is never faked). Every mint is audited as `qa.test_account_mint`.

The Subsonic-compatible `/rest/*` surface (wasm32 only) is routed in
`src/lib.rs` and delegates to `crates/nntune-subsonic`. Its
`MemStore::empty()` contains no credentials and refuses all
authentication; PlanetScale `Library` and stream Worker wiring are
still pending. This is not a deployed, usable music API yet. See
[`docs/subsonic-client-smoke.md`](../../docs/subsonic-client-smoke.md)
for the client verification procedure.

This crate remains standalone (excluded from the root workspace), so use
`--manifest-path workers/api/Cargo.toml` for its checks.

## Security contract (threat-model §3–§4)

- **Envelope encryption:** per-user random 256-bit DEK, wrapped by the KEK
  (Cloudflare Worker secret, versioned). AES-256-GCM, random 96-bit nonces,
  AAD binds `user_id` (+ KEK version for the wrap). Row-swap between users
  fails to decrypt. Rotation via `rewrap_dek` without touching ciphertext.
  (Sources: `workers/api/src/vault.rs` — `encrypt_vault`, `decrypt_vault`,
  `dek_aad`/`payload_aad`, `rewrap_dek`; KEK from `VAULT_KEK_BASE64` +
  `VAULT_KEK_VERSION` (legacy) or `VAULT_KEK_V<N>` + `VAULT_KEK_CURRENT`
  (versioned) in `workers/api/src/worker_entry.rs`; marker/index in
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
  stop with a fixed email-independent `429` carrying the standard
  `rate_limited` error envelope
  (`{"code":"rate_limited","message":"too many requests","retryable":true}`,
  plus `Retry-After`) on `register/start` (never an email-derived
  stand-in, which would oracle taken vs fresh across the denial boundary)
  and with unchanged response shapes on login/magic starts, while
  victim-list email-bombing stops with no further mail. Exhausted budgets fast-fail with one `SELECT count(*)`
  before the slot loop, so denial costs O(1) statements, not one upsert per
  slot. `register_start` also reaps never-completed signups older than
  24 h only when they have no credential, vault, session, unexpired magic
  link, or unexpired registration challenge. Enrollment also holds a parent
  user lock before claiming proofs; the reaper skips locked parents even
  after those proofs expire and rechecks eligibility on a fresh snapshot
  before deletion. Later cleanup retries skipped users and expired
  challenges. Issuing a magic link
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
  `throttled` (429 `rate_limited` envelope), `RATE_LIMIT_EXHAUSTED_SQL`,
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
- **Per-play stream ledger (`stream_play_ledger`, not the ADR daily
  `usage_ledger`):** one row per billed play window. Stream plays derive a
  deterministic `play:{user}:{track}:{play}` key (same derivation as the
  stream worker; 128/512/512-byte field limits). Both derivation helpers
  reject empty fields or anything outside ASCII `[A-Za-z0-9_-]`, without
  rewriting identities; colons are structural delimiters only. Accepted keys
  therefore match the persisted ledger alphabet. Non-play windows use a fresh
  128-bit key minted once and reused across retries. The
  insert is fenced `ON CONFLICT DO NOTHING` through the play owner's
  `users` row (`FOR SHARE`, ADR 0002 rule 3), so a play finalized behind
  the account-deletion marker inserts nothing — then the Worker selects
  the stored row, probes the fence, and classifies exact redelivery
  (`Duplicate`) vs changed payload (`Conflict`, flagged never converged)
  vs fenced refusal (`AccountDeleting`, typed 409 `account_deleting`).
  A stored row decides without the probe, so healthy duplicates never read
  as deleting. Stream seconds must
  equal `ceil(bytes / 48_000)` — the stream worker's nominal rate — or the
  row is rejected; `period_month` derives from `recorded_at`, and monthly
  usage is `SUM` over (`user_id`, `period_month`). All SQL is parameterized;
  errors stay generic and never echo handles.
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
  save/test time only (`IndexerConfig::validate`). Saved indexer URLs are
  refused with `VaultError::Invalid` when the shape is wrong: a non-`https`
  scheme (`vault.rs:363-365`), parsed userinfo (`vault.rs:373-375`), raw
  whitespace or backslash (`vault.rs:384-386`), a non-literal `https://`
  scheme prefix (`vault.rs:387-400`), an empty authority or a raw `@` in
  it — including empty `@host`/`:@host` and obfuscated shapes
  (`https:/@host`, tab/newline splits, backslashes, extra slashes) that
  the parser would strip or normalize (`vault.rs:410-415`), an explicit
  non-443 port (`vault.rs:420-424`), or any fragment (`vault.rs:427-429`).
  Indexer fetches must
  forbid redirects (`INDEXER_ALLOW_REDIRECTS = false`, threat-model
  F-INGEST-01). No live indexer fetch transport exists yet;
  `workers/ingest/src/indexer_fetch.rs` is a test-only policy model whose
  3-hop redirect following is an unresolved discrepancy (see
  `workers/ingest/README.md`). Test-connection attempts atomically
  reserve shared-store rate slots, acquire one of two per-user expiring
  leases. Each handler-time DNS resolution has one 5-second lifetime covering
  both A and AAAA response headers and JSON bodies, not 5 seconds per family
  or per phase. Expiry drops pending resolver work, aborts the Worker fetch/body
  with its AbortSignal, discards partial answers, and fails closed before any
  socket dial. Caller cancellation also aborts pending DoH work. Fast literal
  and dual-family results retain the same all-address validation. This DNS
  budget applies to save/test validation (provider and indexer), separately
  per host; it is not an overall HTTP-request deadline. After successful DNS
  validation, the unchanged socket budgets dial at most four addresses with a
  5-second per-address timeout and stop the socket attempts at 15 seconds.
  (Sources:
  `workers/api/src/vault.rs` — `host_shape_ok`, `ip_is_forbidden` (from
  `crates/nntune-ssrf-guard`),
  `resolve_public_target`, `ProviderConfig::validate`,
  `authorize_pool_dial`, `ALLOWED_NNTP_PORTS`, `MAX_DIAL_ADDRESSES`,
  `RESOLUTION_TIMEOUT_MS`, `DIAL_TIMEOUT_MS`/`TOTAL_TIMEOUT_MS`, `MAX_TEST_CONCURRENCY`,
  `INDEXER_ALLOW_REDIRECTS`; pinned dial in
  `workers/api/src/worker_entry.rs` (`SocketDialer::probe`, `probe_one`);
  slots/leases in `workers/api/src/handlers.rs` (`vault_test_connection`)
  and `workers/api/src/worker_entry.rs`
  (`reserve_rate_limit_pair`, `acquire_test_connection_lease`); tables in
  `migrations/0005_atomic_rate_limits_and_leases.sql`.)

## Test

Run these commands from the repository root:

```sh
cargo test --locked --manifest-path workers/api/Cargo.toml --all-targets
cargo clippy --locked --manifest-path workers/api/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path workers/api/Cargo.toml -- --check
cargo check --locked --manifest-path workers/api/Cargo.toml --target wasm32-unknown-unknown
bash workers/api/check_hyperdrive_cache.sh --self-test
cargo test -p nntune-subsonic --locked
python3 scripts/check-subsonic-fixtures.py
```

(Mirrors CI jobs `api` (`.github/workflows/ci.yml:384-409`) and
`fixture-check` (`.github/workflows/ci.yml:65-74`); `fmt` takes no
`--locked` (it resolves no dependencies), so the asymmetry is
intentional. `cargo test -p nntune-subsonic` also runs under
`rust-test --workspace`.)

Fixtures use synthetic credentials only (`news.example.com`, `203.0.113.7`).
