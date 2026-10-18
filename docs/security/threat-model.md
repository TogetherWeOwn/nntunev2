# NNTune v2 Threat Model (STRIDE)

Owner: CISO · Status: living document (finding IDs map to Paperclip child cards; card identifiers are never pasted into this public repo) —
update it when the vault, session, grant, pool, or ingest-fetch designs change, and re-request
CISO review on the changing PR.

## 1. Scope and assumptions

**In scope:** BYOP credential vault, accounts/auth (passkeys + email magic link),
sessions, signed stream grants, per-user NNTP pool isolation, public-repo hygiene.

**Decided stack (not re-litigated here):** Rust `workers-rs` → `wasm32` for ALL
backend Workers incl. Durable Objects; `crates/nntune-core` (native + wasm32);
SvelteKit `adapter-cloudflare` frontend; PlanetScale Postgres via Hyperdrive for
ALL relational data (no D1); R2 caches; Queues/Workflows; KV; Analytics Engine;
Containers for batch only.

**Assumptions (if one breaks, this model is stale):**

1. The repo is **public**: no secrets, no `TOG-*` ids, no internal hosts, no agent
   names in any public text (commit, PR, issue, comment, doc).
2. Secrets live only in GitHub Actions secrets, Cloudflare (Worker secrets /
   Hyperdrive), or a private operator secrets store. Never in code, logs, or error bodies.
3. NNTune never bundles or resells Usenet access (BYOP). Provider credentials are
   user-supplied and user-owned; we are a custodian, not an owner.
4. Transport is TLS-everywhere; Cloudflare edge + Postgres require TLS.
5. The KEK (key-encrypting key) lives as a Cloudflare Worker secret, never in the
   repo, never logged, rotatable (see F-VAULT-02).

## 2. Assets and trust boundaries

| # | Asset | Where it lives | Worth to attacker |
|---|---|---|---|
| A1 | User Usenet provider credentials (host/port/TLS/user/pass) | Vault row in Postgres (envelope-encrypted) | Direct abuse of paid provider accounts; credential-stuffing corpus |
| A2 | Newznab indexer URL + API key | Same vault row | Paid indexer quota theft |
| A3 | Session tokens / signed cookies | Client cookie + server session row | Account takeover, vault decryption oracle |
| A4 | WebAuthn credentials | Authenticator + stored public key | Account takeover if registration is spoofable |
| A5 | Magic-link tokens | Email in transit + single-use row | Account takeover if predictable/leaked |
| A6 | Signed stream grants | URL/token presented to stream Worker | Free/anonymous content fetch; confused-deputy pool use |
| A7 | Audio/artwork cache (R2) | R2 bucket | Copyright-adjacent exposure; cache-poisoning across users |
| A8 | KEK / DEKs | Worker secret (KEK); wrapped DEK per user row | Total vault compromise |

Trust boundaries: browser ↔ edge; edge ↔ Worker; Worker ↔ Hyperdrive/Postgres;
Worker ↔ Durable Object (pool); Worker ↔ R2/KV/Queues; CI (public logs) ↔
everything — **CI logs are attacker-readable: they must never contain secrets.**

## 3. BYOP credential vault (STRIDE)

