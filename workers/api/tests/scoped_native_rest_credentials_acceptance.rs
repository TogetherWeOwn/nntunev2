//! Acceptance tests: scoped native REST credentials (paired acceptance card).
//!
//! TDD RED: these tests FAIL (compile error, E0432) until the paired build
//! card implements the contract below in
//! `workers/api/src/native_rest.rs` and exports it from `lib.rs`
//! (`pub mod native_rest;`). The builder must NEVER edit this file.
//!
//! Failing command (this card):
//!   `cargo test --manifest-path workers/api/Cargo.toml --test scoped_native_rest_credentials_acceptance`
//!
//! Scheme source: ADR 0010 (`docs/adr/0010-native-rest-credentials.md`,
//! merged via PR #114), which resolves the ADR 0006 deferral. Scope names,
//! the route map, the `ntk_` display form, HMAC-SHA256 storage, the 365-day
//! cap, and the session-only action set all come from that ADR — this file
//! pins them, it does not redefine them.
//!
//! Contract under test (implement exactly these items in `native_rest.rs`;
//! pure Rust, native + wasm32, NO Worker runtime imports — same constraint
//! as `auth`/`session`/`vault`/`rate_limit`):
//!
//! - Scope vocabulary: `pub const SCOPE_LIBRARY_READ: &str = "library:read";`
//!   `SCOPE_PLAYLISTS_WRITE = "playlists:write"`, `SCOPE_PLAYS_WRITE =
//!   "plays:write"`, `SCOPE_STREAM_READ = "stream:read"`, `SCOPE_INGEST_SUBMIT
//!   = "ingest:submit"`, plus `pub const ALL_SCOPES: [&str; 5]` in that
//!   order. `pub enum Scope { LibraryRead, PlaylistsWrite, PlaysWrite,
//!   StreamRead, IngestSubmit }` (`Debug + Clone + Copy + PartialEq + Eq +
//!   Hash`) with `as_str() -> &'static str` and `parse(&str) ->
//!   Option<Scope>`: exact, case-sensitive, no trimming — `"subsonic"`,
//!   `""`, `"Library:Read"` and `"library:read "` are all `None`.
//! - Credential shape: `pub const CREDENTIAL_PREFIX: &str = "ntk_";`
//!   `pub const KEY_PREFIX_LEN: usize = 8;` `pub const
//!   MAX_CREDENTIAL_TTL_SECS: u64 = 31_536_000;` (365 days).
//!   `pub struct NativeRestRecord { pub id: String, pub user_id: String,
//!   pub key_prefix: String, pub key_hmac: String, pub pepper_version: u32,
//!   pub scopes: Vec<Scope>, pub created_at: u64, pub expires_at: u64,
//!   pub revoked_at: Option<u64> }` (`Clone`; MANUAL `Debug` that prints
//!   id, user, prefix, scopes and expiry/revocation state but NEVER the
//!   display secret or `key_hmac`). `pub struct IssuedCredential {
//!   pub display_once: String, pub record: NativeRestRecord }` (manual
//!   redacted `Debug` likewise — the plaintext lives here until the HTTP
//!   layer shows it once and drops it).
//! - `pub struct Pepper<'a> { pub version: u32, pub bytes: &'a [u8] }`
//!   (manual redacted `Debug`: version only, never the bytes) and
//!   `pub fn issue_credential(user_id: &str, scopes: &[Scope],
//!   ttl_secs: u64, now: u64, pepper: &Pepper<'_>) -> IssuedCredential`:
//!   server-generated 256-bit CSPRNG display secret (`ntk_` + base62,
//!   unique per issue), `record.key_hmac ==
//!   credential_hmac(&display_once, pepper.bytes)`, `record.key_prefix ==
//!   display_once[..CREDENTIAL_PREFIX.len() + KEY_PREFIX_LEN]`,
//!   `record.created_at == now`, `record.expires_at ==
//!   now.saturating_add(ttl_secs.min(MAX_CREDENTIAL_TTL_SECS))` (fail-safe
//!   clamp, mirroring the session-cookie Max-Age precedent —
//!   `u64::MAX` TTLs can never mint a forever key), requested scopes
//!   preserved exactly (including the empty set, which authorizes
//!   nothing), ids unique per issue, `revoked_at == None`.
//! - `pub fn credential_hmac(secret: &str, pepper: &[u8]) -> String`:
//!   lowercase-hex HMAC-SHA256 over the display secret, deterministic per
//!   (secret, pepper), pepper-bound (wrong pepper mismatches).
//! - Errors reuse the repo taxonomy (`docs/api-errors.md`), no new codes:
//!   `pub enum CredentialError { Unauthenticated, Forbidden }` (`Debug +
//!   Clone + Copy + PartialEq + Eq`) with `code()` returning
//!   `"unauthorized"` / `"forbidden"`, `http_status()` returning 401 /
//!   403, and generic `Display` (`"invalid credential"` / `"insufficient
//!   scope"` — never which check failed, never any secret material).
//! - `pub fn authorize(record: &NativeRestRecord, presented_secret: &str,
//!   required: Scope, now: u64, caller_user_id: &str, peppers:
//!   &[Pepper<'_>]) -> Result<(), CredentialError>`: every credential-
//!   validity failure maps to `Unauthenticated` (unknown/tampered secret,
//!   unknown pepper version, owner mismatch, revoked, expired — no
//!   oracle distinguishes them); a valid credential lacking `required`
//!   maps to `Forbidden`. Expiry boundary mirrors the magic-link
//!   precedent: `now > expires_at` is expired, `now == expires_at` still
//!   passes. Owner mismatch is `Unauthenticated`, never `Forbidden` (a
//!   non-owner probe must not learn the key was otherwise valid).
//!   Dual-pepper transition: a record whose version matches ANY entry in
//!   `peppers` authorizes; an empty slice or an unlisted version fails
//!   closed. NOTE: this signature takes no cookie/session input by
//!   design — the HTTP layer must not consult cookies on bearer routes
//!   (ADR: no session-cookie fallback on credential routes).
//! - `pub fn revoke_credential(record: &mut NativeRestRecord, now: u64)`
//!   (sets `revoked_at = Some(now)`; idempotent — never un-revokes) and
//!   `pub fn revoke_all_for_user(records: &mut [NativeRestRecord],
//!   user_id: &str) -> usize` (the recovery/revoke-all path: revokes
//!   every unrevoked row for the user, returns how many transitioned;
//!   empty user id revokes nothing — fail closed, mirroring the
//!   logout-everywhere precedent).
//! - `pub fn extract_bearer_key(authorization: Option<&str>) ->
//!   Result<&str, CredentialError>`: header-only transport. Accepts
//!   `Bearer <opaque-token>` (scheme case-insensitive per RFC 7235,
//!   exactly one space, non-empty token with no whitespace inside) and
//!   returns the token slice; everything else (`None`, `""`, `"Bearer"`,
//!   `"Bearer "`, `"Basic ..."`, multi-space, inner whitespace) is
//!   `Unauthenticated`. There is intentionally NO query-string parser:
//!   the HTTP layer must never read credentials from the query (threat
//!   model: query strings land in logs), pinned live by the e2e leg.
//! - `pub enum RouteDecision { Bearer(Scope), SessionOnly, Deny }`
//!   (`Debug + Clone + Copy + PartialEq + Eq`) and
//!   `pub fn scope_for_operation(method: &str, path: &str) ->
//!   RouteDecision`: the exhaustive default-deny map. Methods match exact
//!   uppercase (`"get"` is `Deny` — fail closed); paths match the listed
//!   collection roots plus subpaths (the HTTP layer passes the path
//!   without the query string). Reads on the six catalog roots need
//!   `library:read`; playlist writes need `playlists:write`; `/v1/plays`
//!   writes need `plays:write`; `/v1/stream` reads need `stream:read`;
//!   `/v1/ingest` submission needs `ingest:submit`. Session-only (never
//!   reachable by bearer, enforced by the auth layer): `/v1/me`,
//!   `/v1/indexers`, `/v1/operations` reads, library membership writes
//!   and deletes, every `/v1/vault*` operation including test-connection,
//!   credential issuance/revocation (`/v1/credentials*`), auth/session/
//!   account/billing/step-up paths. Everything else — `/rest/*` in both
//!   directions, unlisted methods on listed routes, unknown paths — is
//!   `Deny`. An endpoint added after ADR 0010 stays `Deny` until the map
//!   is explicitly updated.
//!
//! Issuance HTTP surface the e2e leg pins (builder implements; session-
//! authenticated, never bearer): `POST /v1/credentials`
//! `{label, scopes, ttl_secs}` → `201 {id, secret, key_prefix, scopes,
//! expires_at}` (`secret` shown once, never stored or returned again);
//! `GET /v1/credentials` → `200 {credentials: [...]}` with no `secret`
//! key anywhere; `DELETE /v1/credentials/{id}` → `200 {ok: true}`.
//!
//! Fixtures are synthetic only (public repo: never real secrets, peppers,
//! or credential material). Diagnostics name names, never values.

