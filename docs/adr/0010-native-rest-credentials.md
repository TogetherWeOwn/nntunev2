# ADR 0010: Scoped Native REST Credentials

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-17
- **Roadmap:** resolves the ADR 0006 deferral
  ("Native REST bearer keys are deferred to a separate scoped-credential
  design, not borrowed from Subsonic").

## Context

Native REST (`/v1`) is the canonical API and the only surface `apps/web`
uses (ADR 0006). At launch it authenticates one way: the session cookie
(ADR 0008). That leaves no programmatic access path: scripts, CLI tooling,
and integrations cannot hold a cookie session, and ADR 0006 explicitly
forbids borrowing Subsonic credentials for native access
(`scope='subsonic'` reaches only `/rest/*` — never `/v1`). The gap will
otherwise be filled ad hoc (shared sessions, credential reuse across
surfaces), which is worse than a designed credential.

## Decision

**Native REST credentials are IN scope for NNTune v2** as a bounded,
least-privilege bearer credential (`Authorization: Bearer`), reusing the
storage and lifecycle patterns already accepted for Subsonic `apiKey`
credentials (ADR 0002 §2, ADR 0006 rules). This ADR defines the design
only; it does not authorize issuance code, storage, or any production
secret — implementation needs its own CISO-gated card (threat-model §8:
this touches `credential`/`auth`, so the standing vault/auth review gate
applies to the implementation PR).

### Credential shape

- Opaque 256-bit CSPRNG secret (minimum entropy 128 bits), server-generated
  only — never caller-selected. Display form `ntk_<base62>`; the `ntk_`
  prefix identifies the credential class at a glance and in logs.
- **One-time display:** plaintext is shown once at creation and never
  stored, never returned again. What persists is an HMAC-SHA256 of the key
  plus a short non-secret prefix (for "which key is this" listing).
- **Storage/hash model:** versioned HMAC lookup identical in structure to
  the Subsonic key form — composite `UNIQUE (pepper_version, key_hmac)`,
  keyed by the exact version in a dedicated `NATIVE_REST_PEPPER`
  Worker-secret keyring (separate from `SUBSONIC_INDEX_PEPPER` so either
  surface rotates without forcing the other). Dual-version lookup (at most
  current + previous) during a transition; old-version match is re-HMACed
  to current using the presented key, atomically, without storing
  plaintext. The ADR 0002 retirement precondition applies unchanged: a
  stored HMAC cannot be migrated without the key, so an old pepper retires
  only when no unrevoked credential still uses it — dormant clients must
  authenticate/migrate or explicitly reissue first. No time-based silent
  retirement.

### Additive schema (target; implementation work remains)

```text
native_rest_credentials  id PK, user_id FK, label TEXT, key_prefix TEXT,
                         key_hmac TEXT NOT NULL, pepper_version INTEGER NOT NULL,
                         UNIQUE (pepper_version, key_hmac),
                         scopes TEXT[] NOT NULL, expires_at NOT NULL,
                         created_at, last_used_at NULL, revoked_at NULL,
                         created_from_ip TEXT NOT NULL
```

New table only. No applied migration is edited; no Subsonic, session, or
vault row changes. Unless stated otherwise, ids are UUID and timestamps
are `timestamptz`, per ADR 0002 conventions.

### Scopes (allowlist, checked before handler access)

Minimal enumerated vocabulary mapped onto the `/v1` resources in
ADR 0006; default issuance is `library:read` only:

- `library:read` — catalog reads: artists, releases, tracks, search,
  library, playlist reads, cover art.
- `playlists:write` — playlist create/edit/delete (dormant-tombstone and
  restore rules in ADR 0006 apply unchanged).
- `plays:write` — scrobble / play events.
- `stream:read` — playback via the §5 grant transport (cost-bearing:
  drives provider egress and metering like any other grant).
- `ingest:submit` — ingest job submission (cost-bearing: plan-capped per
  ADR 0004).
- `providers:manage` — BYOP vault writes (custodial: touches user-owned
  provider secrets; highest-risk scope).

**Never grantable to a bearer key (session-only, enforced by the auth
layer, not by scope strings):** account deletion / cancel-deletion,
email change, passkey add/remove, issuance or revocation of any
credential (native or Subsonic), billing operations, and step-up
proof begin/complete. A bearer key also cannot mint stream grants for
another user or read another user's vault rows — owner checks apply
inside every path (threat-model §6 defense in depth).

### Issuance, expiry, revocation

- Issuance requires an authenticated session passing the privilege-freshness
  gate (immutable auth age within 600 s, F-SESS-01 contract): stale
  sessions get `reauth_required`, not a key. Issuance writes an `audit_log`
  row (ids only) and records `created_from_ip`.
- Expiry is mandatory: user-chosen up to a **365-day maximum**; no
  never-expiring keys. Expired keys fail closed with the generic
  unauthenticated response (no existence oracle).
- Revocation is immediate (`revoked_at` timestamp; checked on every
  request alongside expiry and scope). Users can list (prefix, label,
  scopes, expiry, `last_used_at` — never secret material) and revoke
  their own keys from a session-authenticated endpoint.
- Per-credential rate limits apply (same caps family as ADR 0004; no
  bypass of plan limits), and `last_used_at` anomalies (burst from a new
  IP/ASN) alert, mirroring the Subsonic rule.

### Step-up

Issuance-time freshness (above) is the step-up for this credential class:
there is no standing bearer privilege to escalate because high-impact
actions are session-only by construction. If a future scope is judged
privilege-equivalent to today's session-only set, it needs a new ADR —
scope creep by code change is denied.

### Audit

`audit_log` rows for create, revoke, failed-auth spikes, and anomaly
alerts carry credential id, user, timestamp, source IP, and result —
never secret material (threat-model §§3–4 logging rules). Account
completion pseudonymizes these rows under the existing rule 3 sentinels.

### Client transport requirements

- `Authorization: Bearer` header only. A bearer secret in a query string
  is rejected (same rationale as the `p=` rejection: query strings land
  in logs; threat-model §5).
- TLS everywhere (assumption 4 of the threat model). No key material in
  error bodies, access logs, traces, or Analytics Engine events; server
  logs redact the header the way `/rest/*` logging redacts `u/p/t/s/apiKey`.
- Keys authenticate `/v1` only. They never authenticate `/rest/*`
  (Subsonic), provider dials, or ingest fetches directly — those paths
  keep their own credentials (ADR 0006, threat-model §§3, 6).

## Threat model (delta; threat-model §8 gate still applies)

New asset: **native REST bearer secrets** (client-held) alongside A3
(session tokens). STRIDE coverage reuses existing mitigations:

- **Spoofing** (stolen key): blast radius bounded by scopes + expiry;
  `providers:manage` keys are the crown jewels and default-off.
- **Tampering** (HMAC swapped between rows): composite unique lookup;
  a copied HMAC is unusable without the key, and cross-row swaps fail
  authentication (cf. ADR 0002 envelope-swap tests).
- **Repudiation**: issuance/use/revocation audit rows (above).
- **Information disclosure** (SQL dump, log leak): HMAC-only at rest;
  one-time display; header-only transport; generic failures.
- **Denial of service**: per-credential rate limits; shared-store
  (serverless-safe) counters.
- **Elevation**: scope allowlist + session-only action set; owner checks
  on every path; no existence oracles (non-owner/non-member reads stay
  404/70-shaped per ADR 0006).

The implementation PR updates `threat-model.md` §§2/4–5 findings table
and re-requests CISO review; this ADR alone changes no code or model text.

## Migration compatibility

- Additive table; existing sessions, Subsonic credentials, and vault rows
  are untouched and keep working. No backfill, no plaintext handling.
- Cross-surface use is denied in both directions: Subsonic credentials
  cannot authenticate `/v1` (ADR 0006, unchanged), native keys cannot
  authenticate `/rest/*` (this ADR). Rotation on either keyring never
  forces rotation on the other.
- Clients that only hold session cookies see no behavior change.

## Non-goals

No OAuth/OIDC third-party delegation, no per-row ACLs beyond scopes, no
service-to-service auth change, no token exchange or impersonation, no
widening of the Subsonic subset, no implementation in this card.

## Consequences

- Programmatic `/v1` access has a designed, least-privilege path whenever
  the implementation card lands; until then the deferral stands in code
  (session cookie only) and this ADR is the contract the implementation
  PR is reviewed against.
- Fuzz/contract tests for the implementation PR cover: header-only
  transport (query-string bearer rejected), unknown/revoked/expired key
  vectors, scope allowlist/deny checks, session-only action denials,
  dual-pepper lookup, dormant-key retirement refusal, redaction of bearer
  material in emitted logs, and rate-limit/anomaly behavior.
- Widening scopes, raising the 365-day cap, or granting bearer keys any
  session-only action needs a new ADR.