Design under review: envelope encryption — per-user DEK, KEK from Worker secret;
credentials never logged, never returned after save; `test connection` via the
stream pool (accounts/auth/vault implementation card).

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | Attacker calls vault read/write as another user | Auth needed on EVERY vault endpoint; authenticated user must own the row (IDOR check first, cf. v1 avatar-oracle lesson: authorization precedes existence-oracle — non-owner gets the same response for "no vault" as "not yours") |
| Tampering | Ciphertext swapped between rows; KEK confusion across envs | AEAD (AES-256-GCM) with per-row nonce + associated data binding the domain separator and `user_id` (plus the KEK version on the DEK wrap; exact tuples in `kek-rotation.md` §1); KEK version column; staging and prod KEKs never shared |
| Repudiation | "I never stored that provider" / silent credential overwrite | Audit log: vault create/update/delete + test-connection attempts (who/when/source IP/result — NEVER the secret); no log line may contain credential material |
| Information disclosure | SQL dump, log leak, error echo, backup exposure | Envelope encryption at rest; parameterized queries only; generic errors ("vault unavailable"); `test connection` returns boolean only, never provider banners; backups inherit encryption (ciphertext only) |
| Denial of service | Vault row hammering; KEK-fetch storms | Rate-limit vault writes + test-connection per user (shared store, not in-memory — serverless); test-connection has dial + total timeouts and per-user concurrency cap |
| Elevation | SSRF at `test connection` AND at every pool dial: user-supplied host/port aimed at internal targets | **SSRF guard (F-VAULT-01, dial-time invariant):** the check runs on EVERY provider connect, not only at save/test time — a hostname's DNS can change after it is saved. Allowlist scheme/host shape and NNTP ports (119/563 plus documented alternates only); resolve ALL DNS records and reject loopback/link-local/private/unique-local (v4+v6); forbid redirects; dial timeout; pool Worker carries no private-network or service bindings; treat provider host as hostile input. **Pinning is required (one position — ADR 0008, §6, `authorize_pool_dial`):** the transport dials the vetted addresses only, never a second DNS lookup, retaining the hostname for TLS SNI. **Accepted residual (decision), narrow:** ONLY transports that force hostname re-resolution (no IP-dial API) fall back to re-resolution; that DNS-rebinding TOCTOU gap is contained by the per-dial private-range rejection + port allowlist, not eliminated |
| Elevation | SSRF at Newznab indexer fetch: a user-supplied indexer base URL fetched server-side (same attacker shape as the vault guard, third surface) | **F-INGEST-01 validator + fetch policy model (no fetch-time runtime enforcement exists in this tree):** `IndexerConfig::validate` (`workers/api/src/vault.rs`) requires HTTPS, resolves ALL DNS records and rejects loopback/link-local (incl. the cloud metadata endpoint)/private/unique-local/CGNAT/multicast/reserved/documentation ranges (v4+v6, incl. IPv6 transition/special-use space); one forbidden record poisons the whole answer. It returns vetted addresses and pins the parsed URL port (default 443), but does not reject explicit non-443 ports. Separately, `workers/ingest/src/indexer_fetch.rs` models a stricter fetch-time policy (https, no embedded credentials, 443-only, resolve-ALL rejection, a `PinnedRequest` carrying vetted addresses + SNI host to a transport trait, supplied-delay deadlines of 5000 ms dial / 15000 ms total, library-only stable codes with 4xx/504/502 mappings, and a host-only log extractor). That module is **not** live enforcement: nothing outside tests calls it, no non-test resolver/transport implements its traits, it performs no DNS/TCP/TLS dial and no cancellation (its buffered `RawResponse` and supplied delays are checked only after the synchronous transport call returns, so wall-clock deadlines, streamed bounds and an actual TLS/DNS-pin boundary are undemonstrated), and no route emits its HTTP statuses or any decision log. **Required fetch contract (unchanged, not yet implemented):** refuse non-443 ports; resolve the host immediately before fetch and dial only those vetted addresses on 443 without another DNS lookup, retaining the hostname for TLS SNI/certificate checks and the Host header; forbid redirects. Required limits are a 5000 ms dial timeout and 15000 ms total timeout; guard blocks return 4xx, timeout 504, and upstream-status failures 502; logs carry host + code only after URL key redaction. **Unresolved discrepancy:** the model follows up to 3 re-validated redirects while this required policy (and the vault's `INDEXER_ALLOW_REDIRECTS = false`) forbids redirects. The model does not relax the required policy and is not approval to follow live redirects or to deploy an indexer fetch transport; any policy change needs an independent CISO disposition |
| Elevation | SSRF at NZB fetch-from-URL: a caller-supplied NZB URL fetched server-side | **F-INGEST-02 planned fetch-from-URL contract (no route or transport exists in this tree):** require HTTPS, no embedded credentials, port 443 only, and exact-match host allowlist with deny-all default; resolve ALL DNS records and reject loopback/link-local (incl. the cloud metadata endpoint)/private/unique-local/CGNAT/multicast/reserved/documentation ranges (v4+v6, incl. IPv6 transition/special-use space) even for allowlisted hosts — check IP literals directly without DNS; allow at most 3 redirects, re-validating each hop and never fetching forbidden, downgraded, or off-allowlist targets; for the initial request and every redirect hop, dial only the vetted addresses from that hop's validation with no second DNS lookup, retaining the hostname for TLS SNI/certificate checks and the Host header; enforce 10000 ms total timeout, return no partial bytes, cap fetched bodies at 2 MiB, map guard blocks to 4xx, timeout to 504, upstream-status failures to 502, and log host + code only (never the full URL). The existing 2 MiB cap applies to direct XML upload intake, not URL fetching; the fetch limit must be implemented separately

Key rules: KEK rotation is designed day one (versioned keys,
version-pinned decrypt, online re-wrap job — F-VAULT-02, normative design
in `kek-rotation.md`); DEK per user, never hard-coded; no credential in
Analytics Engine, KV, R2, Queues payloads, or error bodies. The KEK
re-wrap follow-up migration (F-VAULT-03, normative design in
`kek-rotation.md` §§2, 6, 8) adds `rewrapped_at` + a `kek_version`
index: re-wrap passes stamp `rewrapped_at` per row (migration 0015;
`rewrapped_at` is the authoritative re-wrap marker and `updated_at` is
no longer the re-wrap signal) and per-version counts drive the retire gate.

## 4. Sessions, passkeys, magic links (STRIDE)

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | Session theft / fixation; WebAuthn registration hijack | Signed cookies (`httpOnly`, `Secure`, `SameSite=Lax` minimum), server-side session row, freshness-gated privilege changes (re-auth + new session id — see Elevation row); WebAuthn RP ID pinned to the real origin; challenge single-use + short expiry |
| Tampering | Cookie editing; magic-link reuse | Cookies are signed (secret from env, never code); magic-link tokens are cryptographic-random (≥128-bit), hashed at rest, single-use, ≤15-min expiry |
| Repudiation | Disputed logins | Audit log: login/register/magic-link issue+consume/session revoke/account-email change (who/when/result) |
| Information disclosure | Session/token leak via logs or error pages | No token material in logs or error bodies; login errors are generic ("check your email") — no user-enumeration oracle |
| Denial of service | Magic-link email bombing; auth-endpoint hammering | Strict rate limits on auth endpoints (shared store), email-send cooldown per address; invite-code gate if abuse appears |
| Elevation | Fixed/forever sessions; leaked logout; stale session used for a privilege change; account-email takeover (there are no passwords — whoever controls the account email controls the account via magic link, so changing it is the main takeover/persistence path) | Bounded `maxAge`, rotation, explicit revoke-all on passkey add/remove; every endpoint re-checks authz, not just auth. **Email change requires** fresh passkey authentication (or confirmation via the OLD address), notification to the old address with a revert link, and revocation of ALL sessions plus outstanding magic links; the change is audit-logged (see Repudiation row). **F-SESS-01 status:** vault writes enforce a 600 s freshness window measured from immutable `created_at`; stale-but-valid sessions return `reauth_required` (401), while dead sessions return the generic auth 401 (`{"error":"check your email and try again"}`), never the reauth code. `POST /v1/session/reauthenticate/start` and `/finish` bind a fresh passkey assertion to the authenticated session's user. The finish handler calls `reauthenticate_session` with the credential's verified owner id; the store conditionally revokes the old row and inserts the replacement atomically, and only returns the new cookie after commit. The BYOP save flow offers this passkey reauthentication and retries the unchanged tested configuration; `login_finish` and `magic_finish` remain independent sign-ins. |

## 5. Signed stream grants (STRIDE)

Stream Worker serves Range requests with R2 cache; grants authorize fetches.

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | Forged or replayed grant | Grants are MAC-signed (server key) and bound to `user_id` + content id. **F-GRANT-01 status (implemented in the merged grant module, `workers/stream/src/grant.rs`):** the production-intended issuer `issue_at` clamps the requested lifetime to 900 s (`MAX_GRANT_TTL`); the raw signer `issue` intentionally accepts an arbitrary expiry for tests and offline tooling and does not clamp. `verify` rejects a validly signed, unexpired grant whose remaining lifetime (`exp - now`) exceeds 900 s + 60 s skew (`CLOCK_SKEW_SECS`; 960 s total) as `TtlTooLong`; `now >= exp` remains `Expired`, and skew never revives an expired grant. Contract coverage: `workers/stream/tests/grant_ttl_contract_acceptance.rs`. This states merged module behavior only — it is not evidence of deployed end-to-end enforcement, a staging/production pass, or that every caller uses `issue_at`; applicable implementation PRs still pass §8 |
| Tampering | Grant edited to another track/user | MAC covers ALL grant fields; any field change invalidates |
| Repudiation | Disputed fetches for metering | Metering events keyed to authenticated grant (Analytics Engine carries ids, never secrets) |
| Information disclosure | Grant URL leaks (logs, Referer) | Grants travel in `Authorization` header or POST body, never query strings; cache keys namespaced per user — NEVER serve user A's bytes for user B's grant (cache-poisoning = cross-user disclosure). **Browser media elements** (plain GET + Range, no custom headers): playback auth uses EITHER a fetch/MSE player sending the `Authorization` header, OR a path-scoped, short-TTL, `HttpOnly`/`Secure`/`SameSite=Lax` grant cookie minted alongside the signed grant — never a signed query-string grant; `Referrer-Policy: no-referrer`; logs scrub grant bytes |
| Denial of service | Grant minting loop; Range-abuse on origin | Mint rate-limited; Range clamped; R2 absorbs repeats; per-user concurrency cap in pool |
| Elevation | Grant for track X used against pool/account Y | Grant audience binds worker + user; pool DO re-validates grant before dialing provider |

## 6. Multi-tenant pool isolation (STRIDE)

Per-user NNTP pool Durable Object + streaming Worker (pool/streaming implementation card).

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | User A drives user B's pool connections | Pool DO id derived from authenticated `user_id`; grant re-validated inside the DO; no cross-user connection reuse |
| Tampering | Queue/job for user A processed with user B's creds | Every ingest/stream job carries authenticated `user_id`; credential lookup happens server-side from the vault, never from client-supplied fields |
| Repudiation | Disputed provider usage / plan overage | Per-user metering (Analytics Engine) on pool dials + bytes; plan limits enforced before dial |
| Information disclosure | Provider creds leak into pool logs/errors; NNTP banners echo secrets | Pool never logs credentials; provider errors mapped to generic codes before leaving the DO; `max connections` from the vault row is a ceiling, not a target |
| Denial of service | One tenant exhausts shared egress / provider connection cap | Per-user connection + rate caps (plan-tiered); per-user queues so one tenant's backlog never blocks another; total timeouts on every NNTP op |
| Elevation | Tenant escapes pool to read another tenant's vault row | Vault read path enforces owner check inside the DO call path too — defense in depth, not edge-only |
| Elevation | Pool dials a provider host whose DNS changed after save (save-time check bypass) | The §3 SSRF guard is a dial-time invariant: enforced on every connect inside the DO (resolve all records, reject private/reserved, NNTP-port allowlist); DNS-rebinding residual accepted, see §3 |

## 7. Public-repo hygiene (verified 2026-10-13, re-verified 2026-10-15
after the main merge: merge-queue `TOG-*` comments removed, two-layer
gitleaks pin negative-tested, lint covers binaries and rejects tracked
artifacts)

