//! Staging PUT replies are cancelled, not collected: the 201 reply
//! carries no verdict beyond its status, so buffering its body would
//! let a misbehaving Container stall staging on discarded bytes.
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
fn put_reply_body_is_cancelled_after_status() {
    let stage = fn_slice("stage_input");
    let status = stage
        .find("response.status_code()")
        .expect("stage_input must read the PUT status");
    let cancel = stage
        .find("stream.cancel()")
        .expect("stage_input must cancel the PUT reply body");
    assert!(
        status < cancel,
        "the PUT status must be read before its body is cancelled"
    );
    assert!(
        !stage.contains("read_response_capped"),
        "the PUT reply body must never be collected"
    );
}
