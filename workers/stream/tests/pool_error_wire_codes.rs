//! Pin for the pool error-wire codes (`docs/api-errors.md`).
//!
//! The stream `NntpPool` serves ingest on [`Lane::Ingest`] with legacy
//! `{"error": <code>}` bodies, not the `{code, message, retryable}` envelope:
//! five [`ProbeError`] codes (`workers/stream/src/probe.rs:109-117`), plus
//! `missing` (`workers/stream/src/body.rs:85-90`) and `internal`
//! (`workers/stream/src/probe.rs:148-152`). No prod-code change on this card:
//! this test reads the pool sources as text via `include_str!` (no new crate
//! deps, so the stream dependency-free constraint is untouched) and asserts
//! the served wire-code set is a subset of the documented pool table, with
//! matching status and retryable flags.
//!
//! Docs table lives under `## Pool error-wire codes` in
//! `docs/api-errors.md`. Its rows deliberately do NOT start with `| \`` so
//! the envelope-registry doc-lint tests (`registry_matches_docs_api_errors`
//! in each `api_error` module and `docs_registry_matches_all_worker_registries`
//! in `workers/mb/tests/api_errors_cross_worker.rs`) keep ignoring them.

use nntune_stream::body::BodyReply;
use nntune_stream::probe::ProbeError;

// Pool sources, read as text so a new served code fails here until docs grow
// a matching row. `routes.rs`/`runtime.rs` reuse the same codes (owner-less
// `bad_request`, pre-wiring `provider_unavailable`, capped-read `too_large`,
// unreadable-body `bad_request`); they contribute no new wire code.
const PROBE_RS: &str = include_str!("../src/probe.rs");
const BODY_RS: &str = include_str!("../src/body.rs");
const ROUTES_RS: &str = include_str!("../src/routes.rs");
const RUNTIME_RS: &str = include_str!("../src/runtime.rs");
// Ingest verdicts, read as text (no crate dep): `retryable` in EXPECTED must
// stay true to these arms. A mapping flip here fails below until docs follow.
const INGEST_PROBE_RS: &str = include_str!("../../ingest/src/pool_probe.rs");
const INGEST_BODY_RS: &str = include_str!("../../ingest/src/pool_body.rs");
const DOCS: &str = include_str!("../../../docs/api-errors.md");

/// Expected served wire codes: (code, HTTP status, retryable).
///
/// Retryable mirrors what ingest does with the answer (`workers/ingest/src/pool_probe.rs`
/// `classify_reply`: 400/413 are `Rejected`, everything else is `Retry`;
/// `workers/ingest/src/pool_body.rs` `classify_body_reply`: exact
/// `{"error":"missing"}` 404 is `Missing` data, 413 is `TooLarge` data,
/// 400 is `Rejected`, everything else is `Retry`):
/// 4xx request/data answers are `false`, 5xx redeliverable answers `true`.
/// All serve on `Lane::Ingest`.
const EXPECTED: &[(&str, u16, bool)] = &[
    ("bad_request", 400, false),
    ("too_large", 413, false),
    ("busy", 503, true),
    ("provider_unavailable", 503, true),
    ("provider_error", 502, true),
    ("missing", 404, false),
    ("internal", 500, true),
];

/// One docs pool-table row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolRow {
    code: String,
    status: u16,
    retryable: bool,
    lane: String,
}

/// Parse the `## Pool error-wire codes` section. Rows are
/// `| code | status | retryable | lane | ... |`, deliberately without
/// backticks so the envelope-registry parsers (`| \`` prefix) skip them.
fn parse_pool_table(doc: &str) -> Vec<PoolRow> {
    const HEADER: &str = "## Pool error-wire codes";
    let start = doc
        .find(HEADER)
        .unwrap_or_else(|| panic!("docs/api-errors.md gains a `{HEADER}` section"));
    let rest = &doc[start..];
    // Section ends at the next top-level section.
    let end = rest[HEADER.len()..]
        .find("\n## ")
        .map(|i| start + HEADER.len() + i)
        .unwrap_or(doc.len());
    let section = &doc[start..end];
    let mut rows = Vec::new();
    for line in section.lines().filter(|l| l.starts_with('|')) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        // Cells: ["", code, status, retryable, lane, meaning, served-by, ""].
        if cells.len() < 6 {
            continue;
        }
        if cells[1] == "code" || cells[1].starts_with("---") {
            continue;
        }
        let code = cells[1].to_string();
        // Skip any envelope-registry row that ever lands in this section by
        // mistake (backticked code): the pool table uses bare codes.
        if code.starts_with('`') {
            continue;
        }
        let status: u16 = cells[2]
            .parse()
            .unwrap_or_else(|_| panic!("pool table status parses for {code}"));
        let retryable: bool = cells[3]
            .parse()
            .unwrap_or_else(|_| panic!("pool table retryable parses for {code}"));
        let lane = cells[4].to_string();
        rows.push(PoolRow {
            code,
            status,
            retryable,
            lane,
        });
    }
    rows
}

