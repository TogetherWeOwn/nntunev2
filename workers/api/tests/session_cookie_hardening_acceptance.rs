//! Acceptance tests: session cookie hardening with logout-everywhere.
//!
//! Written first (TDD RED) for the paired build slice "api: session cookie
//! hardening with logout-everywhere". The builder implements against these
//! pins and must not edit this file.
//!
//! Contract pinned here, in the pure session logic this crate owns today
//! (the HTTP route in the Worker entry calls into these helpers; there is
//! no handler layer in this crate yet — see `src/lib.rs`):
//!
//! 1. The login response cookie (`session::set_cookie_header`) carries every
//!    pinned flag, pinned as an exact header string so no attribute can
//!    drift: `__Host-` name, `Path=/`, `HttpOnly`, `Secure`, `SameSite=Lax`,
//!    a `Max-Age`, and never a `Domain=` or `SameSite=None`.
//! 2. That `Max-Age` is bounded: no caller-supplied value may push the
//!    cookie past `session::MAX_SESSION_SECS` (the module already promises
//!    a "bounded Max-Age"; today `set_cookie_header` echoes any value).
//! 3. Logout-everywhere: `session::revoke_all_sessions(records, user_id)`
//!    revokes every stored session row for that user (returning how many
//!    rows transitioned to revoked), so every pre-existing session cookie
//!    for the user fails `verify_stored_session` afterwards. Other users'
//!    rows are untouched, an empty user id revokes nothing, and a fresh
//!    login afterwards still works.
//! 4. Single-session logout regression: `session::revoke_session(record)`
//!    revokes only that one row; the user's other sessions stay valid, and
//!    `clear_cookie_header` clears the same pinned cookie.
//!
//! Naming note: `revoke_all_sessions` matches the session-store vocabulary
//! already used for the `POST /v1/session/revoke-all` route elsewhere in
//! this repo's history, so the Worker route can wire straight to it.
//!
//! Fixtures use synthetic credentials only (public repo: never real
//! secrets).

use nntune_api::session;

fn test_secret() -> Vec<u8> {
    b"test-session-secret-that-is-32-bytes!!".to_vec()
}

const NOW: u64 = 1_700_000_000;

/// Build the server-side row that joins to a freshly issued token, the way
/// a login handler persists it after `issue_session`.
fn record_for(secret: &[u8], token: &str) -> session::SessionRecord {
    let claims = session::verify_session(secret, token, NOW).expect("issued token verifies");
    session::SessionRecord {
        id: claims.session_id,
        user_id: claims.user_id,
        created_at: claims.created_at,
        expires_at: claims.expires_at,
        rotation_counter: claims.rotation_counter,
        revoked: false,
    }
}

/// One "login": issue a token and its stored row.
fn login(secret: &[u8], user_id: &str) -> (String, session::SessionRecord) {
    let token = session::issue_session(secret, user_id, NOW, 3600).expect("login issues a session");
    let record = record_for(secret, &token);
    (token, record)
}

// --- 1. Login response cookie carries all pinned flags ----------------------

#[test]
fn login_response_cookie_carries_all_pinned_flags() {
    let secret = test_secret();
    let (token, _record) = login(&secret, "user-123");

    // The pinned cookie name is the `__Host-` one, not a bare name a
    // subdomain could shadow.
    assert_eq!(session::COOKIE_NAME, "__Host-nntune-session");

    // Exact header: every pinned flag present, in the pinned spelling, and
    // nothing else — no `Domain=`, no `SameSite=None`, no widened `Path`.
    let header = session::set_cookie_header(&token, 3600);
    assert_eq!(
        header,
        format!(
            "__Host-nntune-session={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=3600"
        ),
        "login cookie must carry exactly the pinned flags"
    );
    assert!(
        !header.contains("Domain="),
        "__Host- cookie must never carry Domain=: {header}"
    );
    assert!(
        !header.contains("SameSite=None"),
        "SameSite=None is banned: {header}"
    );
}

#[test]
fn login_response_cookie_max_age_is_bounded() {
    let secret = test_secret();
    let (token, _record) = login(&secret, "user-123");

    // A caller asking for more than the absolute session lifetime must not
    // get a longer-lived cookie: the emitted Max-Age is clamped to
    // MAX_SESSION_SECS, matching the token lifetime cap.
    for asked in [session::MAX_SESSION_SECS + 1, u64::MAX] {
        let header = session::set_cookie_header(&token, asked);
        assert_eq!(
            header,
            format!(
                "__Host-nntune-session={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={}",
                session::MAX_SESSION_SECS
            ),
            "asked Max-Age={asked} must clamp to the session lifetime cap"
        );
    }

    // A value inside the cap is emitted unchanged.
    let header = session::set_cookie_header(&token, 60);
    assert!(
        header.ends_with("Max-Age=60"),
        "in-cap Max-Age passes through: {header}"
    );
}

