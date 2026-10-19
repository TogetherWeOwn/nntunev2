//! Acceptance tests: preserve request correlation across responses, mail
//! and database diagnostics.
//!
//! Contract (from `docs/logging-standard.md` trace propagation rules):
//! the trace resolved at the api fetch entry is the SAME value that
//! (a) is echoed on every rendered response, including misconfiguration
//! and fail-closed paths, as the `x-trace-id` header;
//! (b) rides the existing mail-webhook call as its `x-trace-id` header;
//! (c) identifies request-owned `PgStore` background connection
//! diagnostics (`api.db.connection_error`) instead of an unrelated
//! fresh root; and
//! (d) accompanies a served-line route rendered under the fixed safe
//! template `/v1/credentials/{id}` for dynamic credential-revoke
//! requests (never the raw credential ID, path or query string), while
//! unknown paths keep the fixed `unknown` sentinel.
//!
//! Invalid/absent inbound IDs keep using the existing safe resolver;
//! raw hostile header contents never appear in output. The trace is
//! never an account-authority or identity value. Six-key envelopes,
//! error codes, cookie/CORS/security headers and normalized
//! `build_sha` are preserved.
//!
//! Seam note: `render_header_pairs`, `internal_render`,
//! `served_display_path`, `PgStore::connect` and `WebhookMailer` are
//! private to the wasm fetch entry (`worker_entry.rs`) and need a
//! Worker `Env`/`Request`, so they have no native-callable surface.
//! Following the established pattern in that module (the build_sha
//! wiring unit test slices the pre-test production source the same
//! way), the wiring criteria below pin the production slice — the
//! test module is cut off first so these literals cannot satisfy
//! themselves — while the resolver/envelope criteria run as real
//! behavior through the public `logging` re-export the entry uses.
//! Wiring pins accept either the `TRACE_HEADER` constant or the
//! `"x-trace-id"` literal so the implementation keeps design freedom.
//!
//! Synthetic fixtures only: no network, mail, database or provider.

use nntune_api::logging::{
    fields_from_json, is_valid_trace_id, parse_build_sha, resolve_trace_id, Level, LogRecord,
    Worker, TRACE_HEADER,
};

const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
const FIXTURE_MILLIS: u64 = 1_792_094_400_123;
/// Fixed safe route template for the dynamic revoke route: the same
/// shape the handlers module already uses in prose for this route.
const REVOKE_TEMPLATE: &str = "/v1/credentials/{id}";

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

fn mentions_trace_header(slice: &str) -> bool {
    slice.contains("TRACE_HEADER") || slice.contains("\"x-trace-id\"")
}

// --- Behavior through the public logging seam the entry uses ----------

#[test]
fn resolver_keeps_valid_inbound_trace_for_echo_and_downstream_use() {
    assert_eq!(TRACE_HEADER, "x-trace-id");
    assert_eq!(
        resolve_trace_id(Some(TRACE)),
        TRACE,
        "a valid inbound trace must survive unchanged so responses, mail \
         and db diagnostics can all carry the originating value"
    );
}

#[test]
fn resolver_replaces_absent_and_hostile_inbound_traces_without_echoing_them() {
    for hostile in [
        None,
        Some(""),
        Some("Bearer hunter2-secret-value"),
        Some("9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C"), // uppercase: invalid
        Some("../../../victim@example.com"),
        Some("9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c\r\nset-cookie: pwned=1"),
    ] {
        let resolved = resolve_trace_id(hostile);
        assert!(
            is_valid_trace_id(&resolved),
            "absent/invalid inbound trace must resolve to a fresh valid ID"
        );
        if let Some(raw) = hostile.filter(|raw| !raw.is_empty()) {
            assert_ne!(resolved, raw, "raw hostile header must never be echoed");
            assert!(
                !resolved.contains(raw),
                "resolved trace must not embed raw header contents"
            );
        }
    }
}