Verified against main post-merge (required checks report on
`merge_group` too, so the queue cannot land an un-scanned head):

- `gitleaks` full-history scan is a required check; on PRs the base-branch
  policy is enforced in TWO layers, because gitleaks reads
  `<source>/.gitleaksignore` from the scanned checkout IN ADDITION to
  `--gitleaks-ignore-path` (flag-pinning alone was reproduced bypassable
  with 8.30.1: key commit + fingerprint-ignore commit exits 0). Layer 1:
  `--gitleaks-ignore-path` points at a base-ref extraction (empty when the
  base has no ignore file). Layer 2: the checkout's own root
  `.gitleaksignore` is overwritten from the base ref (or deleted when the
  base has none) before the scan — history reads git objects, so this
  hides nothing. Negative-tested locally with the pinned 8.30.1 binary
  and the workflow's exact flags: bypass commit exits 0 without layer 2,
  1 with it. A PR-added ignore therefore takes effect only after merge,
  and the merge required this scan to pass. A PR touching the ignore file
  raises a warning annotation for reviewers. Inline `gitleaks:allow`
  comments never suppress findings (`--ignore-gitleaks-allow`; note: the
  flag stops suppression, it does not turn allows into failures — an
  allow comment on a real leak still reports the leak). ✅
- All workflows declare least-privilege `permissions:` (`contents: read`,
  plus `pull-requests: read` where the title must be read). ✅
