//! Independent LIVE key-only refusal/privacy contract (accepted ADR 0002/0006).
//! Historical v1 source-derived fixtures are not evidence for this contract.
//! Literal wire precedence: 43 presence/mixed conflicts, 42 passwords, 44 key
//! faults, 10 incomplete token, otherwise 40. Synthetic inputs, no I/O.

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

#[derive(Default)]
struct AuthCalls {
    key: Cell<usize>,
    password: Cell<usize>,
    token: Cell<usize>,
}

struct KeyOnlyLookup(Rc<AuthCalls>);

fn owner() -> Identity {
    Identity {
        username: "synthetic-persisted-user-alpha".into(),
        email: "alpha@example.test".into(),
        is_admin: false,
    }
}

impl AuthStore for KeyOnlyLookup {
    fn verify_api_key(&self, key: &str) -> Option<Identity> {
        self.0.key.set(self.0.key.get() + 1);
        (key == KEY).then(owner)
    }

    // Deliberately permissive tripwires: a protocol fallback is a bug even when
    // a backend happens to support it. The live slice must never invoke these.
    fn verify_password(&self, _: &str, _: &str) -> Option<Identity> {
        self.0.password.set(self.0.password.get() + 1);
        Some(owner())
    }

    fn verify_token(&self, _: &str, _: &str, _: &str) -> Option<Identity> {
        self.0.token.set(self.0.token.get() + 1);
        Some(owner())
    }
}

fn assert_error(response: &HttpResponse, code: i64, message: &str) {
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
    let body = std::str::from_utf8(&response.body).unwrap();
    assert!(body.contains("\"status\":\"failed\""), "{body}");
    assert!(body.contains(&format!("\"code\":{code}")), "{body}");
    assert!(
        body.contains(&format!("\"message\":\"{message}\"")),
        "{body}"
    );
}

fn assert_refusal(query: &str, code: i64, message: &str) {
    for method in ["GET", "POST"] {
        let lookups = Rc::new(AuthCalls::default());
        let (library, calls) = GuardedLibrary::new(KeyOnlyLookup(Rc::clone(&lookups)));
        let app = SubsonicApp::new(library);
        let response = if method == "GET" {
            app.handle(method, "/rest/getPlaylists", &format!("f=json&{query}"), "")
        } else {
            app.handle(method, "/rest/getPlaylists.view", "f=json", query)
        };
        assert_error(&response, code, message);
        assert!(calls.borrow().is_empty(), "denied request reached Library");
        assert_eq!(lookups.password.get(), 0, "password verifier was invoked");
        assert_eq!(lookups.token.get(), 0, "token verifier was invoked");
        if code != 44 {
            assert_eq!(lookups.key.get(), 0, "refusal queried key storage");
        }
    }
}

#[test]
fn a_valid_key_forwards_its_identity_to_the_protected_library_method() {
    let lookups = Rc::new(AuthCalls::default());
    let (library, calls) = GuardedLibrary::new(KeyOnlyLookup(Rc::clone(&lookups)));
    let response = SubsonicApp::new(library).handle(
        "GET",
        "/rest/getPlaylists",
        &format!("f=json&apiKey={KEY}"),
        "",
    );
    assert_eq!(response.status, 200);
    assert!(std::str::from_utf8(&response.body)
        .unwrap()
        .contains("\"status\":\"ok\""));
    assert_eq!(lookups.key.get(), 1);
    assert_eq!(*calls.borrow(), [owner()]);
}

// Break caught: filtering out empty strings before detecting parameter names.
#[test]
fn api_key_and_u_names_conflict_even_if_either_or_both_values_are_empty() {
    for query in [
        format!("apiKey={KEY}&u=owner-beta"),
        format!("apiKey={KEY}&u="),
        "apiKey=&u=owner-alpha".into(),
        "apiKey=&u=".into(),
        "u=&apiKey=".into(),
        "apiKey=&u=&p=enc:zz&t=token".into(),
    ] {
        assert_refusal(&query, 43, "Conflicting authentication parameters.");
    }
}

