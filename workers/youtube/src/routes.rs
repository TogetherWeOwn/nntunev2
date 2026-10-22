//! Fail-closed HTTP routing decisions for the YouTube measurement Worker.
//!
//! Portable core (no `worker` dependency) so native `cargo test` pins the
//! exact statuses the wasm32-only [`crate::runtime`] adapter renders: the
//! Worker's public fetch path stays `404` until the measurement routes
//! land, and the extraction Durable Object answers `404` off-route and
//! retryable `503` until the `/player` extraction wiring lands. A status
//! change here (including the `503` → `200` mutant) turns the native suite
//! red long before deploy.

/// Durable Object class name. Must match `class_name` in
/// `workers/youtube/wrangler.toml.example` and the `pub struct` in
/// `src/runtime.rs` (pinned by
/// `do_class_name_matches_wrangler_template_and_runtime`).
pub const DO_CLASS_NAME: &str = "YtExtract";

/// Extraction route: method and path the `YtExtract` Durable Object serves.
pub const EXTRACT_METHOD: &str = "POST";
/// Extraction route: method and path the `YtExtract` Durable Object serves.
pub const EXTRACT_PATH: &str = "/extract";

/// Public Worker fetch decision until the measurement routes are wired.
pub const PUBLIC_FETCH_STATUS: u16 = 404;
/// Public Worker fetch decision until the measurement routes are wired.
pub const PUBLIC_FETCH_BODY: &str = "not found";

/// Off-route (or wrong-method) extraction request.
pub const NOT_FOUND_STATUS: u16 = 404;
/// Off-route (or wrong-method) extraction request.
pub const NOT_FOUND_BODY: &str = "not found";
/// Right-route request before the `/player` extraction wiring lands
/// (retryable).
pub const EXTRACTION_UNAVAILABLE_STATUS: u16 = 503;
/// Right-route request before the `/player` extraction wiring lands
/// (retryable).
pub const EXTRACTION_UNAVAILABLE_BODY: &str = r#"{"error":"extraction_unavailable"}"#;

/// Extraction outcome for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractDecision {
    /// Wrong method or path: no such route.
    NotFound,
    /// Right route, extraction wiring not yet present.
    ExtractionUnavailable,
}

impl ExtractDecision {
    /// HTTP status the adapter must render for this outcome.
    pub fn status(self) -> u16 {
        match self {
            ExtractDecision::NotFound => NOT_FOUND_STATUS,
            ExtractDecision::ExtractionUnavailable => EXTRACTION_UNAVAILABLE_STATUS,
        }
    }

    /// Response body the adapter must render for this outcome.
    pub fn body(self) -> &'static str {
        match self {
            ExtractDecision::NotFound => NOT_FOUND_BODY,
            ExtractDecision::ExtractionUnavailable => EXTRACTION_UNAVAILABLE_BODY,
        }
    }
}

/// Decide the extraction outcome from the request method and path: exact
/// `POST /extract` only, everything else is not found.
pub fn decide_extract(method: &str, path: &str) -> ExtractDecision {
    if method != EXTRACT_METHOD || path != EXTRACT_PATH {
        return ExtractDecision::NotFound;
    }
    ExtractDecision::ExtractionUnavailable
}

