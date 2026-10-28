//! Artifact validation wiring: the runtime validates the Container
//! result before fetching, verifies fetched bytes before persisting,
//! and skips the fetch entirely for terminal `untranscodable`.
//!
//! `runtime` compiles only for `wasm32`, so these pins read its
//! source (normalised) instead of calling it.

const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

fn normalized() -> String {
    RUNTIME_RS.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Source slice of one `async fn` (up to the next `async fn`).
fn fn_slice(name: &str) -> String {
    let text = normalized();
    let start = text
        .find(&format!("async fn {name}"))
        .unwrap_or_else(|| panic!("runtime.rs has no async fn {name}"));
    let rest = &text[start + 10..];
    let end = rest.find("async fn ").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn run_validates_result_shape_before_fetching() {
    let run = fn_slice("run");
    let validate = run
        .find("validate_transcoded_result(&result)")
        .expect("run must call validate_transcoded_result");
    let fetch = run
        .find("fetch_artifact(")
        .expect("run must call fetch_artifact");
    assert!(
        validate < fetch,
        "a mis-shaped Container result must reject before any byte is fetched"
    );
}

#[test]
fn run_skips_fetch_for_untranscodable() {
    let run = fn_slice("run");
    let guard = run
        .find("if result.status == TranscodeStatus::Transcoded")
        .expect("run must guard the fetch on Transcoded status");
    let fetch = run.find("fetch_artifact(").expect("run must fetch");
    assert!(
        guard < fetch,
        "only a Transcoded result may reach fetch_artifact"
    );
}

#[test]
fn fetch_verifies_bytes_before_persisting() {
    let fetch = fn_slice("fetch_artifact");
    let verify = fetch
        .find("verify_artifact(&bytes, expected_sha, expected_size)")
        .expect("fetch_artifact must call verify_artifact");
    let put = fetch.find(".put(").expect("fetch_artifact must R2-put");
    assert!(verify < put, "fetched bytes must verify before the R2 put");
}