#[test]
fn presence_conflict_also_applies_across_query_and_form_body() {
    for (query, body) in [
        (format!("apiKey={KEY}"), "u=".to_owned()),
        ("u=".to_owned(), format!("apiKey={KEY}")),
        ("apiKey=".to_owned(), "u=".to_owned()),
    ] {
        let lookups = Rc::new(AuthCalls::default());
        let (library, calls) = GuardedLibrary::new(KeyOnlyLookup(Rc::clone(&lookups)));
        let response = SubsonicApp::new(library).handle(
            "POST",
            "/rest/getPlaylists.view",
            &format!("f=json&{query}"),
            &body,
        );
        assert_error(&response, 43, "Conflicting authentication parameters.");
        assert_eq!(lookups.key.get(), 0);
        assert!(calls.borrow().is_empty());
    }
}

#[test]
fn nonempty_key_mixed_with_any_nonempty_legacy_value_conflicts_first() {
    for legacy in [
        "p=plain",
        "p=enc:zz",
        "t=token",
        "s=salt",
        "p=plain&t=token&s=salt",
    ] {
        assert_refusal(
            &format!("apiKey={KEY}&{legacy}"),
            43,
            "Conflicting authentication parameters.",
        );
    }
}

#[test]
fn empty_p_t_s_without_u_do_not_conflict_with_a_usable_key() {
    let lookups = Rc::new(AuthCalls::default());
    let (library, calls) = GuardedLibrary::new(KeyOnlyLookup(Rc::clone(&lookups)));
    let response = SubsonicApp::new(library).handle(
        "GET",
        "/rest/getPlaylists",
        &format!("f=json&apiKey={KEY}&p=&t=&s="),
        "",
    );
    assert!(std::str::from_utf8(&response.body)
        .unwrap()
        .contains("\"status\":\"ok\""));
    assert_eq!(*calls.borrow(), [owner()]);
    assert_eq!(lookups.key.get(), 1);
    assert_eq!(lookups.password.get(), 0);
    assert_eq!(lookups.token.get(), 0);
}

// Break caught: decoding enc: or calling any password backend before refusing.
#[test]
fn every_nonempty_password_is_unsupported_without_decoding_or_verification() {
    for password in [
        PASSWORD,
        "enc:73796e746865746963",
        "enc:zz",
        "enc:0",
        "enc:",
        "%00",
        "%ZZ",
    ] {
        for prefix in ["", "u=owner-alpha&", "u=&", "apiKey=&"] {
            assert_refusal(
                &format!("{prefix}p={password}"),
                42,
                "Provided authentication mechanism not supported.",
            );
        }
    }
    assert_refusal(
        "u=owner-alpha&p=enc:zz&t=token",
        42,
        "Provided authentication mechanism not supported.",
    );
}

#[test]
fn token_refusal_retains_missing_salt_and_missing_value_distinctions() {
    for query in [
        "u=owner&t=token",
        "u=owner&t=token&s=",
        "u=owner&t=token&p=",
    ] {
        assert_refusal(query, 10, "Required parameter is missing.");
    }
    for query in [
        "u=owner&t=token&s=salt",
        "u=&t=token&s=salt",
        "u=owner&t=&s=salt",
        "t=token",
        "t=token&s=salt",
        "u=owner",
        "p=",
        "u=&p=&t=&s=",
        "",
    ] {
        assert_refusal(query, 40, "Wrong username or password.");
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
        assert_refusal(query, code, message);
    }
}

#[test]
fn unconfigured_auth_never_accepts_published_fixture_credentials() {
    for (query, code, message) in [
        ("apiKey=test-key-1", 44, "Invalid API key."),
        ("apiKey=test-key-2", 44, "Invalid API key."),
        (
            "u=test&p=test-pass",
            42,
            "Provided authentication mechanism not supported.",
        ),
        (
            "u=test&p=enc:746573742d70617373",
            42,
            "Provided authentication mechanism not supported.",
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
fn credentials_debug_is_safe_for_normal_pretty_and_nested_diagnostics() {
    let params = credential_params();
    let credentials = Credentials::from_params(|key| params.get(key).map(str::to_owned));
    for diagnostic in [
        format!("{credentials:?}"),
        format!("{credentials:#?}"),
        format!("{:?}", Some(&credentials)),
        format!("{:#?}", Result::<(), _>::Err(&credentials)),
    ] {
        assert_no_credentials(&diagnostic);
    }
}

#[test]
fn parsed_auth_params_debug_does_not_disclose_credentials() {
    assert_no_credentials(&format!("{:?}", credential_params()));
}

#[test]
fn parsed_auth_params_pretty_debug_does_not_disclose_credentials() {
    assert_no_credentials(&format!("{:#?}", credential_params()));
    assert_no_credentials(&format!("{:#?}", Some(credential_params())));
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
