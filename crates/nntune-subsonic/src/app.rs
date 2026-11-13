//! Request dispatch: path → auth → endpoint → wire bytes.
//!
//! `SubsonicApp<S: Library>` is transport-free so the same code serves the
//! Cloudflare worker shell, native tests, and the golden fixture suite.
//! Mutations go through `RefCell` so `handle` stays `&self`.

use crate::auth::{authenticate, Credentials, Identity};
use crate::endpoints::{handle_endpoint, EndpointResult, Params};
use crate::envelope::{
    error_document, ok_document, render_json, render_jsonp, render_xml, JSON_CONTENT_TYPE,
    XML_CONTENT_TYPE,
};

/// v1 `MergeFormPostParams::MAX_BODY_BYTES`: a POST body over 10 MB is
/// refused with error 10 before any parsing — its parameters cannot be
/// trusted, so nothing downstream may act on a prefix of them.
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
use crate::library::Library;
use crate::routes::{route, Route};
use std::cell::RefCell;

/// Plain HTTP response produced for one Subsonic request.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl HttpResponse {
    fn document(status: u16, content_type: &str, body: Vec<u8>) -> HttpResponse {
        HttpResponse {
            status,
            content_type: content_type.to_string(),
            body,
        }
    }
}

pub struct SubsonicApp<S> {
    store: RefCell<S>,
    production: bool,
}

impl<S: Library> SubsonicApp<S> {
    /// Legacy protocol behaviour for the golden fixtures, not a production mount.
    pub fn new(store: S) -> SubsonicApp<S> {
        SubsonicApp {
            store: RefCell::new(store),
            production: false,
        }
    }

    /// Launch policy (ADR 0006): no password auth or offline downloads.
    /// Production audio must be dispatched by the async worker adapter through
    /// the shared playback gate, never through the fixture `Library::audio` seam.
    pub fn production(store: S) -> SubsonicApp<S> {
        SubsonicApp {
            store: RefCell::new(store),
            production: true,
        }
    }

    /// Handle one request. `method` is accepted as GET or POST (v1 registers
    /// both on every route); `path` is the URL path, `query` the raw query
    /// string without `?`, `body` the raw POST form body (empty for GET).
    /// For POST, body pairs are merged after query pairs (v1 reads params
    /// with `$request->input()`, query plus form body); query wins on
    /// duplicates since `Params::get` returns the first value.
    pub fn handle(&self, method: &str, path: &str, query: &str, body: &str) -> HttpResponse {
        // v1 caps the body before routing or auth: only the query string is
        // parsed for `f`, since body parameters are untrusted past the cap.
        if method.eq_ignore_ascii_case("POST") && body.len() > MAX_BODY_BYTES {
            return oversized_body_response(query);
        }
        let merged = Self::merge_query(method, query, body);
        match route(path) {
            Route::Unknown => HttpResponse::document(404, "text/plain", b"Not Found".to_vec()),
            Route::Hls => HttpResponse::document(410, "text/plain", b"Gone".to_vec()),
            Route::Unsupported(op) => {
                let params = Params::parse(&merged);
                let doc = error_document(
                    crate::envelope::code::GENERIC,
                    &format!("The {op} operation is not supported by this server."),
                );
                Self::render(&params, &doc)
            }
            // Public per the OpenSubsonic spec: body merge and cap apply,
            // authentication is skipped (v1 serves it outside the auth group).
            Route::PublicEndpoint(name) => {
                let params = Params::parse(&merged);
                let identity = Identity::for_public_route();
                let mut store = self.store.borrow_mut();
                Self::render_endpoint(
                    &params,
                    handle_endpoint(&mut *store, &identity, name, &params, !self.production),
                )
            }
            Route::Endpoint(name) => {
                let params = Params::parse(&merged);
                // Presence, not the winning value: an empty or duplicate p=
                // must not be rescued by valid credentials in another field.
                if self.production && !params.get_all("p").is_empty() {
                    return Self::render(
                        &params,
                        &error_document(
                            crate::envelope::code::UNSUPPORTED_AUTH,
                            "Provided authentication mechanism not supported.",
                        ),
                    );
                }
                let get = |key: &str| params.get(key).map(|s| s.to_string());
                let creds = Credentials::from_params(get);
                let mut store = self.store.borrow_mut();
                let identity = match authenticate(&*store, &creds) {
                    Ok(identity) => identity,
                    Err((error_code, message)) => {
                        return Self::render(&params, &error_document(error_code, &message));
                    }
                };
                Self::render_endpoint(
                    &params,
                    handle_endpoint(&mut *store, &identity, name, &params, !self.production),
                )
            }
        }
    }

    fn render_endpoint(params: &Params, result: EndpointResult) -> HttpResponse {
        match result {
            EndpointResult::Doc(payload) => Self::render(params, &ok_document(payload)),
            EndpointResult::Err(error_code, message) => {
                Self::render(params, &error_document(error_code, &message))
            }
            EndpointResult::Binary { content_type, body } => {
                HttpResponse::document(200, &content_type, body)
            }
        }
    }