- No `pull_request_target` anywhere (privilege-escalation vector banned). ✅
- `actions/checkout` (and all other actions) pinned by SHA. ✅
- `.gitignore` covers `.env*` (with `!.env.example` kept), `.dev.vars`,
  `.wrangler/`, dumps, plus `__pycache__/` and `*.pyc`/`*.pyo` (bytecode
  embeds absolute build paths — the `public-text` lint reports tracked
  artifacts as `binary-artifact`). ✅
- `CONTRIBUTING.md` bans internal ids/links in public text. ✅

The hygiene findings below are implemented in this PR series (Dependabot
updates config, the `public-text` CI lint, the PR-template secrets
affirmation); their cards track review + merge, not future work. The vault
findings are normative designs the implementation PR must satisfy (§8 gate).

## 8. Vault + auth review gate (standing)

This is a standing CISO gate for implementation PRs that touch `auth`,
`secret`, `credential`, `vault`, `session`, `passkey`, `grant`, pool
isolation, migrations, or workflow permissions. This docs-only refresh records
requirements; it does not implement or prove them. The slice-1 vault core
(`provider_vaults` + envelope code) merged before the gate existed; its
retroactive items are the `kek-rotation.md` §8 follow-ups (follow-up migration,
`kek_by_version`, re-wrap job, staging drill, retire-after, §10 drill). A CISO
verdict is required on each applicable implementation PR before merge. Normal
flow never waits on CISO — ONLY these paths do. The verdict checks, at the
exact implementation PR head:

1. Threat-model §3–§6 mitigations present for every touched path.
2. `gitleaks` + `ci-ok` + `pr-lint` green on that head; full-history scan clean.
3. No secret, token, KEK, DEK, or credential in code, tests, fixtures, logs, or
   error bodies (fixtures use synthetic creds from the NNTP test-server fixture corpus).
4. Authz on every new endpoint (owner-or-permitted check; no existence oracles).
5. Migrations reviewed for secret handling (no plaintext backfill, no secret in
   migration output).
6. Workflow changes keep least-privilege permissions and no `pull_request_target`.
7. For implementation PRs touching auth, grants, or media: auth changes
   implement the §4 email-change rule (fresh auth, old-address notice +
   revert, revoke-all) and privilege-freshness gate (stale sessions rejected
   with the CISO-approved response; re-auth mints a new id with a single-row
   revoke); grant mint/verify honors the §5 TTL cap (clamp at issue,
   `TtlTooLong` at verify); media playback uses a §5 transport (header player
   or grant cookie, never signed query strings).
8. KEK retire honors backup/PITR retention (`kek-rotation.md` §5, retire-after
   date); a KEK-exposure response follows the `kek-rotation.md` §10
   compromise path, never routine rotation alone.

## 9. Findings index

