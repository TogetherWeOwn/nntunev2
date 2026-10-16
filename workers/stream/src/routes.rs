//! Fail-closed HTTP routing decisions for the stream Worker.
//!
//! Portable core (no `worker` dependency) so native `cargo test` pins the
//! exact statuses the wasm32-only [`crate::runtime`] adapter renders: the
//! Worker's public fetch path stays `404` until the play adapters are wired,
//! and the pool probe answers `404` off-route, `400` without an owner-named
//! instance, and retryable `503` until provider configuration is wired from
//! the account vault. A status change here (including the `503` → `200`
//! mutant) turns the native suite red long before deploy.

/// Durable Object class name. Must match `class_name` in
/// `workers/stream/wrangler.toml` and the `pub struct` in `src/runtime.rs`
/// (pinned by `do_class_name_matches_wrangler_and_runtime`).
pub const DO_CLASS_NAME: &str = "NntpPool";

/// Probe route: method and path the pool Durable Object serves.
pub const PROBE_METHOD: &str = "POST";
/// Probe route: method and path the pool Durable Object serves.
pub const PROBE_PATH: &str = "/probe";

/// Public Worker fetch decision until the play adapters are wired.
pub const PUBLIC_FETCH_STATUS: u16 = 404;
/// Public Worker fetch decision until the play adapters are wired.
pub const PUBLIC_FETCH_BODY: &str = "not found";

/// Off-route (or wrong-method) pool request.
pub const NOT_FOUND_STATUS: u16 = 404;
/// Off-route (or wrong-method) pool request.
pub const NOT_FOUND_BODY: &str = "not found";
/// Probe on an instance without an owner name (`id_from_name(user_id)`).
pub const OWNER_MISSING_STATUS: u16 = 400;
/// Probe on an instance without an owner name (`id_from_name(user_id)`).
pub const OWNER_MISSING_BODY: &str = "pool owner unavailable";
/// Probe on a named instance before provider wiring lands (retryable).
pub const PROVIDER_UNAVAILABLE_STATUS: u16 = 503;
/// Probe on a named instance before provider wiring lands (retryable).
pub const PROVIDER_UNAVAILABLE_BODY: &str = r#"{"error":"provider_unavailable"}"#;

/// Pool probe outcome for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeDecision {
    /// Wrong method or path: no such route.
    NotFound,
    /// Right route but the instance has no owner name.
    OwnerMissing,
    /// Right route, named instance, provider wiring not yet present.
    ProviderUnavailable,
}

impl ProbeDecision {
    /// HTTP status the adapter must render for this outcome.
    pub fn status(self) -> u16 {
        match self {
            ProbeDecision::NotFound => NOT_FOUND_STATUS,
            ProbeDecision::OwnerMissing => OWNER_MISSING_STATUS,
            ProbeDecision::ProviderUnavailable => PROVIDER_UNAVAILABLE_STATUS,
        }
    }

    /// Response body the adapter must render for this outcome.
    pub fn body(self) -> &'static str {
        match self {
            ProbeDecision::NotFound => NOT_FOUND_BODY,
            ProbeDecision::OwnerMissing => OWNER_MISSING_BODY,
            ProbeDecision::ProviderUnavailable => PROVIDER_UNAVAILABLE_BODY,
        }
    }
}

/// Decide the pool probe outcome from the request method/path and whether
/// the Durable Object instance carries an owner name.
pub fn decide_probe(method: &str, path: &str, has_owner_name: bool) -> ProbeDecision {
    if method != PROBE_METHOD || path != PROBE_PATH {
        return ProbeDecision::NotFound;
    }
    if !has_owner_name {
        return ProbeDecision::OwnerMissing;
    }
    ProbeDecision::ProviderUnavailable
}

/// Decide the public Worker fetch response until the play adapters are wired.
pub fn decide_public_fetch() -> (u16, &'static str) {
    (PUBLIC_FETCH_STATUS, PUBLIC_FETCH_BODY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_fetch_stays_not_found() {
        assert_eq!(decide_public_fetch(), (404, "not found"));
    }

    #[test]
    fn probe_needs_exact_post_and_path() {
        // Off-route and wrong-method requests never reach the owner or
        // provider checks: plain 404 either way.
        for (method, path) in [
            ("GET", "/probe"),
            ("PUT", "/probe"),
            ("POST", "/"),
            ("POST", "/other"),
            ("POST", "/probe/"),
            ("POST", "/PROBE"),
            ("post", "/probe"),
            ("", ""),
        ] {
            let decision = decide_probe(method, path, true);
            assert_eq!(decision, ProbeDecision::NotFound, "{method} {path}");
            assert_eq!(decision.status(), 404, "{method} {path}");
            assert_eq!(decision.body(), "not found", "{method} {path}");
        }
        let decision = decide_probe("POST", "/probe", true);
        assert_eq!(decision, ProbeDecision::ProviderUnavailable);
    }

    #[test]
    fn probe_without_owner_name_is_bad_request() {
        let decision = decide_probe("POST", "/probe", false);
        assert_eq!(decision, ProbeDecision::OwnerMissing);
        assert_eq!(decision.status(), 400);
        assert_eq!(decision.body(), "pool owner unavailable");
    }

    #[test]
    fn probe_with_owner_fails_closed_with_retryable_provider_unavailable() {
        // The executed mutant `503 -> 200` stayed green with no test pinning
        // this: assert the exact fail-closed status and body.
        let decision = decide_probe("POST", "/probe", true);
        assert_eq!(decision, ProbeDecision::ProviderUnavailable);
        assert_eq!(decision.status(), 503);
        assert_ne!(
            decision.status(),
            200,
            "provider wiring is absent: never 200"
        );
        assert_eq!(decision.body(), r#"{"error":"provider_unavailable"}"#);
    }

    #[test]
    fn do_class_name_matches_wrangler_and_runtime() {
        // The fetch entry, the DO binding, and the SQLite migration must all
        // name the same class: the adapter struct, `wrangler.toml`, and this
        // constant drift together or this test fails.
        assert_eq!(DO_CLASS_NAME, "NntpPool");
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let wrangler =
            std::fs::read_to_string(dir.join("wrangler.toml")).expect("read wrangler.toml");
        assert!(
            wrangler.contains("class_name = \"NntpPool\""),
            "wrangler.toml must bind class_name \"NntpPool\":\n{wrangler}"
        );
        assert!(
            wrangler.contains("\"NntpPool\""),
            "wrangler.toml migrations must list \"NntpPool\":\n{wrangler}"
        );
        let runtime =
            std::fs::read_to_string(dir.join("src/runtime.rs")).expect("read src/runtime.rs");
        assert!(
            runtime.contains("pub struct NntpPool"),
            "src/runtime.rs must define `pub struct NntpPool`"
        );
    }
}
