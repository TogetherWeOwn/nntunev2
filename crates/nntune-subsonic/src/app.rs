//! Request dispatch: path → auth → endpoint → wire bytes.
//!
//! `SubsonicApp<S: Library>` is transport-free so the same code serves the
//! Cloudflare worker shell, native tests, and the golden fixture suite.
//! Mutations go through `RefCell` so `handle` stays `&self`.

use crate::auth::{authenticate, Credentials};
use crate::endpoints::{handle_endpoint, EndpointResult, Params};
use crate::envelope::{
    error_document, ok_document, render_json, render_jsonp, render_xml, JSON_CONTENT_TYPE,
    XML_CONTENT_TYPE,
};
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
    /// string without `?`.
    pub fn handle(&self, _method: &str, path: &str, query: &str) -> HttpResponse {
        match route(path) {
            Route::Unknown => HttpResponse::document(404, "text/plain", b"Not Found".to_vec()),
            Route::Hls => HttpResponse::document(410, "text/plain", b"Gone".to_vec()),
            Route::Unsupported(op) => {
                let params = Params::parse(query);
                let doc = error_document(
                    crate::envelope::code::GENERIC,
                    &format!("The {op} operation is not supported by this server."),
                );
                self.render(&params, &doc)
            }
            Route::Endpoint(name) => {
                let params = Params::parse(query);
                let get = |key: &str| params.get(key).map(|s| s.to_string());
                let creds = Credentials::from_params(get);
                let mut store = self.store.borrow_mut();
                let identity = match authenticate(&*store, &creds) {
                    Ok(identity) => identity,
                    Err((error_code, message)) => {
                        return self.render(&params, &error_document(error_code, &message));
                    }
                };
                match handle_endpoint(&mut *store, &identity, name, &params) {
                    EndpointResult::Doc(payload) => self.render(&params, &ok_document(payload)),
                    EndpointResult::Err(error_code, message) => {
                        self.render(&params, &error_document(error_code, &message))
                    }
                    EndpointResult::Binary { content_type, body } => {
                        HttpResponse::document(200, &content_type, body)
                    }
                }
            }
        }
    }

    fn render(&self, params: &Params, doc: &crate::value::Value) -> HttpResponse {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::MemStore;

    fn get(app: &SubsonicApp<MemStore>, path: &str, query: &str) -> HttpResponse {
        app.handle("GET", path, query)
    }

    #[test]
    fn post_behaves_like_get() {
        let app = SubsonicApp::new(MemStore::seeded());
        let key = "apiKey=test-key-1&v=1.16.1&c=t";
        assert_eq!(
            app.handle("POST", "/rest/ping", key),
            get(&app, "/rest/ping", key)
        );
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