use nntune_api::native_rest::{
    authorize, credential_hmac, extract_bearer_key, issue_credential, revoke_all_for_user,
    revoke_credential, scope_for_operation, CredentialError, IssuedCredential, Pepper,
    RouteDecision, Scope, ALL_SCOPES, CREDENTIAL_PREFIX, KEY_PREFIX_LEN, MAX_CREDENTIAL_TTL_SECS,
    SCOPE_INGEST_SUBMIT, SCOPE_LIBRARY_READ, SCOPE_PLAYLISTS_WRITE, SCOPE_PLAYS_WRITE,
    SCOPE_STREAM_READ,
};
use nntune_api::session;

const NOW: u64 = 1_700_000_000;
const USER: &str = "user-acceptance";
const OTHER_USER: &str = "user-other";
// Synthetic peppers only. Production peppers live in the Worker-secret
// keyring and never appear in code, tests, or logs.
const PEPPER_V1: &[u8] = b"synthetic-pepper-v1-32-bytes!!!!!!";
const PEPPER_V2: &[u8] = b"synthetic-pepper-v2-32-bytes!!!!!!";

fn pepper_v1() -> Pepper<'static> {
    Pepper {
        version: 1,
        bytes: PEPPER_V1,
    }
}

fn peppers_current_v1() -> [Pepper<'static>; 1] {
    [pepper_v1()]
}

