# ADR 0008: Accounts, Auth, Sessions, BYOP Vault

- **Status:** Accepted (implementation slice 1; CISO review gates the PR before merge)
- **Date:** 2026-10-13

## Context

Accounts need signup/login with passkeys (WebAuthn) plus an email magic-link
fallback, sessions on signed cookies with rotation, and optional invite
codes. BYOP credentials (Usenet provider host/port/TLS/user/pass/max
connections, optional Newznab indexer URL + API key) need envelope
encryption with a per-user DEK and a KEK from the Worker secret — never
logged, never returned after save — plus a `test connection` endpoint via
the stream pool. Relational data lives in PlanetScale Postgres via
Hyperdrive. The STRIDE threat model (§3–§4) is the binding security spec.

## Decision

1. **Slice 1 (this change):** pure-Rust core (`workers/api`: `session`,
   `auth`, `vault`) + Postgres schema (`migrations/0002_auth_vault.sql`),
   native-tested, wasm32-compatible. No Worker runtime imports yet.
2. **Vault:** AES-256-GCM envelope (per-user DEK; KEK versioned, staging and
   prod never shared); AAD binds `user_id` (+ KEK version on the wrap);
   reads return booleans only; `test connection` returns a boolean only;
   audit carries ids/results, never secrets; SSRF guard resolves ALL DNS
   records and rejects non-public ranges (v4+v6); timeouts and per-user caps
   are code constants.
3. **Sessions:** HMAC-SHA256 cookies (`HttpOnly; Secure; SameSite=Lax`,
   bounded `Max-Age`), rotation counter, server-side rows for revocation.
4. **Magic links:** 256-bit random, SHA-256 at rest, single-use, 15-minute
   expiry, one generic failure message.
5. **Passkeys:** challenge lifecycle (RP pin, exact origin, 5-minute
   single-use) + credential storage shapes now; attestation/assertion
   signature verification + HTTP/Hyperdrive Worker entry in the follow-up
   slice (child card) once staging secrets are provisioned.

## Consequences

- Slice 1 is verifiable today (`cargo test`, clippy, fmt) with zero secret
  handling in CI (synthetic fixtures only).
- The follow-up slice adds the Worker HTTP entry, Hyperdrive queries
  (parameterized only), WebAuthn signature verification, and the pool-backed
  `test connection`; it re-requests CISO review as a vault/auth-touching PR.