| Id | Finding | Card |
|---|---|---|
| F-VAULT-01 | `test connection` SSRF guard for user-supplied provider host/port | Paperclip child card (mapping in the threat-model card thread, never pasted here) |
| F-VAULT-02 | KEK rotation + versioning designed day one (re-wrap job) | Normative design: `kek-rotation.md`; tracking on the Paperclip child card (see above) |
| F-VAULT-03 | KEK rewrap follow-up migration: `rewrapped_at` + `kek_version` index; per-version progress counts | Normative design: `kek-rotation.md` §§2, 6, 8; tracking on the Paperclip child card (see above) |
| F-SESS-01 | Vault writes enforce 600 s freshness and return `reauth_required`; passkey reauthentication is session-bound and atomically replaces only the proven user's current row | Implemented by `POST /v1/session/reauthenticate/start` and `/finish` |
| F-GRANT-01 | Play-grant TTL cap: production-intended `issue_at` clamps to 15 min (900 s); raw `issue` does not clamp; `verify` rejects over-cap grants (`exp - now` > 960 s) as `TtlTooLong` and `now >= exp` as `Expired` | Implemented in `workers/stream/src/grant.rs`; contract tests in `workers/stream/tests/grant_ttl_contract_acceptance.rs` (merged module behavior, not a deployment claim) |
| F-INGEST-01 | Indexer validator (`workers/api/src/vault.rs`): HTTPS, resolve-ALL + private-range rejection, vetted target. Indexer fetch policy **model** (`workers/ingest/src/indexer_fetch.rs`, tests only — no live resolver/transport, route, HTTP response or log): 443-only, pin-request shape, supplied-delay deadlines. Required live fetch contract still not implemented; model follows ≤3 re-validated redirects vs. required forbidden redirects — discrepancy unresolved, policy change needs independent CISO disposition | Paperclip child card (see above) |
| F-INGEST-02 | Planned NZB fetch-from-URL SSRF guard (no route/transport here): exact-match allowlist, resolve-ALL + private-range rejection, pinned target (no second DNS lookup), ≤3 re-validated redirects and timeout; separate fetch-body cap (current 2 MiB cap covers uploads only) | Paperclip child card (see above) |
| F-HYG-01 | Dependabot updates config (alerts ≠ updates) | Paperclip child card (see above) |
| F-HYG-02 | CI lint banning internal ids/hosts/agent names in public text | Paperclip child card (see above) |
| F-HYG-03 | PR-template secrets affirmation checkbox | Paperclip child card (see above) |
| F-DEL-01 | Account-deletion fence gaps (required by ADR 0002 rule 3, not yet implemented): auth/session writers carry no fence in either state — the `scheduled`-only bound lands with the cancel-deletion and executor slice; no stream production sink exists (only `LEDGER_WRITE_FENCE_SQL` plus the in-memory `UsageLedger::record_fenced` mirror) | Paperclip child card (mapping in the threat-model card thread, never pasted here) |

V1 lessons carried forward: per-user resource authorization precedes lookup (no
existence oracles — cf. avatar-oracle fix); every credential-adjacent finding
gets a card with exact head, never a bare comment.

## 10. Demo-media surfaces (no auth by design)

`apps/web` serves `/demo/player` and `/api/demo/{audio,artwork}` (plus
`/api/demo/` 404-probe paths used only as broken-image fixtures) with
explicitly no auth. This is intentional, not an auth gap, and holds ONLY
while every bullet below stays true:

- Synthetic bytes only: the audio handler serves deterministic PCM WAV
  bytes synthesized in `demoMedia.ts` (`?track=<id>` selects among
  allowlisted salts; unknown ids serve the default stream); the artwork
  handler serves a constant placeholder SVG (the R2 object is
  "missing"). No provider dial, no R2/cache read, no vault, session,
  or grant lookup.
- No credential or identity surface: the routes accept no session,
  token, grant, or credential input and emit no authenticated bytes.
  Range handling slices the synthetic body; nothing reflects free-form
  request input into bodies or headers.
- Smoke-test scope only: the player page exists for seek-storm and
  artwork-fallback smoke tests. Real playback must use the §5 grant
  transport (header player or grant cookie, never signed query strings)
  and must never be pointed at these routes.

**Invariant violations** (treat as security findings requiring a CISO
disposition before merge): serving library, cache, or R2 bytes from a
demo route; reading or forwarding credentials, sessions, or grants
there; reflecting request input beyond the allowlisted `track` salt
selector and byte-range slicing; or wiring real user playback through
these routes. Any such change moves the route out of this section and
under §§4–6.

## 11. Web security-header posture (static headers + nonce CSP)

`apps/web` applies `SECURITY_HEADERS` (`src/lib/security/headers.ts`) to
every response from the `handle` hook (`src/hooks.server.ts`, `set` not
`append` so a route can never weaken or duplicate them), and emits the
`Content-Security-Policy` from SvelteKit's `csp` option
(`vite.config.ts`), which owns the per-request nonces. Unit coverage in
`src/lib/security/headers.test.ts`; header + no-violation coverage on
public pages in `tests/security-headers.spec.ts`. The entries below are
trust assumptions, not header documentation: each states what would
violate it.

- **Nonce CSP, no `unsafe-inline`/`unsafe-eval` on scripts.** `csp`
  runs in `mode: 'nonce'`: every inline script/style SvelteKit emits
  carries a per-request nonce. `script-src` is `'self'`-only (no
  `unsafe-inline`, no `unsafe-eval`); style *elements* stay nonce-only
  while `style-src-attr 'unsafe-inline'` covers style *attributes* only
  (the `app.html` wrapper, Svelte `style:` directives), which nonces
  cannot; `script-src-attr` allows exactly one inline handler via
  `unsafe-hashes` plus its pinned hash
  (`sha256-7dQwUgLau1NFCCGjfn9FsYptB6ZtWxJin6VohGIu20I=` for the Svelte 5
  SSR `onerror="this.__e=event"` capture on the artwork `<img>`
  fallback). `default-src`/`base-uri`/`form-action` are `'self'`,
  `object-src` is `'none'`, `frame-ancestors` is `'none'`,
  `img-src` is `'self'` + `data:`/`blob:` (artwork sources),
  `media-src`/`connect-src` are the `cspSources` allowlist only. **Violated
  by:** adding `unsafe-inline`/`unsafe-eval` to `script-src`; widening
  any directive to a wildcard or to an origin that serves
  user-controlled bytes, user data, or credentials (an allowlisted
  origin becomes a script/data exfiltration path); adding a second inline
  handler without a hash review (recompute the hash after a Svelte
  upgrade if the artwork fallback tests fail).