/// Decide the public Worker fetch response until the measurement routes are
/// wired.
pub fn decide_public_fetch() -> (u16, &'static str) {
    (PUBLIC_FETCH_STATUS, PUBLIC_FETCH_BODY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_line(text: &str, line: &str) -> bool {
        text.lines().any(|candidate| candidate.trim() == line)
    }

    fn non_doc_attributes(attrs: &[syn::Attribute]) -> Vec<String> {
        attrs
            .iter()
            .filter(|attribute| !attribute.path().is_ident("doc"))
            .map(tokens)
            .collect()
    }

    fn tokens<T: quote::ToTokens>(value: &T) -> String {
        value.to_token_stream().to_string()
    }

    #[test]
    fn public_fetch_stays_not_found() {
        assert_eq!(decide_public_fetch(), (404, "not found"));
    }

    #[test]
    fn extract_needs_exact_post_and_path() {
        // Off-route and wrong-method requests never reach extraction:
        // plain 404 either way.
        for (method, path) in [
            ("GET", "/extract"),
            ("PUT", "/extract"),
            ("POST", "/"),
            ("POST", "/other"),
            ("POST", "/extract/"),
            ("POST", "/EXTRACT"),
            ("post", "/extract"),
            ("", ""),
        ] {
            let decision = decide_extract(method, path);
            assert_eq!(decision, ExtractDecision::NotFound, "{method} {path}");
            assert_eq!(decision.status(), 404, "{method} {path}");
            assert_eq!(decision.body(), "not found", "{method} {path}");
        }
        let decision = decide_extract("POST", "/extract");
        assert_eq!(decision, ExtractDecision::ExtractionUnavailable);
    }

    #[test]
    fn extract_on_route_fails_closed_with_retryable_extraction_unavailable() {
        // The executed mutant `503 -> 200` would stay green with no test
        // pinning this: assert the exact fail-closed status and body.
        let decision = decide_extract("POST", "/extract");
        assert_eq!(decision, ExtractDecision::ExtractionUnavailable);
        assert_eq!(decision.status(), 503);
        assert_ne!(
            decision.status(),
            200,
            "extraction wiring is absent: never 200"
        );
        assert_eq!(decision.body(), r#"{"error":"extraction_unavailable"}"#);
    }

    #[test]
    fn do_class_name_matches_wrangler_template_and_runtime() {
        // The fetch entry, the DO binding, and the SQLite migration must all
        // name the same class: the adapter struct,
        // `wrangler.toml.example`, and this constant drift together or this
        // test fails. The adapter shape itself is AST-pinned by
        // `wasm_adapter_renders_portable_statuses` below.
        assert_eq!(DO_CLASS_NAME, "YtExtract");
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let wrangler = std::fs::read_to_string(dir.join("wrangler.toml.example"))
            .expect("read wrangler.toml.example");
        assert!(
            has_line(&wrangler, r#"class_name = "YtExtract""#),
            "wrangler.toml.example must bind class_name \"YtExtract\":\n{wrangler}"
        );
        assert!(
            has_line(&wrangler, r#"new_sqlite_classes = ["YtExtract"]"#),
            "wrangler.toml.example must migrate \"YtExtract\" as a SQLite class:\n{wrangler}"
        );
        let runtime =
            std::fs::read_to_string(dir.join("src/runtime.rs")).expect("read src/runtime.rs");
        assert!(
            has_line(&runtime, "#[durable_object(fetch)]"),
            "runtime.rs must bind the Durable Object fetch handler:\n{runtime}"
        );
        assert!(
            has_line(&runtime, "pub struct YtExtract;"),
            "runtime.rs must declare the YtExtract struct:\n{runtime}"
        );
        assert!(
            runtime.contains("routes::decide_extract"),
            "runtime.rs must render the portable extract decision:\n{runtime}"
        );
        assert!(
            runtime.contains("routes::decide_public_fetch"),
            "runtime.rs must render the portable public-fetch decision:\n{runtime}"
        );
    }

    #[test]
    fn wasm_adapter_renders_portable_statuses() {
        // The wasm32-only adapter (src/runtime.rs) is never natively linked,
        // so removing `.with_status(decision.status())` (Response::ok
        // defaults to 200) or swapping the NotFound / ExtractionUnavailable
        // arms stays green on the portable tests above. Pin the adapter's
        // token shape here, mirroring the stream worker's
        // `wasm_adapter_uses_dispatch_and_preserves_worker_status_rendering`:
        // both mutants must turn this test red.
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let runtime = std::fs::read_to_string(dir.join("src/runtime.rs")).expect("read runtime.rs");
        let parsed = syn::parse_file(&runtime).expect("parse runtime.rs");
        assert!(
            non_doc_attributes(&parsed.attrs).is_empty(),
            "runtime.rs must not have inner attributes that can disable its Worker exports"
        );

        let lib = std::fs::read_to_string(dir.join("src/lib.rs")).expect("read lib.rs");
        let parsed_lib = syn::parse_file(&lib).expect("parse lib.rs");
        let runtime_module = parsed_lib
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Mod(module) if module.ident == "runtime" => Some(module),
                _ => None,
            })
            .expect("runtime module declaration");
        let wasm_cfg: syn::Attribute = syn::parse_quote!(#[cfg(target_arch = "wasm32")]);
        assert_eq!(
            non_doc_attributes(&runtime_module.attrs),
            vec![tokens(&wasm_cfg)],
            "runtime module must be enabled only by the wasm32 target cfg"
        );

        let public_fetch = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == "fetch" => Some(function),
                _ => None,
            })
            .expect("public fetch handler");
        let expected_public_fetch = syn::parse_str::<syn::ItemFn>(
            "async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {\n                let (status, body) = routes::decide_public_fetch();\n                Response::error(body, status)\n            }",
        )
        .expect("parse expected public fetch handler");
        assert_eq!(
            tokens(&public_fetch.sig),
            tokens(&expected_public_fetch.sig)
        );
        assert_eq!(
            tokens(&public_fetch.block),
            tokens(&expected_public_fetch.block)
        );
        let event_attribute: syn::Attribute = syn::parse_quote!(#[event(fetch)]);
        assert_eq!(
            non_doc_attributes(&public_fetch.attrs),
            vec![tokens(&event_attribute)],
            "public fetch must have exactly its Worker event binding and no cfg/other attributes"
        );

        let durable_object_fetch = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Impl(implementation)
                    if implementation.trait_.as_ref().is_some_and(|(path, _)| {
                        path.segments
                            .last()
                            .is_some_and(|s| s.ident == "DurableObject")
                    }) =>
                {
                    implementation.items.iter().find_map(|item| match item {
                        syn::ImplItem::Fn(function) if function.sig.ident == "fetch" => {
                            Some(function)
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("YtExtract Durable Object fetch handler");
        assert!(
            non_doc_attributes(&durable_object_fetch.attrs).is_empty(),
            "Durable Object fetch handler must not carry cfg/other attributes"
        );
        let expected_durable_object_fetch = syn::parse_str::<syn::ImplItemFn>(
            "async fn fetch(&self, req: Request) -> Result<Response> {\n                let decision = routes::decide_extract(req.method().as_ref(), &req.path());\n                match decision {\n                    ExtractDecision::NotFound => Response::error(ExtractDecision::NotFound.body(), ExtractDecision::NotFound.status(),),\n                    ExtractDecision::ExtractionUnavailable => respond_unavailable(),\n                }\n            }",
        )
        .expect("parse expected Durable Object handler");
        assert_eq!(
            tokens(&durable_object_fetch.sig),
            tokens(&expected_durable_object_fetch.sig)
        );
        assert_eq!(
            tokens(&durable_object_fetch.block),
            tokens(&expected_durable_object_fetch.block),
            "Durable Object fetch must keep each decision on its intended response path"
        );

        let durable_object = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Struct(item) if item.ident == "YtExtract" => Some(item),
                _ => None,
            })
            .expect("YtExtract Durable Object");
        let fetch_attribute: syn::Attribute = syn::parse_quote!(#[durable_object(fetch)]);
        assert_eq!(
            non_doc_attributes(&durable_object.attrs),
            vec![tokens(&fetch_attribute)],
            "YtExtract must have exactly its Durable Object fetch binding"
        );

        let respond_unavailable = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == "respond_unavailable" => {
                    Some(function)
                }
                _ => None,
            })
            .expect("respond_unavailable function");
        assert!(
            non_doc_attributes(&respond_unavailable.attrs).is_empty(),
            "respond_unavailable must not carry cfg/other attributes"
        );
        let expected_respond_unavailable = syn::parse_str::<syn::ItemFn>(
            "fn respond_unavailable() -> Result<Response> {\n                let decision = ExtractDecision::ExtractionUnavailable;\n                let headers = Headers::new();\n                headers.set(\"content-type\", \"application/json\")?;\n                Ok(Response::ok(decision.body())?\n                    .with_status(decision.status())\n                    .with_headers(headers))\n            }",
        )
        .expect("parse expected respond_unavailable");
        assert_eq!(
            tokens(&respond_unavailable.sig),
            tokens(&expected_respond_unavailable.sig)
        );
        assert_eq!(
            tokens(&respond_unavailable.block),
            tokens(&expected_respond_unavailable.block),
            "respond_unavailable must preserve the portable 503 status"
        );
    }
}
