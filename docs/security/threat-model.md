# NNTune v2 Threat Model (STRIDE)

Owner: CISO · Status: living document (finding IDs map to Paperclip child cards; card identifiers are never pasted into this public repo) —
update it when the vault, session, grant, or pool designs change, and re-request
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
| Tampering | Ciphertext swapped between rows; KEK confusion across envs | AEAD (AES-256-GCM) with per-row nonce + associated data binding `user_id`; KEK version column; staging and prod KEKs never shared |
| Repudiation | "I never stored that provider" / silent credential overwrite | Audit log: vault create/update/delete + test-connection attempts (who/when/source IP/result — NEVER the secret); no log line may contain credential material |
| Information disclosure | SQL dump, log leak, error echo, backup exposure | Envelope encryption at rest; parameterized queries only; generic errors ("vault unavailable"); `test connection` returns boolean only, never provider banners; backups inherit encryption (ciphertext only) |
| Denial of service | Vault row hammering; KEK-fetch storms | Rate-limit vault writes + test-connection per user (shared store, not in-memory — serverless); test-connection has dial + total timeouts and per-user concurrency cap |
| Elevation | `test connection` SSRF: user-supplied host/port aimed at internal targets | **SSRF guard (F-VAULT-01):** allowlist scheme/host shape, resolve ALL DNS records, reject loopback/link-local/private/unique-local (v4+v6), forbid redirects, dial timeout; treat provider host as hostile input |

Key rules: KEK rotation is designed day one (versioned keys, dual-read
decrypt, online re-wrap job — F-VAULT-02, normative design in
`kek-rotation.md`); DEK per user, never hard-coded; no credential in Analytics Engine,
KV, R2, Queues payloads, or error bodies.

## 4. Sessions, passkeys, magic links (STRIDE)

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | Session theft / fixation; WebAuthn registration hijack | Signed cookies (`httpOnly`, `Secure`, `SameSite=Lax` minimum), server-side session row, rotation on privilege change; WebAuthn RP ID pinned to the real origin; challenge single-use + short expiry |
| Tampering | Cookie editing; magic-link reuse | Cookies are signed (secret from env, never code); magic-link tokens are cryptographic-random (≥128-bit), hashed at rest, single-use, ≤15-min expiry |
| Repudiation | Disputed logins | Audit log: login/register/magic-link issue+consume/session revoke (who/when/result) |
| Information disclosure | Session/token leak via logs or error pages | No token material in logs or error bodies; login errors are generic ("check your email") — no user-enumeration oracle |
| Denial of service | Magic-link email bombing; auth-endpoint hammering | Strict rate limits on auth endpoints (shared store), email-send cooldown per address; invite-code gate if abuse appears |
| Elevation | Fixed/forever sessions; leaked logout | Bounded `maxAge`, rotation, explicit revoke-all on password/passkey change; every endpoint re-checks authz, not just auth |

## 5. Signed stream grants (STRIDE)

Stream Worker serves Range requests with R2 cache; grants authorize fetches.

| Threat | Attack | Mitigation (required) |
|---|---|---|
| Spoofing | Forged or replayed grant | Grants are MAC-signed (server key), short TTL (minutes), bound to `user_id` + content id; `nbf`/`exp` enforced with clock-skew cap |
| Tampering | Grant edited to another track/user | MAC covers ALL grant fields; any field change invalidates |
| Repudiation | Disputed fetches for metering | Metering events keyed to authenticated grant (Analytics Engine carries ids, never secrets) |
| Information disclosure | Grant URL leaks (logs, Referer) | Grants travel in `Authorization` header or POST body, not query strings; cache keys namespaced per user — NEVER serve user A's bytes for user B's grant (cache-poisoning = cross-user disclosure) |
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

## 7. Public-repo hygiene (verified 2026-10-13)

Verified against the scaffold at `11d1f03`:

- `gitleaks` full-history scan is a required check; config uses the base branch's
  policy on PRs so a PR cannot silence its own scan; `--ignore-gitleaks-allow`
  means inline `gitleaks:allow` comments FAIL the scan (fail-closed). ✅
- All three workflows declare least-privilege `permissions:` (`contents: read`,
  plus `pull-requests: read` where the title must be read). ✅
- No `pull_request_target` anywhere (privilege-escalation vector banned). ✅
- `actions/checkout` pinned by SHA. ✅
- `.gitignore` covers `.env*` (with `!.env.example` kept), `.dev.vars`,
  `.wrangler/`, dumps. ✅
- `CONTRIBUTING.md` bans internal ids/links in public text. ✅

Residual hygiene findings are filed as cards (see §9) — they are hardening, not
holes: Dependabot updates config, a CI lint for internal identifiers in public
text, and a PR-template secrets affirmation.

## 8. Vault + auth review gate (standing)

No vault/auth implementation PR exists yet (checked 2026-10-13; only the CI
probe PR is open). When the accounts/auth/vault implementation PR (or any PR touching
`auth`, `secret`, `credential`, `vault`, `session`, `passkey`, `grant`, pool
isolation, migrations, or workflow permissions) is ready, the CISO verdict is a
comment-gate on that card BEFORE merge. Normal flow never waits on CISO — ONLY
these paths do. The verdict checks, at the exact PR head:

1. Threat-model §3–§6 mitigations present for every touched path.
2. `gitleaks` + `ci-ok` + `pr-lint` green on that head; full-history scan clean.
3. No secret, token, KEK, DEK, or credential in code, tests, fixtures, logs, or
   error bodies (fixtures use synthetic creds from the NNTP test-server fixture corpus).
4. Authz on every new endpoint (owner-or-permitted check; no existence oracles).
5. Migrations reviewed for secret handling (no plaintext backfill, no secret in
   migration output).
6. Workflow changes keep least-privilege permissions and no `pull_request_target`.

## 9. Findings index

| Id | Finding | Card |
|---|---|---|
| F-VAULT-01 | `test connection` SSRF guard for user-supplied provider host/port | Paperclip child card (mapping in the threat-model card thread, never pasted here) |
| F-VAULT-02 | KEK rotation + versioning designed day one (re-wrap job) | Normative design: `kek-rotation.md`; tracking on the Paperclip child card (see above) |
| F-HYG-01 | Dependabot updates config (alerts ≠ updates) | Paperclip child card (see above) |
| F-HYG-02 | CI lint banning internal ids/hosts/agent names in public text | Paperclip child card (see above) |
| F-HYG-03 | PR-template secrets affirmation checkbox | Paperclip child card (see above) |

V1 lessons carried forward: per-user resource authorization precedes lookup (no
existence oracles — cf. avatar-oracle fix); every credential-adjacent finding
gets a card with exact head, never a bare comment.