fn issued(scopes: &[Scope], ttl: u64, now: u64) -> IssuedCredential {
    issue_credential(USER, scopes, ttl, now, &pepper_v1())
}

fn ok_authorize(record: &nntune_api::native_rest::NativeRestRecord, secret: &str, scope: Scope) {
    authorize(record, secret, scope, NOW, USER, &peppers_current_v1())
        .expect("fresh in-scope credential must authorize");
}

// --- 1. Scope vocabulary -----------------------------------------------------

#[test]
fn scope_vocabulary_parses_and_round_trips() {
    assert_eq!(
        ALL_SCOPES,
        [
            "library:read",
            "playlists:write",
            "plays:write",
            "stream:read",
            "ingest:submit"
        ],
        "ADR 0010 vocabulary, in order"
    );
    assert_eq!(SCOPE_LIBRARY_READ, "library:read");
    assert_eq!(SCOPE_PLAYLISTS_WRITE, "playlists:write");
    assert_eq!(SCOPE_PLAYS_WRITE, "plays:write");
    assert_eq!(SCOPE_STREAM_READ, "stream:read");
    assert_eq!(SCOPE_INGEST_SUBMIT, "ingest:submit");

    for (text, scope) in [
        ("library:read", Scope::LibraryRead),
        ("playlists:write", Scope::PlaylistsWrite),
        ("plays:write", Scope::PlaysWrite),
        ("stream:read", Scope::StreamRead),
        ("ingest:submit", Scope::IngestSubmit),
    ] {
        assert_eq!(Scope::parse(text), Some(scope), "must parse {text}");
        assert_eq!(scope.as_str(), text, "must render back to {text}");
    }

    // Exact match only: cross-surface, empty, case and whitespace variants
    // are not scopes (a scope allowlist that trims or folds admits forged
    // scope strings past the check).
    for bad in [
        "",
        "subsonic",
        "Library:Read",
        "LIBRARY:READ",
        "library:read ",
        " library:read",
        "library:read\n",
        "library:write",
        "admin",
        "*",
    ] {
        assert_eq!(Scope::parse(bad), None, "{bad:?} must not parse as a scope");
    }
}

