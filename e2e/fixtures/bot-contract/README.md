# Bot-contract conformance corpus (spec section 5)

JSON vectors + checker for the TWO-bot consumption contract, banked ahead
of the B-chain runtime work so the B8 staging harness consumes them later.
Offline only: no staging, no network, no real credentials.

- Vectors: `grants.json` (20), `signed-url.json` (19), `profile-matrix.json` (32).
- Checker: `e2e/helpers/bot-contract.ts` (`checkGrantVector`,
  `checkSignedUrlVector`, `checkMatrixRow`).
- Executable pins: `e2e/tests/bot-contract.spec.ts` (runs in
  `npm run test:unit` on every PR).
- B8 consumes from here: feed the URL vectors to staging, run the checker
  over the fixtures, and compare wire verdicts against the matrix
  expectations. Corpus path and checker import are stable; vector ids
  (`botgrant-NNN`, `boturl-NNN`, `botmx-NNN`) are stable and citable.

## The banked contract

Section 5 of the e2e spec defines the bot flow (link bearer, catalog,
playback request, byte fetch, metering). This corpus banks the byte shapes
and verdict mapping so implementations converge before the runtime exists:

1. **Signed stream URL.** `https://<stream-host>/play/<track_id>?grant=<v1 token>[&profile=opus-64]`.
   - `https` only; a distinct `/play/` path so the stream Worker's surface
     cannot be confused with the api's `/v1/stream/` route.
   - Exactly one `grant` param, at most one `profile` param; unknown params
     are ignored (forward-compatible).
   - Absent `profile` selects the verified **source** bytes; `opus-64`
     selects the transcode artifact; any other value (including empty) is an
     unknown profile. The two-profile set is the whole matrix: section 4.1
     allows one profile and no negotiation.
2. **Grant envelope.** `v1.<b64url(user|track|exp)>.<b64url(hmac-sha256)>`
   exactly as `workers/stream/src/grant.rs` verifies it (shape, then
   signature, then payload, then expiry, then the 900 s cap + 60 s skew).
   Every grant rejection is the generic 401 `unauthorized` on the wire;
   checker reasons (`expired`, `bad-signature`, …) are diagnostics only.
3. **Verdict precedence** for one byte-fetch attempt, mirroring the server
   order (bearer gate, scope map, membership, grant verify, track check,
   range resolution, provider fetch):
   bearer -> scope (`stream:read` required) -> membership -> grant ->
   track binding -> profile binding -> profile value -> artifact presence ->
   range -> provider auth -> serve (200 full / 206 partial).
4. **Typed codes** come from `docs/api-errors.md`, never invented here:
   `unauthorized`, `provider_auth_expired`, `forbidden`, `not_found`,
   `invalid_request`, `range_not_satisfiable`, each with its registry
   status and retryable flag.

## Known forward edges (not gaps in this corpus)

- **Grant v2.** Section 5 recommends binding `(play_id, profile)` on a v2
  wire, but the exact bytes are B1's decision. This corpus pins v1 bytes
  and expresses the profile-replay rule (`botmx-012`, `botmx-013`) with
  declared bindings; v1 grants (`grant_profile: null`) are compatible with
  either profile (`botmx-009`). When B1 lands, extend `grants.json` with v2
  vectors and add the version arm to the checker.
- **Absent artifact** (`botmx-011`) reads as 404 `not_found`. The
  conditional on-demand transcode fallback may later turn this
  wait-then-serve instead; that change updates this row, not the checker.
- **Bearer/scope issuance** (linking, catalog, revocation UX) stays pinned
  where it lives: the Rust route-map tests and the offline half of
  `e2e/tests/scoped-credentials.spec.ts`. This corpus takes bearer state
  and scopes as matrix inputs.
- **Subsonic compat** (the `/70` half of the spec's 404/70) rides the
  Subsonic card; this corpus banks the native REST verdicts only.

## Stable IDs

| ID | Value | Pinned against |
|---|---|---|
| profile | `opus-64` | `PROFILE_NAME` in `crates/nntune-core/src/transcode.rs` |
| artifact format / bitrate / rate / channels | `opus` / 64 / 48000 / 2 | same file |
| artifact key | `audio/{hh}/{hh}/{sha256hex}` | `artifact_r2_key`, same file |
| grant TTL cap / skew | 900 s / 60 s | `workers/stream/src/grant.rs` |
| stream scope | `stream:read` | `SCOPE_STREAM_READ` in `workers/api/src/native_rest.rs` |
| error codes | registry subset | `docs/api-errors.md` envelope table |

The spec asserts each row against the live source, so a const drift reds
`test:unit` instead of rotting the corpus.

## Negative control

The corpus passing is the positive control; the mutation batteries in
`e2e/tests/bot-contract.spec.ts` are the negative control. Each battery
clones a happy vector, breaks one shape, and requires the checker to fail
with the exact reason:

- grants: flipped payload/signature chars, now-at/now-past expiry,
  lifetime one past cap+skew, dropped version, extra segment, truncation,
  tampered expectations (11 mutants);
- signed URLs: http downgrade, dropped/duplicated grant, duplicated or
  unknown profile, swapped track, api-path retarget, nested path, flipped
  grant char, now-at-expiry, tampered expectation (12 mutants), plus a
  positive mutant proving unknown query params still pass;
- matrix: fault injection on happy rows, artifact const/key breaks,
  fault clearing on negative rows, expectation edits, structural breaks
  (31 mutants).

Cross-language proof (run locally, not in CI): every corpus token was
additionally verified with the real `grant::verify` compiled standalone
via `rustc` (no cargo), and the TS checker agrees with it 28/28 (20 grant
vectors plus 8 decisive URL-embedded grants); the crypto primitives
reproduce the RFC 4231 and `sha256("abc")` vectors both implementations pin.

The expiry boundary vectors (`botgrant-015` through `botgrant-020`) cover
2^53 - 1, 2^53, 2^53 + 1, leading-plus 2^53 + 1, u64 maximum, and u64
overflow. Parsing and TTL arithmetic use `BigInt` before narrowing to the
corpus's JSON-safe successful outputs: in-range huge expiries reject as
`ttl-too-long`, while u64 overflow remains `malformed`. The existing
corpus test failed before the parser fix (four diagnostic mismatches)
and passes after it; no spec assertion was changed.

## Minting new vectors

Tokens use the `corpus` fixture secret
(`bot-contract-corpus-secret-please-ignore`: synthetic, offline-only, and
never a real credential). Fixtures name the secret; values never appear in
fixtures or diagnostics. Mint with the documented composition:

```python
import base64, hashlib, hmac
secret = b"bot-contract-corpus-secret-please-ignore"
payload = base64.urlsafe_b64encode(b"user|track|1700000000").decode().rstrip("=")
sig = hmac.new(secret, payload.encode(), hashlib.sha256).digest()
token = f"v1.{payload}.{base64.urlsafe_b64encode(sig).decode().rstrip('=')}"
```

New vectors need stable ids, an expectation, and at least one mutant in
the spec battery that breaks the new shape. The high-entropy `v1.…`
segments trip the `generic-api-key` rule, so `.gitleaks.toml` allowlists
exactly that envelope shape, conjunctively (`condition = "AND"`: this
directory plus the anchored `v1` shape): non-`v1`, other-rule, and
outside-directory findings still trip. Never commit real secrets here:
anything `v1`-shaped in this directory is invisible to the secret gate
by design.