// --- 2. Logout-everywhere invalidates all sessions for the user -------------

#[test]
fn logout_everywhere_invalidates_all_sessions_for_the_user() {
    let secret = test_secret();
    let keys = session::SessionKeys::single(&secret);

    // Login x2 (two devices/browsers) for the same user, plus one login
    // for a different user who must not be affected.
    let (token_a, record_a) = login(&secret, "user-123");
    let (token_b, record_b) = login(&secret, "user-123");
    let (token_other, record_other) = login(&secret, "user-999");
    assert_ne!(
        record_a.id, record_b.id,
        "each login is a distinct stored session"
    );

    let mut records = vec![record_a, record_b, record_other];
    // Every pre-existing cookie authenticates against its stored row.
    for (token, record) in [
        (&token_a, &records[0]),
        (&token_b, &records[1]),
        (&token_other, &records[2]),
    ] {
        assert!(
            session::verify_stored_session(&keys, token, NOW + 60, record).is_ok(),
            "session must be valid before logout-everywhere"
        );
    }

    // Logout-everywhere for user-123 revokes exactly that user's rows.
    let revoked = session::revoke_all_sessions(&mut records, "user-123");
    assert_eq!(revoked, 2, "both of the user's sessions are revoked");
    assert!(records[0].revoked, "first session row revoked");
    assert!(records[1].revoked, "second session row revoked");
    assert!(!records[2].revoked, "another user's session is untouched");

    // Both pre-existing cookies are rejected afterwards, with the generic
    // error (no oracle for why); the other user's cookie still works.
    for (token, record) in [(&token_a, &records[0]), (&token_b, &records[1])] {
        assert_eq!(
            session::verify_stored_session(&keys, token, NOW + 61, record),
            Err(session::SessionError::Invalid),
            "pre-logout-everywhere cookie must be rejected"
        );
    }
    assert!(
        session::verify_stored_session(&keys, &token_other, NOW + 61, &records[2]).is_ok(),
        "another user's session survives someone else's logout-everywhere"
    );

    // The response clears the caller's own pinned cookie too.
    assert_eq!(
        session::clear_cookie_header(),
        "__Host-nntune-session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
        "logout-everywhere must clear the same __Host- cookie"
    );

    // Idempotent: a second logout-everywhere revokes nothing new ...
    assert_eq!(
        session::revoke_all_sessions(&mut records, "user-123"),
        0,
        "already-revoked rows are not counted twice"
    );

    // ... and it is not a ban: a fresh login afterwards authenticates.
    let (token_fresh, record_fresh) = login(&secret, "user-123");
    assert!(
        session::verify_stored_session(&keys, &token_fresh, NOW + 62, &record_fresh).is_ok(),
        "a fresh login after logout-everywhere must work"
    );
}

#[test]
fn logout_everywhere_with_empty_user_id_revokes_nothing() {
    let secret = test_secret();
    let (_token, record) = login(&secret, "user-123");
    let mut records = vec![record];

    // Fail closed: a missing/empty user id (e.g. a bug upstream of the
    // route) must never turn into a mass revocation.
    assert_eq!(session::revoke_all_sessions(&mut records, ""), 0);
    assert!(!records[0].revoked, "no row revoked for an empty user id");
    assert_eq!(session::revoke_all_sessions(&mut [], "user-123"), 0);
}

// --- 3. Single-session logout still works (regression) ----------------------

#[test]
fn single_session_logout_still_works() {
    let secret = test_secret();
    let keys = session::SessionKeys::single(&secret);

    // Two sessions for one user; logging out of one kills only that one.
    let (token_a, mut record_a) = login(&secret, "user-123");
    let (token_b, record_b) = login(&secret, "user-123");
    assert!(
        session::verify_stored_session(&keys, &token_a, NOW + 60, &record_a).is_ok(),
        "session A valid before logout"
    );

    session::revoke_session(&mut record_a);
    assert!(record_a.revoked, "the logged-out row is revoked");
    // Revoking again is a harmless no-op, never a panic or un-revoke.
    session::revoke_session(&mut record_a);
    assert!(record_a.revoked, "double logout stays revoked");

    assert_eq!(
        session::verify_stored_session(&keys, &token_a, NOW + 61, &record_a),
        Err(session::SessionError::Invalid),
        "the logged-out cookie is rejected afterwards"
    );
    assert!(
        session::verify_stored_session(&keys, &token_b, NOW + 61, &record_b).is_ok(),
        "the user's other session is NOT killed by a single-session logout"
    );

    // And the browser is told to drop the cookie: same pinned name/flags,
    // empty value, Max-Age=0.
    assert_eq!(
        session::clear_cookie_header(),
        "__Host-nntune-session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
        "single-session logout clears the same __Host- cookie"
    );
}