// --- 2. Credential shape: one-time display, HMAC-only rest -------------------

#[test]
fn credential_display_shape_and_storage() {
    let first = issued(&[Scope::LibraryRead], 3600, NOW);
    let second = issued(&[Scope::LibraryRead], 3600, NOW);

    // Display form: the `ntk_` class prefix plus server-generated entropy.
    assert!(
        first.display_once.starts_with(CREDENTIAL_PREFIX),
        "display must carry the ntk_ class prefix"
    );
    assert_ne!(
        first.display_once, second.display_once,
        "display secrets must be unique per issue (CSPRNG)"
    );
    assert_ne!(first.record.id, second.record.id, "ids unique per issue");
    assert!(!first.record.id.is_empty(), "id is never empty");

    // The stored row keeps a short non-secret prefix for listing, and the
    // HMAC — never the secret itself.
    assert_eq!(
        first.record.key_prefix,
        first.display_once[..CREDENTIAL_PREFIX.len() + KEY_PREFIX_LEN].to_string(),
        "prefix is the leading slice of the display form"
    );
    assert_eq!(
        first.record.key_hmac,
        credential_hmac(&first.display_once, PEPPER_V1),
        "stored HMAC must verify against the display secret"
    );
    assert_ne!(
        first.record.key_hmac, first.display_once,
        "the secret itself is never stored"
    );

    // Row metadata is preserved for owner checks, listing, and audit.
    assert_eq!(first.record.user_id, USER);
    assert_eq!(first.record.scopes, vec![Scope::LibraryRead]);
    assert_eq!(first.record.pepper_version, 1);
    assert_eq!(first.record.created_at, NOW);
    assert_eq!(first.record.expires_at, NOW + 3600);
    assert_eq!(first.record.revoked_at, None);
}

#[test]
fn credential_hmac_is_deterministic_and_pepper_bound() {
    let secret = "ntk_synthetic-display-secret-for-hmac-pin";
    let mac_a = credential_hmac(secret, PEPPER_V1);
    assert_eq!(mac_a.len(), 64, "hex HMAC-SHA256 is 64 chars");
    assert_eq!(
        credential_hmac(secret, PEPPER_V1),
        mac_a,
        "same (secret, pepper) verifies deterministically"
    );
    assert_ne!(
        credential_hmac(secret, PEPPER_V2),
        mac_a,
        "wrong pepper must mismatch (rotation safety)"
    );
    assert_ne!(
        credential_hmac("ntk_different-secret", PEPPER_V1),
        mac_a,
        "wrong secret must mismatch"
    );
}

// --- 3. TTL cap ---------------------------------------------------------------

#[test]
fn ttl_never_exceeds_365_day_cap() {
    assert_eq!(
        MAX_CREDENTIAL_TTL_SECS, 31_536_000,
        "ADR 0010: 365-day maximum, no never-expiring keys"
    );
    // Over-max requests clamp fail-safe (session-cookie Max-Age precedent),
    // so even `u64::MAX` can never mint a forever key.
    for ttl in [MAX_CREDENTIAL_TTL_SECS + 1, u64::MAX] {
        let cred = issued(&[Scope::LibraryRead], ttl, NOW);
        assert_eq!(
            cred.record.expires_at,
            NOW.saturating_add(MAX_CREDENTIAL_TTL_SECS),
            "ttl={ttl} must clamp to the 365-day cap"
        );
    }
    // In-cap TTLs pass through exactly; zero TTL expires at issuance.
    let cred = issued(&[Scope::LibraryRead], 60, NOW);
    assert_eq!(cred.record.expires_at, NOW + 60);
    let zero = issued(&[Scope::LibraryRead], 0, NOW);
    assert_eq!(zero.record.expires_at, NOW);
}

// --- 4. Header-only transport --------------------------------------------------

