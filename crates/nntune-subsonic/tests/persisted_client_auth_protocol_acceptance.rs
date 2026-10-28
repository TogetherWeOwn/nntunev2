//! Independent protocol/privacy controls for the persisted-client auth slice.
//!
//! Uses the merged ADR 0001 / migration 0023 refusal contract, not the unshipped
//! ADR 0006 credential redesign: 40 missing/legacy, 43 conflicting parameters,
//! 44 invalid API key, 10 token without salt. No real credentials or I/O.
//! The separate API bank exercises the future persisted lookup adapter.

#[path = "support/client_auth_library.rs"]
mod client_auth_library;

use client_auth_library::GuardedLibrary;
use nntune_subsonic::{
    AuthStore, Credentials, HttpResponse, Identity, MemStore, Params, SubsonicApp,
};
use std::cell::Cell;
use std::rc::Rc;

const KEY: &str = "synthetic-client-key-only-for-tests";
const PASSWORD: &str = "synthetic-password-only-for-tests";
const TOKEN: &str = "synthetic-token-only-for-tests";
const SALT: &str = "synthetic-salt-only-for-tests";

struct KeyOnlyLookup {
    calls: Rc<Cell<usize>>,
}

impl AuthStore for KeyOnlyLookup {
    fn verify_api_key(&self, key: &str) -> Option<Identity> {
        self.calls.set(self.calls.get() + 1);
        (key == KEY).then(|| Identity {
            username: "owner-alpha".into(),
            email: "alpha@example.test".into(),
            is_admin: false,
        })
    }

    fn verify_password(&self, _: &str, _: &str) -> Option<Identity> {
        None
    }

    fn verify_token(&self, _: &str, _: &str, _: &str) -> Option<Identity> {
        None
    }
}

fn assert_error(response: &HttpResponse, code: i64, message: &str) {
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
    let body = std::str::from_utf8(&response.body).unwrap();
    assert!(body.contains("\"status\":\"failed\""));
    assert!(body.contains(&format!("\"code\":{code}")));
    assert!(body.contains(&format!("\"message\":\"{message}\"")));
}

#[test]
fn a_valid_key_forwards_its_identity_to_the_protected_library_method() {
    let lookups = Rc::new(Cell::new(0));
    let (library, calls) = GuardedLibrary::new(KeyOnlyLookup {
        calls: Rc::clone(&lookups),
    });
    let app = SubsonicApp::new(library);
    let response = app.handle(
        "GET",
        "/rest/getPlaylists",
        &format!("f=json&apiKey={KEY}"),
        "",
    );
    assert_eq!(response.status, 200);
    assert!(std::str::from_utf8(&response.body)
        .unwrap()
        .contains("\"status\":\"ok\""));
    assert_eq!(lookups.get(), 1);
    assert_eq!(
        *calls.borrow(),
        [Identity {
            username: "owner-alpha".into(),
            email: "alpha@example.test".into(),
            is_admin: false,
        }]
    );
}

#[test]
fn a_claimed_other_user_is_a_conflict_before_key_lookup_or_library_access() {
    for legacy in ["u=owner-beta", "p=synthetic", "t=synthetic", "s=synthetic"] {
        let lookups = Rc::new(Cell::new(0));
        let (library, calls) = GuardedLibrary::new(KeyOnlyLookup {
            calls: Rc::clone(&lookups),
        });
        let app = SubsonicApp::new(library);
        let response = app.handle(
            "GET",
            "/rest/getPlaylists",
            &format!("f=json&apiKey={KEY}&{legacy}"),
            "",
        );
        assert_error(&response, 43, "Conflicting authentication parameters.");
        assert_eq!(
            lookups.get(),
            0,
            "conflicting requests must not reach auth storage"
        );
        assert!(calls.borrow().is_empty());
    }
}

#[test]
fn missing_empty_unknown_and_malformed_keys_deny_before_library_access() {
    for (query, code, message) in [
        ("", 40, "Wrong username or password."),
        ("apiKey=", 40, "Wrong username or password."),
        ("apiKey=synthetic-unknown", 44, "Invalid API key."),
        ("apiKey=%00", 44, "Invalid API key."),
        ("apiKey=%ZZ", 44, "Invalid API key."),
        ("apiKey[]=synthetic", 40, "Wrong username or password."),
    ] {
        let (library, calls) = GuardedLibrary::new(MemStore::empty());
        let response = SubsonicApp::new(library).handle(
            "GET",
            "/rest/getPlaylists",
            &format!("f=json&{query}"),
            "",
        );
        assert_error(&response, code, message);
        assert!(calls.borrow().is_empty());
    }
}

#[test]
fn unconfigured_auth_never_accepts_the_published_fixture_or_legacy_credentials() {
    for (query, code, message) in [
        ("apiKey=test-key-1", 44, "Invalid API key."),
        ("apiKey=test-key-2", 44, "Invalid API key."),
        ("u=test&p=test-pass", 40, "Wrong username or password."),
        (
            "u=test&p=enc:746573742d70617373",
            40,
            "Wrong username or password.",
        ),
        (
            "u=test&t=synthetic&s=synthetic",
            40,
            "Wrong username or password.",
        ),
        ("u=test&t=synthetic", 10, "Required parameter is missing."),
    ] {
        let (library, calls) = GuardedLibrary::new(MemStore::empty());
        let response =
            SubsonicApp::new(library).handle("POST", "/rest/getPlaylists.view", "f=json", query);
        assert_error(&response, code, message);
        assert!(calls.borrow().is_empty());
    }
}

fn credential_params() -> Params {
    Params::parse(&format!(
        "apiKey={KEY}&u=owner-canary&p={PASSWORD}&t={TOKEN}&s={SALT}"
    ))
}

fn assert_no_credentials(output: &str) {
    for canary in [KEY, PASSWORD, TOKEN, SALT, "owner-canary"] {
        assert!(!output.contains(canary), "credential value disclosed");
    }
}

#[test]
fn credentials_debug_is_safe_for_normal_and_pretty_log_formatting() {
    let params = credential_params();
    let credentials = Credentials::from_params(|key| params.get(key).map(str::to_owned));
    assert_no_credentials(&format!("{credentials:?}"));
    assert_no_credentials(&format!("{credentials:#?}"));
}

// Break caught: deriving value-bearing Debug for the parsed credential carrier
// bypasses Credentials' presence-only formatter when a request is logged.
#[test]
fn parsed_auth_params_debug_does_not_disclose_credentials() {
    assert_no_credentials(&format!("{:?}", credential_params()));
}

#[test]
fn parsed_auth_params_pretty_debug_does_not_disclose_credentials() {
    assert_no_credentials(&format!("{:#?}", credential_params()));
}

#[test]
fn error_envelopes_do_not_echo_any_credential_value() {
    for format in ["xml", "json", "jsonp&callback=safeCallback"] {
        let query =
            format!("f={format}&apiKey={KEY}&u=owner-canary&p={PASSWORD}&t={TOKEN}&s={SALT}");
        let response = SubsonicApp::new(MemStore::empty()).handle("GET", "/rest/ping", &query, "");
        assert_eq!(response.status, 200);
        assert_no_credentials(std::str::from_utf8(&response.body).unwrap());
        assert_no_credentials(&format!("{response:?}"));
        assert_no_credentials(&format!("{response:#?}"));
    }
}