- **HSTS without preload (reversible, separate decision).**
  `Strict-Transport-Security: max-age=31536000; includeSubDomains` with
  deliberately no `preload`: preload submissions are hard to reverse and
  are a separate decision. **Violated by:** adding `preload` without that
  independent decision, or shortening/removing `max-age` so TLS
  downgrade protection silently lapses.
- **`Referrer-Policy: no-referrer` (load-bearing).** Magic-link tokens
  travel in the query string (`/auth/magic?token=…`, asset A5), so no
  outbound click or third-party subresource may ever leak them via
  `Referer`. **Violated by:** relaxing the policy (e.g. to `origin` or
  `unsafe-url`); adding a third-party subresource or outbound navigation
  that receives a `Referer`; or putting token/grant material in query
  strings on any new flow (signed stream grants already travel in the
  `Authorization` header, POST body, or grant cookie per §5, never query
  strings).
- **`Permissions-Policy` denies unused capabilities, never WebAuthn.**
  `camera=(), microphone=(), geolocation=()` turns off capabilities the
  app never uses; the policy must never mention
  `publickey-credentials-*`, or passkey signup/login (asset A4) breaks.
  **Violated by:** adding `publickey-credentials-get`/`-create`
  restrictions (breaks WebAuthn), or enabling a capability the app does
  not need (widens the compromise/request-forgery surface for no
  function).
- **Deny framing twice.** `X-Frame-Options: DENY` plus CSP
  `frame-ancestors 'none'` (defense in depth against clickjacking on
  auth/vault flows, assets A1–A5). **Violated by:** relaxing either side
  (`SAMEORIGIN`, `allow-from`, or a `frame-ancestors` origin list) so the
  login, magic-link, passkey, or vault pages become frameable.
- **Fail-closed `cspSources` (misconfiguration throws at build).** Empty
  base URL means same-origin (`'self'` only). Anything else must be an
  absolute `https:` URL with any path stripped to its origin; a
  plaintext or malformed API base throws and fails the build instead of
  silently widening or corrupting the policy. SvelteKit's `csp` option is
  evaluated at build time, so `PUBLIC_API_BASE_URL` must be set in the
  build environment — a runtime-only override leaves the compiled CSP
  narrower than the client. `allowHttpLoopback` exists for local
  development only (`vitest`, `vite dev`, `svelte-check` against a local
  API worker); production builds always pass `false`, so any `http:` API
  base — loopback included — still throws. **Violated by:** catching or
  ignoring the throw, allowing `http:` in a production build, or relying
  on a runtime-only API-base override that the compiled
  `media-src`/`connect-src` does not cover (browser blocks the calls the
  client tries to make).

## 12. Account-deletion writer fence (fenced writers merged; auth/session bound and stream sink required)

Account deletion first marks `users.deleting_at` and records the
operation as `scheduled` with a 24-hour cancellable grace window (ADR
0002 rule 3, normative text in `docs/adr/0002-data-model.md`; persistence
contract in `workers/api/src/account_deletion.rs`). From the moment the
marker is set, no writer may create or restore that account's
user-owned state: every such writer carries the fence **in-statement**,
selecting through `FROM (SELECT id FROM users WHERE id = $n AND
deleting_at IS NULL FOR SHARE) AS writer_fence` (`WRITER_FENCE_USER_SQL`).
`FOR SHARE` conflicts with the scheduling `UPDATE users` (which needs
the row lock) but not with sibling writers, so the marker either waits
for an in-flight write or fences every later one — no writer can pass
the marker check and commit after the marker. A fenced write affects 0
rows; the caller disambiguates with the lock-free probe
(`ACCOUNT_DELETING_PROBE_SQL`, never the fence itself) via
`classify_fenced_write`: 0 rows + marker set is the typed fence
rejection, 0 rows + fence open (or user gone) stays not-found, and a
landed write is never reclassified.

Status at this head: the api vault/lease/library fences and the ingest
upsert fence below are merged; the auth/session `scheduled`-only bound
and the stream production sink are required by ADR 0002 rule 3 but not
yet implemented (tracked as F-DEL-01 in §9). Required controls are
labelled as such throughout — nothing below presents one as merged.

Fenced writers (account-owned creates/restores, api/ingest/stream):

- **api vault credential upsert** (`upsert_vault` in
  `workers/api/src/worker_entry.rs`): fenced `INSERT ... SELECT`;
  a 0-row upsert fails closed and the handler classifies it through
  the `account_deleting` probe into the typed 409. `delete_vault`
  is deliberately NOT fenced — removal cannot restore state, and the
  erasure executor deletes the row anyway.
- **api test-connection lease acquire** (`vault_test_leases` insert in
  `workers/api/src/worker_entry.rs`): fenced like the upsert, so a
  deleting account acquires no new lease rows; a fenced lease returns
  the typed 409 `account_deleting` (probed once), never the boolean
  collapse.
- **api library genre/playlist mutations** (`workers/api/src/library.rs`):
  every create/update/delete statement carries the fence in-statement
  (user-initiated deletes are deliberately fenced — a 409 during grace);
  0 affected rows map via `fenced_write_outcome` to
  `LibraryError::AccountDeleting` (HTTP 409) when the probe shows the
  marker set, else `NotFound`.
- **ingest library upsert** (`workers/ingest/src/library.rs`): the write
  selects through the job owner's `users` row `FOR SHARE`; 0 rows map
  through `upsert_job_result` to the permanent `account_deleting`
  failure — never retried behind the delete.