#[test]
fn bearer_header_extraction() {
    // Canonical form returns the opaque token slice.
    assert_eq!(
        extract_bearer_key(Some("Bearer abc123")),
        Ok("abc123"),
        "canonical Bearer header must yield the token"
    );
    // Scheme is case-insensitive (RFC 7235) but the shape is strict.
    assert_eq!(
        extract_bearer_key(Some("bearer abc123")),
        Ok("abc123"),
        "lowercase scheme must be accepted"
    );

    // Everything else fails closed with the generic code — a malformed
    // header never becomes an authenticated caller.
    for bad in [
        None,
        Some(""),
        Some("Bearer"),
        Some("Bearer "),
        Some("Bearer  abc"),
        Some("Bearer a bc"),
        Some("Bearer abc "),
        Some("Basic abc123"),
        Some("Token abc123"),
        Some("abc123"),
    ] {
        assert_eq!(
            extract_bearer_key(bad),
            Err(CredentialError::Unauthenticated),
            "header {bad:?} must be rejected as unauthenticated"
        );
    }
}

// --- 5. Lifecycle: mint → 200-path → revoke → 401 ------------------------------

#[test]
fn lifecycle_mint_use_revoke_deny() {
    let cred = issued(&[Scope::LibraryRead], 3600, NOW);

    // Minted credential authorizes its scoped route (the "200" leg).
    ok_authorize(&cred.record, &cred.display_once, Scope::LibraryRead);

    // Revocation is immediate and recorded.
    let mut record = cred.record.clone();
    revoke_credential(&mut record, NOW + 10);
    assert_eq!(record.revoked_at, Some(NOW + 10));
    assert_eq!(
        authorize(
            &record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW + 11,
            USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Unauthenticated),
        "revoked credential must fail closed"
    );

    // Idempotent: a second revoke never un-revokes.
    revoke_credential(&mut record, NOW + 20);
    assert_eq!(record.revoked_at, Some(NOW + 10), "revoke is sticky");
    assert_eq!(
        authorize(
            &record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW + 21,
            USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Unauthenticated),
        "double-revoked credential stays denied"
    );
}

#[test]
fn unknown_expired_and_foreign_credentials_fail_closed() {
    let cred = issued(&[Scope::LibraryRead], 3600, NOW);

    // Unknown secret: no existence oracle, just the generic 401.
    assert_eq!(
        authorize(
            &cred.record,
            "ntk_no-such-credential-000000000000000000000",
            Scope::LibraryRead,
            NOW,
            USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Unauthenticated),
        "unknown secret must fail closed"
    );

    // Tampered row (e.g. swapped HMAC) does not authenticate either.
    let mut tampered = cred.record.clone();
    tampered.key_hmac = "0".repeat(64);
    assert_eq!(
        authorize(
            &tampered,
            &cred.display_once,
            Scope::LibraryRead,
            NOW,
            USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Unauthenticated),
        "tampered HMAC must fail closed"
    );

    // Expiry boundary mirrors the magic-link precedent: `now > expires_at`
    // is expired, `now == expires_at` still passes.
    assert!(
        authorize(
            &cred.record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW + 3600,
            USER,
            &peppers_current_v1()
        )
        .is_ok(),
        "credential valid at exactly expires_at"
    );
    assert_eq!(
        authorize(
            &cred.record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW + 3601,
            USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Unauthenticated),
        "expired credential must fail closed"
    );

    // Owner mismatch is Unauthenticated, never Forbidden: a non-owner probe
    // must not learn the key was otherwise valid (no oracle).
    assert_eq!(
        authorize(
            &cred.record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW,
            OTHER_USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Unauthenticated),
        "foreign owner must fail closed without revealing validity"
    );

    // Unknown pepper version fails closed (retired without migration).
    assert_eq!(
        authorize(
            &cred.record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW,
            USER,
            &[Pepper {
                version: 9,
                bytes: b"synthetic-pepper-v9",
            }]
        ),
        Err(CredentialError::Unauthenticated),
        "unlisted pepper version must fail closed"
    );
    assert_eq!(
        authorize(
            &cred.record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW,
            USER,
            &[]
        ),
        Err(CredentialError::Unauthenticated),
        "empty pepper set must fail closed"
    );

    // Dual-pepper transition: a record on the previous version still
    // authorizes while both versions are listed (ADR 0010 § storage).
    let old_pepper = Pepper {
        version: 1,
        bytes: PEPPER_V1,
    };
    let both = [
        Pepper {
            version: 2,
            bytes: PEPPER_V2,
        },
        old_pepper,
    ];
    assert!(
        authorize(
            &cred.record,
            &cred.display_once,
            Scope::LibraryRead,
            NOW,
            USER,
            &both
        )
        .is_ok(),
        "previous-version credential authorizes during transition"
    );
}