#[test]
fn db_connection_error_record_with_request_trace_keeps_six_key_envelope() {
    // The request-owned diagnostics line the implementation must emit:
    // originating request trace, generic error class, normalized
    // build_sha — and nothing about connection strings, hosts or roles.
    let record = LogRecord::new(
        Worker::Api,
        Level::Error,
        TRACE,
        "api.db.connection_error",
        fields_from_json(serde_json::json!({
            "error_class": "service_unavailable",
            "build_sha": parse_build_sha(Some("0123456789abcdef0123456789abcdef01234567")),
        }))
        .unwrap(),
        FIXTURE_MILLIS,
    )
    .unwrap();
    let line = record.to_json_line();
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    let object = parsed.as_object().unwrap();
    assert_eq!(object.len(), 6, "exactly the six envelope keys");
    assert_eq!(object["trace_id"], TRACE);
    assert_eq!(object["event"], "api.db.connection_error");
    assert_eq!(
        object["fields"]["build_sha"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert!(
        !line.contains("postgres://"),
        "no connection string in line"
    );
    for banned_key in ["\"host\"", "\"role\"", "\"password\""] {
        assert!(
            !line.contains(banned_key),
            "db diagnostics must not carry connection detail key {banned_key}"
        );
    }
}

#[test]
fn served_record_renders_fixed_revoke_template_without_credential_id() {
    // A served line routed through the fixed template renders cleanly,
    // while one embedding an address-shaped dynamic segment is dropped
    // by the shared validator — the template is the safe representation.
    let fields = |route: &str| {
        fields_from_json(serde_json::json!({
            "route": route,
            "status_code": 200,
            "duration_ms": 4,
            "build_sha": "unknown",
        }))
    };
    let ok = LogRecord::new(
        Worker::Api,
        Level::Info,
        TRACE,
        "api.request.served",
        fields(&format!("DELETE {REVOKE_TEMPLATE}")).unwrap(),
        FIXTURE_MILLIS,
    )
    .unwrap();
    let line = ok.to_json_line();
    assert!(line.contains(REVOKE_TEMPLATE));
    assert!(
        fields("DELETE /v1/credentials/victim@example.com?token=abc").is_err()
            || LogRecord::new(
                Worker::Api,
                Level::Info,
                TRACE,
                "api.request.served",
                fields("DELETE /v1/credentials/victim@example.com?token=abc").unwrap(),
                FIXTURE_MILLIS,
            )
            .is_err(),
        "a raw dynamic path carrying an address/query must never render"
    );
}

// --- Wiring: response echo --------------------------------------------

#[test]
fn rendered_responses_echo_the_resolved_trace_header() {
    let source = production_source();
    let pairs = slice_from(source, "fn render_header_pairs(");
    let render = slice_from(source, "fn render(");
    assert!(
        mentions_trace_header(&pairs) || mentions_trace_header(&render),
        "response rendering must add the resolved request trace as the \
         x-trace-id header (render_header_pairs/render never mention it)"
    );
    let dispatch = slice_from(source, "pub async fn dispatch(");
    assert!(
        dispatch.contains("trace_id"),
        "dispatch must thread its resolved trace_id into rendering"
    );
}

#[test]
fn fail_closed_and_misconfigured_responses_also_carry_the_trace() {
    let source = production_source();
    let internal = slice_from(source, "fn internal_render(");
    assert!(
        internal.contains("trace") || mentions_trace_header(&internal),
        "the generic 500 (misconfiguration / fail-closed) render path must \
         receive and echo the resolved trace, not render trace-free"
    );
}

#[test]
fn rendering_preserves_existing_security_and_cors_headers() {
    // Control: passes today and must still pass after the trace echo is
    // added — the echo is additive, never a replacement.
    let source = production_source();
    let pairs = slice_from(source, "fn render_header_pairs(");
    for literal in [
        "application/json; charset=utf-8",
        "default-src 'none'; frame-ancestors 'none'",
        "\"cache-control\"",
        "\"x-content-type-options\"",
        "set-cookie",
    ] {
        assert!(
            pairs.contains(literal),
            "header {literal} must be preserved"
        );
    }
    let dispatch = slice_from(source, "pub async fn dispatch(");
    assert!(
        dispatch.contains("cors_headers("),
        "the tested CORS decision must still feed rendering"
    );
    assert!(
        dispatch.matches("\"build_sha\": build_sha").count() >= 2,
        "served and misconfigured lines must keep normalized build_sha"
    );
}

// --- Wiring: mail webhook ---------------------------------------------

#[test]
fn mail_webhook_call_carries_the_request_trace_header() {
    let source = production_source();
    let mailer = slice_from(source, "impl Mailer for WebhookMailer");
    assert!(
        mentions_trace_header(&mailer),
        "WebhookMailer::send_magic_link must set the downstream \
         x-trace-id header to the originating request trace"
    );
    let dispatch = slice_from(source, "pub async fn dispatch(");
    let mailer_ctor = dispatch
        .split_once("WebhookMailer {")
        .expect("dispatch must construct the WebhookMailer")
        .1;
    let ctor_head = &mailer_ctor[..mailer_ctor.len().min(300)];
    assert!(
        ctor_head.contains("trace"),
        "dispatch must hand its resolved trace to the WebhookMailer it \
         constructs for the request"
    );
}

// --- Wiring: request-owned database diagnostics ------------------------

#[test]
fn request_owned_pgstore_diagnostics_use_the_originating_trace_not_a_fresh_root() {
    let source = production_source();
    let store_impl = slice_from(source, "impl PgStore");
    assert!(
        store_impl.contains("api.db.connection_error"),
        "PgStore::connect must keep emitting api.db.connection_error"
    );
    assert!(
        !store_impl.contains("resolve_trace_id(None)"),
        "request-owned PgStore diagnostics must not root an unrelated \
         fresh trace via resolve_trace_id(None); they must carry the \
         originating request trace (a context-free background root, if \
         any remains, must not be this request path)"
    );
    let store_struct = slice_from(source, "struct PgStore");
    assert!(
        store_struct.contains("trace") || store_impl.contains("self.trace"),
        "PgStore must carry the request trace it was constructed with"
    );
    let dispatch = slice_from(source, "pub async fn dispatch(");
    let store_ctor = dispatch
        .split_once("PgStore {")
        .expect("dispatch must construct the PgStore")
        .1;
    let ctor_head = &store_ctor[..store_ctor.len().min(300)];
    assert!(
        ctor_head.contains("trace"),
        "dispatch must construct PgStore with its resolved request trace"
    );
}

// --- Wiring: safe route template ---------------------------------------

#[test]
fn served_display_path_uses_fixed_revoke_template_and_keeps_unknown_sentinel() {
    let source = production_source();
    let display = slice_from(source, "fn served_display_path(");
    assert!(
        display.contains(REVOKE_TEMPLATE),
        "dynamic credential-revoke requests must render under the fixed \
         safe template {REVOKE_TEMPLATE} (excluding credential IDs, raw \
         paths and query strings), not collapse to unknown"
    );
    assert!(
        display.contains("/v1/credentials/")
            && (display.contains("contains('/')")
                || display.contains("contains(\"/\")")
                || display.contains("split('/')")),
        "the template must apply only to the single-segment revoke shape; \
         deeper dynamic paths must not be templated"
    );
    assert!(
        display.contains("\"unknown\""),
        "unknown paths must remain the fixed unknown sentinel"
    );
}
