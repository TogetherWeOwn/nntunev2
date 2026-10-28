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
}

impl<S: Library> SubsonicApp<S> {
    pub fn new(store: S) -> SubsonicApp<S> {
        SubsonicApp {
            store: RefCell::new(store),
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
                    handle_endpoint(&mut *store, &identity, name, &params),
                )
            }
            Route::Endpoint(name) => {
                let params = Params::parse(&merged);
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
                    handle_endpoint(&mut *store, &identity, name, &params),
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

fn body_error_response(query: &str, message: &str) -> HttpResponse {
    let params = Params::parse(query);
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
    fn post_behaves_like_get() {
        let app = SubsonicApp::new(MemStore::seeded());
        let key = "apiKey=test-key-1&v=1.16.1&c=t";
        assert_eq!(
            app.handle("POST", "/rest/ping", key, ""),
            get(&app, "/rest/ping", key)
        );
    }

    fn assert_auth_error(res: HttpResponse, code: u32, message: &str) {
        assert_eq!(res.status, 200);
        let body = String::from_utf8(res.body).unwrap();
        assert!(body.contains("status=\"failed\""), "{body}");
        assert!(body.contains(&format!("code=\"{code}\"")), "{body}");
        assert!(body.contains(&format!("message=\"{message}\"")), "{body}");
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
        // Query wins on duplicate keys in either direction.
        let res = app.handle(
            "POST",
            "/rest/ping",
            "v=1.16.1&c=t&apiKey=test-key-1",
            "apiKey=bad",
        );
        assert_eq!(res.status, 200);
        assert!(String::from_utf8(res.body)
            .unwrap()
            .contains("status=\"ok\""));
        let res = app.handle(
            "POST",
            "/rest/ping",
            "v=1.16.1&c=t&apiKey=bad",
            "apiKey=test-key-1",
        );
        assert_auth_error(res, 44, "Invalid API key.");
        let res = app.handle("POST", "/rest/ping", "v=1.16.1&c=t", "u=test&p=test-pass");
        assert_auth_error(res, 42, "Provided authentication mechanism not supported.");
    }

    #[test]
    fn password_and_token_auth_are_refused() {
        let app = SubsonicApp::new(MemStore::seeded());
        for query in [
            "u=test&p=test-pass&v=1.16.1&c=t",
            "u=test&p=enc:746573742d70617373&v=1.16.1&c=t",
            "u=test&p=enc:zz&v=1.16.1&c=t",
        ] {
            assert_auth_error(
                get(&app, "/rest/ping", query),
                42,
                "Provided authentication mechanism not supported.",
            );
        }
        let salted = crate::auth::md5_hex(b"test-passabc");
        let res = get(
            &app,
            "/rest/ping",
            &format!("u=test&t={salted}&s=abc&v=1.16.1&c=t"),
        );
        assert_auth_error(res, 40, "Wrong username or password.");
        for query in [
            format!("u=test&t={salted}&v=1.16.1&c=t"),
            format!("u=test&t={salted}&s=&v=1.16.1&c=t"),
        ] {
            assert_auth_error(
                get(&app, "/rest/ping", &query),
                10,
                "Required parameter is missing.",
            );
        }
    }
}