#[test]
fn recovery_revoke_all() {
    // Two rows for USER (one pre-revoked), one for another user.
    let mut mine_a = issued(&[Scope::LibraryRead], 3600, NOW).record;
    let mut mine_b = issued(&[Scope::StreamRead], 3600, NOW).record;
    let mut theirs = issued(&[Scope::LibraryRead], 3600, NOW).record;
    mine_a.user_id = USER.to_string();
    mine_b.user_id = USER.to_string();
    theirs.user_id = OTHER_USER.to_string();
    revoke_credential(&mut mine_b, NOW + 5);
    let mut rows = vec![mine_a, mine_b, theirs];

    // The recovery/revoke-all event (email change, passkey add/remove,
    // session-compromise response) revokes exactly this user's live rows.
    let revoked = revoke_all_for_user(&mut rows, USER);
    assert_eq!(revoked, 1, "only the unrevoked user row transitions");
    assert!(rows[0].revoked_at.is_some(), "user row revoked");
    assert_eq!(
        rows[1].revoked_at,
        Some(NOW + 5),
        "already-revoked row untouched"
    );
    assert_eq!(rows[2].revoked_at, None, "another user's row untouched");

    // A second sweep revokes nothing new (idempotent bulk path).
    assert_eq!(revoke_all_for_user(&mut rows, USER), 0);

    // Fail closed: an empty user id (upstream bug) must never become a
    // mass revocation.
    assert_eq!(revoke_all_for_user(&mut rows, ""), 0);
    assert_eq!(rows[2].revoked_at, None);
}

// --- 6. Scope matrix: scope A cannot reach scope B ------------------------------

/// Canonical (method, path) per scope, all `Bearer` per the route map.
fn canonical_route(scope: Scope) -> (&'static str, &'static str) {
    match scope {
        Scope::LibraryRead => ("GET", "/v1/search"),
        Scope::PlaylistsWrite => ("POST", "/v1/playlists"),
        Scope::PlaysWrite => ("POST", "/v1/plays"),
        Scope::StreamRead => ("GET", "/v1/stream"),
        Scope::IngestSubmit => ("POST", "/v1/ingest"),
    }
}

#[test]
fn scope_matrix_scope_a_cannot_reach_scope_b() {
    let scopes = [
        Scope::LibraryRead,
        Scope::PlaylistsWrite,
        Scope::PlaysWrite,
        Scope::StreamRead,
        Scope::IngestSubmit,
    ];
    for held in scopes {
        let cred = issued(&[held], 3600, NOW);
        for target in scopes {
            let (method, path) = canonical_route(target);
            // The map itself must agree the canonical route needs `target`.
            assert_eq!(
                scope_for_operation(method, path),
                RouteDecision::Bearer(target),
                "canonical route for {} must require itself",
                target.as_str()
            );
            let got = authorize(
                &cred.record,
                &cred.display_once,
                target,
                NOW,
                USER,
                &peppers_current_v1(),
            );
            if held == target {
                assert_eq!(got, Ok(()), "own scope {held:?} must authorize");
            } else {
                // Fails BY NAME: authenticated but the wrong scope — 403
                // `forbidden`, never the 401 `unauthorized` (which would
                // claim the credential itself was bad).
                assert_eq!(
                    got,
                    Err(CredentialError::Forbidden),
                    "scope {} on a {} route must be forbidden by name",
                    held.as_str(),
                    target.as_str()
                );
                assert_eq!(CredentialError::Forbidden.code(), "forbidden");
                assert_eq!(CredentialError::Forbidden.http_status(), 403);
            }
        }
    }

    // Multi-scope credentials authorize each held scope and nothing else.
    let multi = issued(&[Scope::LibraryRead, Scope::StreamRead], 3600, NOW);
    for scope in [Scope::LibraryRead, Scope::StreamRead] {
        ok_authorize(&multi.record, &multi.display_once, scope);
    }
    assert_eq!(
        authorize(
            &multi.record,
            &multi.display_once,
            Scope::IngestSubmit,
            NOW,
            USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Forbidden),
        "unheld scope on a multi-scope credential is forbidden"
    );

    // The empty set authorizes nothing (least privilege floor).
    let bare = issued(&[], 3600, NOW);
    for scope in scopes {
        assert_eq!(
            authorize(
                &bare.record,
                &bare.display_once,
                scope,
                NOW,
                USER,
                &peppers_current_v1()
            ),
            Err(CredentialError::Forbidden),
            "scopeless credential must be forbidden everywhere"
        );
    }
}

