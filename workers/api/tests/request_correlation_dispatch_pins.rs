//! Builder-owned dispatch pins for request correlation.
//!
//! Companion to `request_correlation_acceptance.rs` (Test-Author owned,
//! intentionally unchanged): the acceptance wiring pins check that the
//! substring `trace` appears after each dispatch-site constructor, which
//! a fresh-root expression also satisfies. These pins assert the exact
//! threaded value at every dispatch call site instead, so re-rooting any
//! one of them turns this suite red.
//!
//! Same seam note as the acceptance file: `dispatch` is private to the
//! wasm fetch entry and needs a Worker `Env`/`Request`, so the pins slice
//! the pre-test production source (the test module is cut off first so
//! these literals cannot satisfy themselves).
//!
//! Synthetic fixtures only: no network, mail, database or provider.

fn production_source() -> &'static str {
    let source = include_str!("../src/worker_entry.rs");
    source
        .split_once("#[cfg(test)]")
        .expect("worker_entry.rs must keep a test module after production code")
        .0
}

/// Slice from `start` up to the next top-level item boundary.
fn slice_from(source: &str, start: &str) -> String {
    let rest = source
        .split_once(start)
        .unwrap_or_else(|| panic!("production source must contain `{start}`"))
        .1;
    let end = [
        "\nfn ",
        "\nimpl ",
        "\nstruct ",
        "\npub async fn ",
        "\npub fn ",
    ]
    .iter()
    .filter_map(|marker| rest.find(marker))
    .min()
    .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// The head of the `Name { ... }` constructor inside dispatch: the first
/// 300 bytes after the opening brace, matching the acceptance window.
fn ctor_head(dispatch: &str, name: &str) -> String {
    let marker = format!("{name} {{");
    let ctor = dispatch
        .split_once(&marker)
        .unwrap_or_else(|| panic!("dispatch must construct the {name}"))
        .1;
    ctor[..ctor.len().min(300)].to_string()
}

#[test]
fn dispatch_resolves_the_request_trace_exactly_once() {
    let dispatch = slice_from(production_source(), "pub async fn dispatch(");
    assert!(
        dispatch.contains("resolve_trace_id(incoming_trace.as_deref())"),
        "dispatch must resolve the request trace from the inbound header once"
    );
    assert_eq!(
        dispatch.matches("resolve_trace_id").count(),
        1,
        "dispatch must not root a second trace at any call site; every \
         constructor and render call below must reuse the one resolved value"
    );
}

#[test]
fn dispatch_threads_the_same_trace_into_pgstore() {
    let dispatch = slice_from(production_source(), "pub async fn dispatch(");
    let head = ctor_head(&dispatch, "PgStore");
    assert!(
        head.contains("trace_id: trace_id.clone()"),
        "dispatch must construct PgStore with its resolved request trace \
         (`trace_id: trace_id.clone()`), not a fresh root"
    );
    assert!(
        !head.contains("resolve_trace_id"),
        "the PgStore constructor must reuse the resolved trace, never \
         resolve a fresh one"
    );
}

#[test]
fn dispatch_threads_the_same_trace_into_webhook_mailer() {
    let dispatch = slice_from(production_source(), "pub async fn dispatch(");
    let head = ctor_head(&dispatch, "WebhookMailer");
    assert!(
        head.contains("trace_id: trace_id.clone()"),
        "dispatch must hand its resolved trace to the WebhookMailer it \
         constructs (`trace_id: trace_id.clone()`), not a fresh root"
    );
    assert!(
        !head.contains("resolve_trace_id"),
        "the WebhookMailer constructor must reuse the resolved trace, never \
         resolve a fresh one"
    );
}

#[test]
fn dispatch_threads_the_same_trace_into_response_rendering() {
    let dispatch = slice_from(production_source(), "pub async fn dispatch(");
    assert!(
        dispatch.contains("render(&response, &cors, &trace_id)"),
        "the served path must render with the resolved request trace \
         (`render(&response, &cors, &trace_id)`)"
    );
    assert!(
        dispatch.contains("internal_render(&trace_id)"),
        "the misconfigured fail-closed path must render with the resolved \
         request trace (`internal_render(&trace_id)`)"
    );
}
