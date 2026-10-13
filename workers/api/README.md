# nntune-api: accounts, auth, sessions, BYOP vault

Rust core for NNTune v2 accounts/auth (passkey challenges + email magic
link), signed-cookie sessions, and the BYOP credential vault. Compiles to
native (tests) and `wasm32` (Cloudflare Worker).

## Layout

```text
workers/api/
  src/lib.rs      # crate root (native + wasm32, no Worker runtime imports)
  src/session.rs  # HMAC-signed cookies: issue / verify / rotate
  src/auth.rs     # magic links, passkey challenge lifecycle, invite codes
  src/vault.rs    # envelope encryption, validation, SSRF guard, redaction
  tests/api_tests.rs  # acceptance tests (TDD)
  wrangler.toml.example  # Worker config template (no secrets)
```

`migrations/0002_auth_vault.sql` is the Postgres schema (PlanetScale via
Hyperdrive). The HTTP + Hyperdrive wiring (Worker entry) lands in the
follow-up slice once staging secrets are provisioned; this crate is the
security-critical logic with tests.

## Security contract (threat-model §3–§4)

- **Envelope encryption:** per-user random 256-bit DEK, wrapped by the KEK
  (Cloudflare Worker secret, versioned). AES-256-GCM, random 96-bit nonces,
  AAD binds `user_id` (+ KEK version for the wrap). Row-swap between users
  fails to decrypt. Rotation via `rewrap_dek` without touching ciphertext.
- **Never logged, never returned:** reads return booleans only
  (`redacted_summary`); `test connection` returns a boolean, never provider
  banners; audit events carry ids/results only — there is no field for
  secret material anywhere in the API or schema.
- **Sessions:** `HttpOnly; Secure; SameSite=Lax`, bounded `Max-Age`,
  rotation bumps a counter; server-side `sessions` rows are the revocation
  source. Secret comes from Worker env, never code.
- **Magic links:** 256-bit random tokens, SHA-256 hash at rest, single-use,
  15-minute expiry. All failures return one generic message (no
  user-enumeration oracle).
- **Passkeys:** RP ID pinned, exact-origin match (https; http only for
  localhost dev), 5-minute single-use challenges, monotonic `sign_count`
  storage shape. Full attestation/assertion signature verification wires to
  a WebAuthn library in the follow-up slice; the lifecycle gates here run
  first and independently.
- **SSRF guard:** provider hosts are hostile input. Shape check (never a
  URL), resolve ALL DNS records, reject loopback/link-local/private/
  unique-local/multicast/reserved (v4+v6); literal IPs checked directly.
  Indexer fetches forbid redirects; dial/total timeouts and per-user
  concurrency caps are constants in `vault.rs`.

## Test

```sh
cargo test --manifest-path workers/api/Cargo.toml
cargo clippy --manifest-path workers/api/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path workers/api/Cargo.toml -- --check
```

Fixtures use synthetic credentials only (`news.example.com`, `203.0.113.7`).
