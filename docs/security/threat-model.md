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
| Elevation | SSRF at Newznab indexer fetch: a user-supplied indexer base URL fetched server-side (same attacker shape as the vault guard, third surface) | **F-INGEST-01 validator (fetch-time enforcement is not present in this tree):** `IndexerConfig::validate` requires HTTPS, resolves ALL DNS records and rejects loopback/link-local (incl. the cloud metadata endpoint)/private/unique-local/CGNAT/multicast/reserved/documentation ranges (v4+v6, incl. IPv6 transition/special-use space); one forbidden record poisons the whole answer. It returns vetted addresses and pins the parsed URL port (default 443), but does not reject explicit non-443 ports. No indexer fetch transport exists in this tree. **Required fetch contract:** refuse non-443 ports; resolve the host immediately before fetch and dial only those vetted addresses on 443 without another DNS lookup, retaining the hostname for TLS SNI/certificate checks and the Host header; forbid redirects. Required limits are a 5000 ms dial timeout and 15000 ms total timeout; guard blocks return 4xx, timeout 504, and upstream-status failures 502; logs carry host + code only after URL key redaction |
| Elevation | SSRF at NZB fetch-from-URL: a caller-supplied NZB URL fetched server-side | **F-INGEST-02 planned fetch-from-URL contract (no route or transport exists in this tree):** require HTTPS, no embedded credentials, port 443 only, and exact-match host allowlist with deny-all default; resolve ALL DNS records and reject loopback/link-local (incl. the cloud metadata endpoint)/private/unique-local/CGNAT/multicast/reserved/documentation ranges (v4+v6, incl. IPv6 transition/special-use space) even for allowlisted hosts — check IP literals directly without DNS; allow at most 3 redirects, re-validating each hop and never fetching forbidden, downgraded, or off-allowlist targets; enforce 10000 ms total timeout, return no partial bytes, cap fetched bodies at 2 MiB, map guard blocks to 4xx, timeout to 504, upstream-status failures to 502, and log host + code only (never the full URL). The existing 2 MiB cap applies to direct XML upload intake, not URL fetching; the fetch limit must be implemented separately |

Key rules: KEK rotation is designed day one (versioned keys,
version-pinned decrypt, online re-wrap job — F-VAULT-02, normative design
in `kek-rotation.md`); DEK per user, never hard-coded; no credential in
Analytics Engine, KV, R2, Queues payloads, or error bodies. The KEK
re-wrap follow-up migration (F-VAULT-03, normative design in
`kek-rotation.md` §§2, 6, 8) adds `rewrapped_at` + a `kek_version`
index: re-wrap passes stamp `rewrapped_at` per row (until the migration
lands, progress is tracked in `rotation_runs` with `updated_at` as the
last-touch signal) and per-version counts drive the retire gate.

## 4. Sessions, passkeys, magic links (STRIDE)

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | Session theft / fixation; WebAuthn registration hijack | Signed cookies (`httpOnly`, `Secure`, `SameSite=Lax` minimum), server-side session row, freshness-gated privilege changes (re-auth + new session id — see Elevation row); WebAuthn RP ID pinned to the real origin; challenge single-use + short expiry |
| Tampering | Cookie editing; magic-link reuse | Cookies are signed (secret from env, never code); magic-link tokens are cryptographic-random (≥128-bit), hashed at rest, single-use, ≤15-min expiry |
| Repudiation | Disputed logins | Audit log: login/register/magic-link issue+consume/session revoke/account-email change (who/when/result) |
| Information disclosure | Session/token leak via logs or error pages | No token material in logs or error bodies; login errors are generic ("check your email") — no user-enumeration oracle |
| Denial of service | Magic-link email bombing; auth-endpoint hammering | Strict rate limits on auth endpoints (shared store), email-send cooldown per address; invite-code gate if abuse appears |
| Elevation | Fixed/forever sessions; leaked logout; stale session used for a privilege change; account-email takeover (there are no passwords — whoever controls the account email controls the account via magic link, so changing it is the main takeover/persistence path) | Bounded `maxAge`, rotation, explicit revoke-all on passkey add/remove; every endpoint re-checks authz, not just auth. **Email change requires** fresh passkey authentication (or confirmation via the OLD address), notification to the old address with a revert link, and revocation of ALL sessions plus outstanding magic links; the change is audit-logged (see Repudiation row). **F-SESS-01 required behavior (not implemented in this tree):** privilege-gated operations must require immutable authentication age (`now - created_at`; rotation preserves `created_at`) within 600 s. The intended contract is `reauth_required` for stale-but-valid sessions, a new session id on successful re-auth with only the old row revoked, and `invalid_session` for dead sessions. Current session code has only generic `SessionError::Invalid`; neither the 600 s gate nor these codes exist. These are normative requirements, not current runtime behavior; an implementation PR must establish the CISO-approved error taxonomy before clients rely on it |

