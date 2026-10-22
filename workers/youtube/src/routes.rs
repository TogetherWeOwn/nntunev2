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
        // test fails. Substring pins (no syn/quote dev-deps for the spike
        // slice); the wasm check + clippy gate the adapter shape.
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
}