    fn merge_query(method: &str, query: &str, body: &str) -> String {
        if method.eq_ignore_ascii_case("POST") && !body.is_empty() {
            if query.is_empty() {
                body.to_string()
            } else {
                format!("{query}&{body}")
            }
        } else {
            query.to_string()
        }
    }

    fn render(params: &Params, doc: &crate::value::Value) -> HttpResponse {
        match params.get("f") {
            Some("json") => HttpResponse::document(200, JSON_CONTENT_TYPE, render_json(doc)),
            Some("jsonp") => {
                let callback = params.get("callback");
                let (body, content_type) = render_jsonp(doc, callback);
                HttpResponse::document(200, content_type, body)
            }
            _ => HttpResponse::document(200, XML_CONTENT_TYPE, render_xml(doc)),
        }
    }
}

/// Error-10 document for an over-cap POST body, answered without touching
/// the store. The worker shell calls this on the `Content-Length` fast path
/// (before buffering the body); `handle` calls it on the actual byte length.
pub fn oversized_body_response(query: &str) -> HttpResponse {
    body_error_response(query, "POST body exceeds the 10 MB limit.")
}

/// A failed or invalid form-body read must not dispatch partial parameters.
/// Like the size cap, use only query parameters to select the error envelope.
pub fn unreadable_body_response(query: &str) -> HttpResponse {
    body_error_response(query, "Unable to read request body.")
}

/// Extract only error-envelope selectors, with a fixed work and allocation budget.
/// If either source exceeds the scan budget, fall back to XML rather than
/// trusting a prefix that could hide an earlier selector in the query.
/// Oversized selector values remain present but invalid, preserving first-wins.
pub fn bounded_error_params(query: &str, body: &str) -> String {
    const MAX_SOURCE: usize = 256 * 1024;
    const MAX_PAIRS: usize = 1024;
    let mut format = None;
    let mut callback = None;
    for source in [query, body] {
        if source.len() > MAX_SOURCE {
            return String::new();
        }
        let mut count = 0;
        for part in source.split('&').filter(|part| !part.is_empty()) {
            count += 1;
            if count > MAX_PAIRS {
                return String::new();
            }
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            // An encoded selector name is at most three bytes per ASCII byte.
            if key.len() > 24 {
                continue;
            }
            match crate::endpoints::decode(key).as_str() {
                "f" if format.is_none() => {
                    format = Some(if value.len() <= 15 { value } else { "" });
                }
                "callback" if callback.is_none() => {
                    callback = Some(if value.len() <= 384 { value } else { "" });
                }
                _ => {}
            }
        }
    }
    let mut result = String::new();
    if let Some(value) = format {
        result.push_str("f=");
        result.push_str(value);
    }
    if let Some(value) = callback {
        if !result.is_empty() {
            result.push('&');
        }
        result.push_str("callback=");
        result.push_str(value);
    }
    result
}