// --- 7. Route map: exhaustive default-deny -------------------------------------

#[test]
fn route_map_is_exhaustive_and_default_deny() {
    use RouteDecision::{Bearer, Deny, SessionOnly};
    let cases: &[(&str, &str, RouteDecision)] = &[
        // Catalog reads → library:read (roots and subpaths, GET and HEAD).
        ("GET", "/v1/library", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/artists", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/releases", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/releases/rel_123", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/tracks", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/search", Bearer(Scope::LibraryRead)),
        ("HEAD", "/v1/search", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/playlists", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/playlists/pl_1", Bearer(Scope::LibraryRead)),
        ("GET", "/v1/library", Bearer(Scope::LibraryRead)),
        // Playlist writes → playlists:write.
        ("POST", "/v1/playlists", Bearer(Scope::PlaylistsWrite)),
        ("PUT", "/v1/playlists/pl_1", Bearer(Scope::PlaylistsWrite)),
        ("PATCH", "/v1/playlists/pl_1", Bearer(Scope::PlaylistsWrite)),
        (
            "DELETE",
            "/v1/playlists/pl_1",
            Bearer(Scope::PlaylistsWrite),
        ),
        // Play events → plays:write; playback → stream:read; ingest → submit.
        ("POST", "/v1/plays", Bearer(Scope::PlaysWrite)),
        ("GET", "/v1/stream", Bearer(Scope::StreamRead)),
        ("GET", "/v1/stream/tr_1", Bearer(Scope::StreamRead)),
        ("POST", "/v1/ingest", Bearer(Scope::IngestSubmit)),
        ("POST", "/v1/ingest/jobs", Bearer(Scope::IngestSubmit)),
        // Session-only: never reachable by bearer (auth layer, not scopes).
        ("GET", "/v1/me", SessionOnly),
        ("PUT", "/v1/me", SessionOnly),
        ("GET", "/v1/indexers", SessionOnly),
        ("POST", "/v1/indexers", SessionOnly),
        ("GET", "/v1/operations/op_1", SessionOnly),
        ("POST", "/v1/libraries/lib_1/members", SessionOnly),
        ("DELETE", "/v1/libraries/lib_1", SessionOnly),
        ("GET", "/v1/vault", SessionOnly),
        ("PUT", "/v1/vault", SessionOnly),
        ("POST", "/v1/vault/test", SessionOnly),
        ("POST", "/v1/vault/test-connection", SessionOnly),
        ("POST", "/v1/credentials", SessionOnly),
        ("GET", "/v1/credentials", SessionOnly),
        ("DELETE", "/v1/credentials/cred_1", SessionOnly),
        ("POST", "/v1/auth/passkey/login/start", SessionOnly),
        ("POST", "/v1/session/refresh", SessionOnly),
        ("POST", "/v1/session/revoke-all", SessionOnly),
        ("DELETE", "/v1/account", SessionOnly),
        ("POST", "/v1/billing/checkout", SessionOnly),
        // Default-deny: cross-surface use, unlisted methods, unknown paths.
        ("GET", "/rest/getArtists.view", Deny),
        ("POST", "/rest/ping.view", Deny),
        ("POST", "/v1/operations/op_1", Deny),
        ("GET", "/v1/libraries/lib_1", Deny),
        ("POST", "/v1/search", Deny),
        ("DELETE", "/v1/tracks", Deny),
        ("PUT", "/v1/stream", Deny),
        ("DELETE", "/v1/plays", Deny),
        ("GET", "/v1/ingest", Deny),
        ("DELETE", "/v1/playlists", Deny),
        ("GET", "/v1/nope", Deny),
        ("GET", "/", Deny),
        ("", "", Deny),
        // Methods match exact uppercase: lowercase fails closed.
        ("get", "/v1/search", Deny),
        ("post", "/v1/plays", Deny),
    ];
    for (method, path, want) in cases {
        assert_eq!(
            scope_for_operation(method, path),
            *want,
            "{method} {path} must resolve to {want:?}"
        );
    }
}

// --- 8. No session-cookie fallback -----------------------------------------------

#[test]
fn session_material_never_authenticates_as_bearer() {
    let cred = issued(&[Scope::LibraryRead], 3600, NOW);

    // A REAL session token for the same user, minted by the session module,
    // is an unknown bearer key — the two credential spaces never intersect.
    // (And `authorize` takes no cookie/session input at all, so the HTTP
    // layer cannot fall back to one on credential routes even by accident.)
    let secret = b"synthetic-session-secret-at-least-32!".to_vec();
    let session_token = session::issue_session(&secret, USER, NOW, 3600).expect("session issues");
    assert_eq!(
        authorize(
            &cred.record,
            &session_token,
            Scope::LibraryRead,
            NOW,
            USER,
            &peppers_current_v1()
        ),
        Err(CredentialError::Unauthenticated),
        "a valid session token must not authenticate as a bearer credential"
    );

    // A raw Cookie header value presented as a bearer key likewise fails.
    assert_eq!(
        extract_bearer_key(Some("__Host-nntune-session=abc123")),
        Err(CredentialError::Unauthenticated),
        "cookie material is not a Bearer token"
    );
}

// --- 9. Names-only diagnostics ----------------------------------------------------

#[test]
fn diagnostics_name_names_never_secrets() {
    let cred = issued(&[Scope::LibraryRead], 3600, NOW);
    let secret = cred.display_once.clone();
    // The suffix without the prefix is the sensitive part (the prefix alone
    // is the public class marker); the HMAC is lookup material. None of
    // them may appear in any diagnostic surface.
    let sensitive = [
        secret.as_str(),
        secret.trim_start_matches(CREDENTIAL_PREFIX),
        cred.record.key_hmac.as_str(),
    ];

    for surface in [
        format!("{:?}", cred.record),
        format!(
            "{:?}",
            Pepper {
                version: 1,
                bytes: PEPPER_V1,
            }
        ),
        format!("{}", CredentialError::Unauthenticated),
        format!("{}", CredentialError::Forbidden),
        format!("{:?}", CredentialError::Unauthenticated),
        format!("{:?}", CredentialError::Forbidden),
        CredentialError::Unauthenticated.code().to_string(),
        CredentialError::Forbidden.code().to_string(),
    ] {
        for needle in sensitive {
            assert!(
                !surface.contains(needle),
                "diagnostic surface must never carry secret material: {surface:?}"
            );
        }
    }

    // The redacted Debugs still name the useful routing facts.
    let debug = format!("{:?}", cred.record);
    assert!(
        debug.contains(&cred.record.id),
        "Debug names the credential id"
    );
    assert!(
        debug.contains(&cred.record.key_prefix),
        "Debug names the non-secret listing prefix"
    );

    // Exact generic wording: clients can match on it, attackers learn
    // nothing beyond the outcome class.
    assert_eq!(CredentialError::Unauthenticated.code(), "unauthorized");
    assert_eq!(CredentialError::Unauthenticated.http_status(), 401);
    assert_eq!(CredentialError::Forbidden.code(), "forbidden");
    assert_eq!(CredentialError::Forbidden.http_status(), 403);
    assert_eq!(
        CredentialError::Unauthenticated.to_string(),
        "invalid credential"
    );
    assert_eq!(CredentialError::Forbidden.to_string(), "insufficient scope");
}