- **stream usage-ledger / play-event write**
  (`workers/stream/src/meter.rs`): required, not yet implemented (F-DEL-01)
  — no production sink exists (`workers/stream/README.md`: metering "is
  not yet wired into the production `play()` path or production sinks";
  `UsageLedger` is an in-memory portable-core model, not a live Hyperdrive
  table). When the sink lands, its INSERT must select through
  `LEDGER_WRITE_FENCE_SQL`; the portable-core mirror
  (`UsageLedger::record_fenced`) already reports
  `RecordOutcome::AccountDeleting` and appends nothing, so a play can
  never repopulate a deleting account's ledger/event rows. Zero rows is
  ambiguous under `DO NOTHING` (a duplicate redelivery with an open
  fence also affects 0 rows), so the production sink must converge with
  `DO UPDATE` or probe the fence first — healthy duplicates must never
  read as `account_deleting`.

Exemptions (bounded, ADR 0002 rule 3 — required, not yet implemented,
F-DEL-01): the auth/session writers needed to cancel during grace (new
`sessions` login rows, magic-link issue/consume, challenge/assertion,
sign-count updates, step-up begin/verify/consume, audit writes, rotation,
cancel-deletion) MUST be exempt ONLY while the account operation is in
`state = 'scheduled'` (the only state where cancel can win the
compare-and-set); once it is `running` those writers must be fenced like
any other. At this head those writers carry no fence in either state
(plain INSERTs in `workers/api/src/worker_entry.rs`; routes and execution
are later slices per `workers/api/src/account_deletion.rs`, and nothing
yet gates on `delete_operations.state`) — the bound lands with the
cancel-deletion and executor slice. The executor's own cursor/state
writes are exempt outright. Everything else fails closed.

Stable contract (`docs/api-errors.md` registry,
`workers/api/src/api_error.rs`): HTTP **409**, `code:
account_deleting`, **`retryable: false`** — "Nothing was written; do
not retry unchanged." The api worker owns the wire emission; ingest and
stream carry the same stable code as a permanent non-retryable
job/ledger outcome.

**Assumption violations** (each breaks this section; treat as a
security finding requiring a CISO disposition before merge):

- A create/restore writer without the in-statement fence — e.g. a new
  writer that skips `writer_fence`, splits the marker check and the
  write across statements (check-then-write TOCTOU), or gates on the
  lock-free probe instead of the locked in-statement subquery. Any of
  these lets a write commit behind the marker and repopulate state the
  erasure pass already swept.
- A fence that takes no lock (plain `deleting_at IS NULL` predicate
  without `FOR SHARE`): reopens the marker race the lock closes.
- An exemption that outlives its bound (once the F-DEL-01 bound is
  implemented): auth/session writers exempt while the operation is
  `running`, any new exemption not tied to the `scheduled` state, or
  executor writes touching user-owned state beyond cursor/state.
  Post-grace logins, sessions, or step-up proofs on a doomed account are
  a takeover/persistence path, not liveness. (The current absence of any
  auth/session fence is the tracked F-DEL-01 gap, not a new violation of
  this section.)
- Fencing a pure removal path (the `delete_vault` inversion):
  `delete_vault` and the erasure executor's own cleanup must stay
  unfenced so erasure and cancel cleanup always land.
  (User-initiated library/playlist UPDATE/DELETE statements are
  deliberately fenced by design — a 409 during grace — and are not
  covered by this rule.)
- Replacing the 409 with any other shape: a retryable 5xx turns the
  fence into a retry storm behind a delete that never reopens (ingest
  must keep the permanent-failure mapping); a 404 misleads the client
  into recreate flows and conflates "fenced" with "not yours / no
  such row", breaking the §3 no-oracle shape; queuing a fenced write
  behind the delete violates fail-closed.

## 13. Future Worker deploy-lane rollback: trust assumptions (docs-only)

**Status: no Worker deploy lane exists.** `.github/workflows/deploy.yml`
deploys `apps/web` only; no tracked workflow deploys the backend Workers
under `workers/`. This section records trust assumptions only. It grants
no deployment authorization, creates no lane, and changes neither the
decided stack (§1) nor the standing review gate (§8).