fn body_error_response(query: &str, message: &str) -> HttpResponse {
    let params = Params::parse(&bounded_error_params(query, ""));
    let doc = error_document(crate::envelope::code::REQUIRED_PARAM_MISSING, message);
    match params.get("f") {
        Some("json") => HttpResponse::document(200, JSON_CONTENT_TYPE, render_json(&doc)),
        Some("jsonp") => {
            let (body, content_type) = render_jsonp(&doc, params.get("callback"));
            HttpResponse::document(200, content_type, body)
        }
        _ => HttpResponse::document(200, XML_CONTENT_TYPE, render_xml(&doc)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::MemStore;

    fn get(app: &SubsonicApp<MemStore>, path: &str, query: &str) -> HttpResponse {
        app.handle("GET", path, query, "")
    }

    #[test]
    fn body_fault_selection_is_bounded_and_first_wins() {
        assert_eq!(
            unreadable_body_response("%66=json").content_type,
            JSON_CONTENT_TYPE
        );
        assert_eq!(
            oversized_body_response("f=xml&f=json").content_type,
            XML_CONTENT_TYPE
        );
        assert_eq!(
            unreadable_body_response(&format!("f=json&x={}", "a".repeat(256 * 1024))).content_type,
            XML_CONTENT_TYPE
        );
        assert_eq!(
            unreadable_body_response(&format!("f=json&{}", vec!["x"; 1024].join("&"))).content_type,
            XML_CONTENT_TYPE
        );
        let response =
            unreadable_body_response(&format!("f=jsonp&callback={}&callback=ok", "a".repeat(385)));
        assert_eq!(response.content_type, JSON_CONTENT_TYPE);
        assert!(String::from_utf8(response.body)
            .unwrap()
            .contains("\"code\":10"));
    }

    #[test]
    fn post_behaves_like_get() {
        let app = SubsonicApp::new(MemStore::seeded());
        let key = "apiKey=test-key-1&v=1.16.1&c=t";
        assert_eq!(
            app.handle("POST", "/rest/ping", key, ""),
            get(&app, "/rest/ping", key)
        );
    }

    #[test]
    fn post_form_body_authenticates_like_query() {
        let app = SubsonicApp::new(MemStore::seeded());
        // Credentials entirely in the POST form body (v1 `$request->input()`).
        let res = app.handle("POST", "/rest/ping", "v=1.16.1&c=t", "apiKey=test-key-1");
        assert_eq!(res.status, 200);
        assert!(String::from_utf8(res.body)
            .unwrap()
            .contains("status=\"ok\""));
        // Query wins on duplicates; body-only password auth works too.
        let res = app.handle("POST", "/rest/ping", "v=1.16.1&c=t", "u=test&p=test-pass");
        assert!(String::from_utf8(res.body)
            .unwrap()
            .contains("status=\"ok\""));
    }

    #[test]
    fn production_refuses_password_presence_in_query_or_form() {
        let app = SubsonicApp::production(MemStore::seeded());
        for password in [
            "p=test-pass",
            "p=enc:746573742d70617373",
            "p=",
            "p=&p=test-pass",
            "%70=test-pass",
        ] {
            for method in ["GET", "POST"] {
                for in_body in [false, true] {
                    if in_body && method == "GET" {
                        continue;
                    }
                    let query = if in_body {
                        "apiKey=test-key-1&f=json".to_string()
                    } else {
                        format!("apiKey=test-key-1&f=json&{password}")
                    };
                    let body = if in_body { password } else { "" };
                    let res = app.handle(method, "/rest/ping.view", &query, body);
                    assert_eq!(res.status, 200);
                    let text = String::from_utf8(res.body).unwrap();
                    assert!(text.contains("\"code\":42"), "{method}: {text}");
                    assert!(!text.contains("test-pass"));
                }
            }
        }
    }

    #[test]
    fn production_keeps_key_token_and_conflict_semantics() {
        let app = SubsonicApp::production(MemStore::seeded());
        let salted = crate::auth::md5_hex(b"test-passabc");
        for query in [
            "apiKey=test-key-1".to_string(),
            format!("u=test&t={salted}&s=abc"),
        ] {
            let res = get(&app, "/rest/ping", &query);
            assert!(String::from_utf8(res.body)
                .unwrap()
                .contains("status=\"ok\""));
        }
        for (query, code) in [
            ("apiKey=wrong", 44),
            ("apiKey=test-key-1&u=test", 43),
            ("u=test&t=token", 10),
            ("u=test&t=wrong&s=abc", 40),
        ] {
            let res = get(&app, "/rest/ping", query);
            assert!(String::from_utf8(res.body)
                .unwrap()
                .contains(&format!("code=\"{code}\"")));
        }
    }

    #[test]
    fn production_never_serves_fixture_audio_or_offers_downloads() {
        let app = SubsonicApp::production(MemStore::seeded());
        for (endpoint, code) in [("download", 0), ("stream", 70)] {
            for method in ["GET", "POST"] {
                let res = app.handle(
                    method,
                    &format!("/rest/{endpoint}.view"),
                    "apiKey=test-key-1&f=json&id=so-1&format=mp3&maxBitRate=128",
                    "",
                );
                assert_eq!(res.status, 200);
                assert_eq!(res.content_type, JSON_CONTENT_TYPE);
                assert!(String::from_utf8(res.body)
                    .unwrap()
                    .contains(&format!("\"code\":{code}")));
            }
        }
        let res = get(
            &app,
            "/rest/getUser",
            "apiKey=test-key-1&username=test&f=json",
        );
        assert!(String::from_utf8(res.body)
            .unwrap()
            .contains("\"downloadRole\":false"));
        // The legacy fixture contract remains available only outside launch mode.
        let legacy = SubsonicApp::new(MemStore::seeded());
        let res = get(&legacy, "/rest/download", "apiKey=test-key-1&id=so-1");
        assert_eq!(res.body, crate::library::DEMO_AUDIO);
    }

    #[test]
    fn production_empty_mount_never_accepts_fixture_credentials() {
        let app = SubsonicApp::production(MemStore::empty());
        let res = get(&app, "/rest/ping", "apiKey=test-key-1&f=json");
        assert!(String::from_utf8(res.body).unwrap().contains("\"code\":44"));
        let res = get(&app, "/rest/getOpenSubsonicExtensions", "f=json");
        assert!(String::from_utf8(res.body)
            .unwrap()
            .contains("\"status\":\"ok\""));
    }

    #[test]
    fn password_and_token_auth_work() {
        let app = SubsonicApp::new(MemStore::seeded());
        let res = get(&app, "/rest/ping", "u=test&p=test-pass&v=1.16.1&c=t");
        assert_eq!(res.status, 200);
        assert!(String::from_utf8(res.body)
            .unwrap()
            .contains("status=\"ok\""));
        let salted = crate::auth::md5_hex(b"test-passabc");
        let res = get(
            &app,
            "/rest/ping",
            &format!("u=test&t={salted}&s=abc&v=1.16.1&c=t"),
        );
        assert_eq!(res.status, 200);
        assert!(String::from_utf8(res.body)
            .unwrap()
            .contains("status=\"ok\""));
    }
}