/// Served codes from the runtime mappings themselves (plus `internal`, whose
/// serialization-failure path is pinned by source presence below).
fn served_from_runtime() -> Vec<(&'static str, u16, bool)> {
    let busy = ProbeError::Busy {
        retry_after_ms: 5_000,
    };
    let cases: Vec<(&'static str, u16, bool)> = vec![
        (
            ProbeError::BadRequest("x").code(),
            ProbeError::BadRequest("x").status(),
            false,
        ),
        (
            ProbeError::TooLarge.code(),
            ProbeError::TooLarge.status(),
            false,
        ),
        (busy.code(), busy.status(), true),
        (
            ProbeError::ProviderUnavailable.code(),
            ProbeError::ProviderUnavailable.status(),
            true,
        ),
        (
            ProbeError::Provider.code(),
            ProbeError::Provider.status(),
            true,
        ),
    ];
    // `missing` is served by the body path, not `ProbeError::code()`.
    let missing = BodyReply::from(Ok(nntune_stream::body::BodyOutcome::Missing));
    assert_eq!(missing.status, 404);
    assert_eq!(missing.body, br#"{"error":"missing"}"#.to_vec());
    let mut served = cases;
    served.push(("missing", missing.status, false));
    // `internal` is the `ProbeOutcome` serialization-failure answer
    // (`probe.rs:148-152`): status 500, `{"error":"internal"}`. The
    // serialization itself is infallible for `Vec<String>` outcomes, so pin
    // the served literal by source presence, not by triggering it.
    served.push(("internal", 500, true));
    served
}

/// Codes returned by `ProbeError::code()`, parsed from the `code()` arms so a
/// new variant fails here until docs grow a row. The `Err` arm renders via
/// `json!({ "error": error.code() })`, so no `{"error":"<new>"}` literal
/// exists for a new variant: only this parse catches it.
fn code_arm_codes() -> Vec<String> {
    const MARKER: &str = "pub fn code(&self)";
    let start = PROBE_RS
        .find(MARKER)
        .unwrap_or_else(|| panic!("probe.rs keeps `ProbeError::code()`"));
    // The `code()` body is the next `match` through its closing `}` at the
    // same indent as `pub fn code` (the following doc comment starts `///`).
    let rest = &PROBE_RS[start..];
    let end = rest
        .find("\n    ///")
        .unwrap_or_else(|| panic!("probe.rs `code()` ends before the next doc comment"));
    let block = &rest[..end];
    let mut codes = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find('"') {
        let after = &rest[at + 1..];
        if let Some(end) = after.find('"') {
            codes.push(after[..end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    codes.sort();
    codes.dedup();
    codes
}

/// Collect every `{"error":"<code>"}` literal in the pool sources. `code()`
/// arms render through `json!({ "error": ... })`, so their codes are covered
/// by `code_arm_codes` above; this scan guards the literal bodies (`missing`,
/// `internal`, route constants) against a new literal landing without docs.
fn literal_wire_codes() -> Vec<String> {
    let mut codes = Vec::new();
    for src in [PROBE_RS, BODY_RS, ROUTES_RS, RUNTIME_RS] {
        let mut rest = src;
        while let Some(at) = rest.find("{\"error\":\"") {
            let after = &rest[at + "{\"error\":\"".len()..];
            if let Some(end) = after.find('"') {
                codes.push(after[..end].to_string());
                rest = &after[end..];
            } else {
                break;
            }
        }
    }
    codes.sort();
    codes.dedup();
    codes
}

#[test]
fn runtime_mappings_match_the_expected_wire_codes() {
    let served = served_from_runtime();
    let mut codes: Vec<&str> = served.iter().map(|(c, _, _)| *c).collect();
    codes.sort();
    let mut expected: Vec<&str> = EXPECTED.iter().map(|(c, _, _)| *c).collect();
    expected.sort();
    assert_eq!(codes, expected, "served wire-code set");
    for (code, status, retryable) in EXPECTED {
        let found = served
            .iter()
            .find(|(c, _, _)| c == code)
            .unwrap_or_else(|| panic!("served set contains {code}"));
        assert_eq!(
            (found.1, found.2),
            (*status, *retryable),
            "mapping for {code}"
        );
    }
    // Spot-check the exact served bodies so a shape change fails here.
    assert_eq!(
        ProbeError::TooLarge.code(),
        "too_large",
        "probe.rs:112 pins too_large"
    );
    assert_eq!(ProbeError::TooLarge.status(), 413);
    assert_eq!(busy_retry_secs(), Some(5));
    assert!(
        PROBE_RS.contains(r#"{"error":"internal"}"#),
        "probe.rs:150 serves the internal literal"
    );
    assert!(
        BODY_RS.contains(r#"{"error":"missing"}"#),
        "body.rs:87 serves the missing literal"
    );
    // A new `ProbeError` variant renders through `json!({ "error": ... })`
    // with no new literal, so only the `code()` parse catches it: the arms
    // must stay exactly the five non-literal served codes.
    assert_eq!(
        code_arm_codes(),
        vec![
            "bad_request",
            "busy",
            "provider_error",
            "provider_unavailable",
            "too_large"
        ],
        "ProbeError::code() arms stay pinned"
    );
    // `retryable` mirrors the ingest verdicts below; a mapping flip must fail
    // here until docs follow.
    assert!(
        INGEST_PROBE_RS.contains("400 | 413 => Err(PoolProbeError::Rejected)"),
        "ingest probe 400/413 stay Rejected (non-retryable)"
    );
    assert!(
        INGEST_BODY_RS.contains("404 if body ==") && INGEST_BODY_RS.contains("BodyFetch::Missing"),
        "ingest body exact-missing 404 stays Missing data (non-retryable)"
    );
    assert!(
        INGEST_BODY_RS.contains("413 => Ok(BodyFetch::TooLarge)"),
        "ingest body 413 stays TooLarge data (non-retryable)"
    );
    assert!(
        INGEST_BODY_RS.contains("400 => Err(PoolBodyError::Rejected)"),
        "ingest body 400 stays Rejected (non-retryable)"
    );
}

fn busy_retry_secs() -> Option<u64> {
    ProbeError::Busy {
        retry_after_ms: 5_000,
    }
    .retry_after_secs()
}

#[test]
fn served_wire_codes_are_subset_of_the_documented_pool_table() {
    let rows = parse_pool_table(DOCS);
    assert_eq!(rows.len(), EXPECTED.len(), "pool table row count");
    for (code, status, retryable) in EXPECTED {
        let row = rows
            .iter()
            .find(|r| r.code == *code)
            .unwrap_or_else(|| panic!("docs pool table gains a row for served code `{code}`"));
        assert_eq!(
            (row.status, row.retryable),
            (*status, *retryable),
            "docs drift for pool code {code}"
        );
        assert_eq!(
            row.lane, "Ingest",
            "pool code {code} serves on Lane::Ingest"
        );
    }
    // Literal bodies in the pool sources add no undocumented code.
    for code in literal_wire_codes() {
        assert!(
            EXPECTED.iter().any(|(c, _, _)| *c == code),
            "pool source literal `{code}` is a served wire code with a docs row"
        );
    }
}

#[test]
fn envelope_registry_parsers_still_ignore_the_pool_table() {
    // The pool table must not leak into the `| \`` envelope registry: every
    // pool row uses a bare code, and no pool code appears as an envelope row
    // naming stream (pool codes have no `api_error` REGISTRY entry by design
    // on this card: no prod-code change).
    for line in DOCS.lines().filter(|l| l.starts_with("| `")) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let code = cells[1].trim_matches('`');
        assert!(
            !EXPECTED.iter().any(|(c, _, _)| *c == code),
            "pool wire code `{code}` must not appear as an envelope-registry row"
        );
    }
}