It is the trust companion to the procedure in `docs/ops.md` ("Future
Worker deploy lanes: required rollback evidence", items 1–6). Procedure
and trust boundary agree: a Worker rollback never restores data or the
state of connected resources (Hyperdrive config, KV/R2/Durable Object
contents, Access policies; cf. `docs/ops.md`: "Rollback is never a data
restore"). Whether it also restores secrets, vars, and binding/route/
migration declarations depends on the mechanism (items 2–3); the
`docs/ops.md` "code only" wording is that evidence rule's shorthand for
this boundary. Where they overlap, the evidence rule governs
what must be retained and this section governs what must be trusted.

**Rollback actor(s).** The only actor this note contemplates is the
identity permitted to Re-run the future Worker deploy workflow's
deploy/rollback job, acting through the repo's Cloudflare CI credentials
(secret names only, never values; scope recorded in `docs/ops.md`, "CI API
token + GitHub secrets"). Re-running is a privileged deploy action, not a
read-only operation: whoever can re-run that job can ship Worker code to
the target environment once the lane exists. Its reach is bounded by that
identity's GitHub permissions plus the CI token's Cloudflare scope, and by
the lane's own required gates on the exact head being (re)deployed. No
other actor — reader, commenter, or holder of an unrelated token — gains
any deploy capability from this note.

**What re-running an old-SHA deploy implicitly trusts.** Whichever
mechanism the future lane implements (redeploy of the prior known-good
source revision, or activation of the prior version), the rollback trusts
all three of the following at re-run time, not at the original deploy time:

1. **Artifact integrity.** That the old source revision is the same code
   that passed exact-head CI and review when it was the known-good
   version (prior version, SHA, deploy run, and health result recorded
   *before* the new deploy per `docs/ops.md` item 4); that a rebuild from
   that SHA is faithful to what was reviewed (toolchain/pinning drift can
   break this — hence the deployed-build identity check in `docs/ops.md`
   items 2 and 6); and that source control and the build path have not been
   tampered with since. A green rollback run alone does not prove the prior
   version is serving — the post-rollback health result plus the
   deployed-build identity do.
2. **Secret versions (mechanism-dependent).** A redeploy of the prior
   known-good source revision keeps the *current* Worker secret values
   and trusts the current secrets, KEK versions, Hyperdrive credentials,
   and Access policies — not the values in force at the old deploy.
   Activation of a prior version instead restores that version's
   snapshotted secret values, vars, and binding config as a unit (a Worker
   version snapshots code plus bindings — secrets are a binding type —
   plus vars and compatibility settings), so it can bring back a
   pre-rotation `VAULT_KEK_BASE64` / `VAULT_KEK_VERSION` or
   `SESSION_SECRET_BASE64` (read at `workers/api/src/worker_entry.rs`).
   Old code must tolerate current secret versions (e.g. KEK version-pinned
   decrypt, `kek-rotation.md`; the `kek_version` column is the only version
   signal the decrypt path trusts). A forward-incompatible secret change
   (new KEK generation retired, credential format change) can make a code
   rollback fail or misbehave even when the rollback run is green.
3. **Binding state (mechanism-dependent).** Activation of a prior version
   restores that version's binding targets, vars, and compatibility
   settings; a redeploy of an old SHA reapplies that SHA's Wrangler
   binding, route, and migration declarations. Neither mechanism restores
   resource state (Hyperdrive config, KV/R2 contents, Durable Object
   storage, Access policies), which stays current as inventoried in
   `docs/ops.md`. If the deploy being rolled back also changed any of
   these (or schema/stored data), the rollback evidence is incomplete until
   it states, per change, that the restored version is compatible with the
   current state or records that change's own rollback. Evidence records
   carry names, version identifiers, commit SHAs, and run/health-check URLs
   only — never secret values (CI logs are attacker-readable, §2).

**Explicit residual risk (accepted, not mitigated here):**

- Old code against current secrets/bindings/schema may be incompatible;
  fail-closed behavior is preferred but not guaranteed for every Worker.
- A rebuilt artifact may differ byte-for-byte from the originally
  deployed version if the toolchain is unpinned; without a
  deployed-build identity matched to the rollback target, serving state
  is unproven.
- The re-run capability itself is a deploy capability: anyone holding it
  can restore any recorded known-good SHA, including one predating a
  security fix. The mitigation is the lane's exact-head gates and the
  prior-known-good recording rule — not this note.
- Data written by the newer version persists after a code rollback;
  forward-only migrations in particular have no code-rollback path.
- Activating a pre-rotation version after a secret or KEK compromise
  rotation (`kek-rotation.md` §10, `session-secret-rotation.md` §6) undoes
  the rotation by restoring the exposed secret values; such versions are
  not rollback targets unless re-keyed first (e.g. `wrangler versions
  secret put`), and the `docs/ops.md` item 4 prior-known-good record must
  postdate the latest secret rotation.
- A first deploy has no prior known-good version, so rollback acceptance
  cannot be claimed for it (per `docs/ops.md` item 4); removal/disablement
  is a separate gated action.

## 14. Staging-only emailed-code retrieval seam (`POST /api/test-codes`)

The seam exists so the staging E2E can complete the real `/signup` form with the genuinely issued inbox-proof code; its gates are layered so that no single misconfiguration exposes a real account's code: capture writes the raw token to the migration 0018 outbox only when the explicit `QA_CODE_SEAM` staging flag is armed and the recipient is an `example.test` mailbox (RFC 2606 reserved, never routable, so no real user is ever captured — a deliberate narrow exception to the §4 "plaintext token never stored" invariant), retrieval additionally requires the staged `QA_AUTH_TOKEN` bearer under constant-time comparison and returns only an unexpired code for a well-formed synthetic mailbox, every other posture (flag off, bearer unstaged or wrong, real mailbox, miss, expiry) answers the indistinguishable unknown-route 404 or a names-only 401 with no mailbox, token, or gate detail, the handler is strictly read-only (it creates no users, challenges, or magic rows), and production — which never sets the flag or stages the bearer — serves the path as a plain 404, proven by the `qa_seam_absent_without_the_staging_flag` gate test; residual risks accepted here are bearer brute-force without lockout (mitigated by the Cloudflare Access wall in front of staging plus a high-entropy token, not by this note) and plaintext test codes at rest in the isolated staging database (staging roles cannot reach prod per `docs/ops.md`).