## 5. Signed stream grants (STRIDE)

Stream Worker serves Range requests with R2 cache; grants authorize fetches.

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | Forged or replayed grant | Grants are MAC-signed (server key) and bound to `user_id` + content id. **F-GRANT-01 required behavior (not implemented by the current grant module):** issue must clamp requested lifetime to 900 s (`MAX_GRANT_TTL`); verify must reject a validly signed, unexpired grant whose remaining lifetime (`exp - now`) exceeds 900 s + 60 s skew as `TtlTooLong`; `now >= exp` remains `Expired` (skew never resurrects a grant). Current `issue` accepts arbitrary expiry and `verify` only checks expiration. This is a normative requirement, not current runtime behavior; do not rely on the cap until an implementation PR passes §8 |
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
   date); a KEK-exposure response follows the §10 compromise path, never
   routine rotation alone.

## 9. Findings index

| Id | Finding | Card |
|---|---|---|
| F-VAULT-01 | `test connection` SSRF guard for user-supplied provider host/port | Paperclip child card (mapping in the threat-model card thread, never pasted here) |
| F-VAULT-02 | KEK rotation + versioning designed day one (re-wrap job) | Normative design: `kek-rotation.md`; tracking on the Paperclip child card (see above) |
| F-VAULT-03 | KEK rewrap follow-up migration: `rewrapped_at` + `kek_version` index; per-version progress counts | Normative design: `kek-rotation.md` §§2, 6, 8; tracking on the Paperclip child card (see above) |
| F-SESS-01 | Required privilege-change re-auth: 600 s freshness on immutable auth age, proposed `reauth_required` response, new session id with single-row revoke; not implemented in the current session module | Paperclip child card (see above) |
| F-GRANT-01 | Required play-grant TTL cap: clamp to 15 min at issue; reject over-cap grants at verify (`TtlTooLong`) with 60 s skew bound; not implemented in the current grant module | Paperclip child card (see above) |
| F-INGEST-01 | Indexer validator: HTTPS, resolve-ALL + private-range rejection, pinned target; required fetch contract (not implemented here): 443-only, redirects forbidden, timeouts, response mapping | Paperclip child card (see above) |
| F-INGEST-02 | Planned NZB fetch-from-URL SSRF guard (no route/transport here): exact-match allowlist, resolve-ALL + private-range rejection, ≤3 re-validated redirects and timeout; separate fetch-body cap (current 2 MiB cap covers uploads only) | Paperclip child card (see above) |
| F-HYG-01 | Dependabot updates config (alerts ≠ updates) | Paperclip child card (see above) |
| F-HYG-02 | CI lint banning internal ids/hosts/agent names in public text | Paperclip child card (see above) |
| F-HYG-03 | PR-template secrets affirmation checkbox | Paperclip child card (see above) |

V1 lessons carried forward: per-user resource authorization precedes lookup (no
existence oracles — cf. avatar-oracle fix); every credential-adjacent finding
gets a card with exact head, never a bare comment.
