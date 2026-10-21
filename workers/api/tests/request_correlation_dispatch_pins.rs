//! Builder-owned dispatch and use-site pins for request correlation.
//!
//! Companion to `request_correlation_acceptance.rs` (Test-Author owned,
//! intentionally unchanged): the acceptance wiring pins check that the
//! substring `trace` appears after each dispatch-site constructor, which
//! a fresh-root expression also satisfies. These pins assert the exact
//! threaded value at every dispatch call site AND at each internal hop
//! that forwards the trace downstream (mailer use site, render,
//! internal_render, PgStore connect), so re-rooting any one of them
//! turns this suite red.
//!
//! Same seam note as the acceptance file: `dispatch`, `send_magic_link`
//! and `connect` need a Worker `Env`/`Request`, so the pins slice the
//! pre-test production source (the test module is cut off first so
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

/// The `Name { ... }` constructor inside dispatch, bounded at its own
/// closing brace: a fixed byte window would bleed into the neighbouring
/// constructor and let one site satisfy the other's pin.
fn ctor_head(dispatch: &str, name: &str) -> String {
    let marker = format!("{name} {{");
    let ctor = dispatch
        .split_once(&marker)
        .unwrap_or_else(|| panic!("dispatch must construct the {name}"))
        .1;
    let end = ctor
        .find("\n        },")
        .unwrap_or_else(|| panic!("the {name} constructor must have a closing brace"));
    ctor[..end].to_string()
}

/// The body of a top-level `fn` (closing brace at column 0).
fn top_level_fn_body(source: &str, signature: &str) -> String {
    let rest = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("production source must contain `{signature}`"))
        .1;
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("`{signature}` must have a closing brace"));
    rest[..end].to_string()
}

/// The body of a method inside an `impl` block (closing brace indented
/// four spaces; nested blocks sit deeper, so the first match ends the
/// method).
fn method_body(source: &str, signature: &str) -> String {
    let rest = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("production source must contain `{signature}`"))
        .1;
    let end = rest
        .find("\n    }\n")
        .unwrap_or_else(|| panic!("`{signature}` must have a closing brace"));
    rest[..end].to_string()
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
fn mailer_forwards_its_stored_trace_to_the_webhook() {
    let body = method_body(production_source(), "async fn send_magic_link(");
    assert!(
        body.contains("(crate::logging::TRACE_HEADER, &self.trace_id)"),
        "send_magic_link must forward the mailer's stored request trace \
         (`(crate::logging::TRACE_HEADER, &self.trace_id)`), not a fresh root"
    );
    assert!(
        !body.contains("resolve_trace_id"),
        "send_magic_link must reuse the stored trace, never resolve a fresh one"
    );
}

#[test]
fn render_forwards_the_trace_to_header_pairs() {
    let body = top_level_fn_body(production_source(), "fn render(");
    assert!(
        body.contains("render_header_pairs(response, cors, trace_id)"),
        "render must forward its trace argument to render_header_pairs \
         (`render_header_pairs(response, cors, trace_id)`)"
    );
    assert!(
        !body.contains("resolve_trace_id"),
        "render must forward the given trace, never resolve a fresh one"
    );
}

#[test]
fn internal_render_forwards_the_trace_to_render() {
    let body = top_level_fn_body(production_source(), "fn internal_render(");
    assert!(
        body.contains("render(&internal_response(), &[], trace_id)"),
        "internal_render must forward its trace argument to render \
         (`render(&internal_response(), &[], trace_id)`)"
    );
    assert!(
        !body.contains("resolve_trace_id"),
        "internal_render must forward the given trace, never resolve a fresh one"
    );
}

#[test]
fn pgstore_connect_hands_the_request_trace_to_connection_diagnostic() {
    let body = method_body(production_source(), "async fn connect(&self)");
    assert!(
        body.contains("let trace_id = self.trace_id.clone();"),
        "connect must own the store's request trace for the driver task \
         (`let trace_id = self.trace_id.clone();`), not a fresh root"
    );
    assert!(
        body.contains("Self::connection_diagnostic(connection, &trace_id, &build_sha)"),
        "connect must hand the owned request trace to connection_diagnostic \
         (`Self::connection_diagnostic(connection, &trace_id, &build_sha)`)"
    );
    assert!(
        !body.contains("resolve_trace_id"),
        "connect must reuse the stored trace, never resolve a fresh one"
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
