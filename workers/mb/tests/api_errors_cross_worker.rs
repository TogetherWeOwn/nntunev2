//! Cross-worker pin for the shared API error taxonomy (`docs/api-errors.md`).
//!
//! Each worker already pins its own slice both ways in its `api_error`
//! module (`registry_matches_docs_api_errors` in `workers/api`,
//! `workers/ingest`, `workers/mb` and `workers/stream`): a declared code
//! must appear in the docs table with matching status/retryable, and a docs
//! row naming that worker must be declared. What no per-worker test can see
//! is the cross-worker picture, so this file pins it in one place:
//!
//! - every docs registry row names *exactly* the set of workers declaring
//!   that code, with matching status and retryable flags;
//! - a code declared by two workers with different status/retryable fails
//!   with a message naming both modules;
//! - docs rows naming an unknown worker (there is no other registry) fail.
//!
//! `workers/repair` has no codified registry and the docs carry no repair
//! rows, so repair is explicitly out of scope: no repair codes are invented
//! anywhere here, and `repair_has_no_registry_rows_or_module` keeps that
//! absence explicit. Only test files change for this pin: the four
//! registries are read as source text via `include_str!` (no new crate
//! dependencies, so the stream dependency-free constraint is untouched).

use std::collections::BTreeMap;
use std::path::Path;

const DOCS: &str = include_str!("../../../docs/api-errors.md");
const API_SRC: &str = include_str!("../../api/src/api_error.rs");
const INGEST_SRC: &str = include_str!("../../ingest/src/api_error.rs");
const MB_SRC: &str = include_str!("../src/api_error.rs");
const STREAM_SRC: &str = include_str!("../../stream/src/api_error.rs");

/// Workers with a codified `api_error` registry. Repair is out of scope and
/// must never appear here; see `repair_has_no_registry_rows_or_module`.
const KNOWN_WORKERS: [&str; 4] = ["api", "ingest", "mb", "stream"];

/// One `docs/api-errors.md` registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocRow {
    code: String,
    status: u16,
    retryable: bool,
    owners: Vec<String>,
}

/// One `CodeSpec` entry parsed from a worker's `REGISTRY` slice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Declaration {
    worker: &'static str,
    code: String,
    status: u16,
    retryable: bool,
}

/// Parse the `| \`code\` | status | retryable | owners | meaning |` rows.
fn parse_docs_table(doc: &str) -> Vec<DocRow> {
    let mut rows = Vec::new();
    for line in doc.lines().filter(|l| l.starts_with("| `")) {
        // Cells: ["", "`code`", status, retryable, owners, meaning, ""].
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        assert!(cells.len() >= 6, "registry row keeps 6+ cells: {line}");
        let code = cells[1].trim_matches('`').to_string();
        let status: u16 = cells[2]
            .parse()
            .unwrap_or_else(|_| panic!("registry status parses for {code}"));
        let retryable: bool = cells[3]
            .parse()
            .unwrap_or_else(|_| panic!("registry retryable parses for {code}"));
        let owners: Vec<String> = cells[4].split(',').map(|o| o.trim().to_string()).collect();
        rows.push(DocRow {
            code,
            status,
            retryable,
            owners,
        });
    }
    rows
}

/// Parse the `CodeSpec { code, status, retryable }` entries of one worker's
/// `REGISTRY` slice. Scoped to the `REGISTRY` const so the `FALLBACK`
/// `CodeSpec` (same shape, outside the slice) is never picked up.
fn parse_registry(src: &str, worker: &'static str) -> Vec<Declaration> {
    let start = src
        .find("pub const REGISTRY")
        .unwrap_or_else(|| panic!("{worker} api_error.rs declares REGISTRY"));
    let block = &src[start..];
    let end = block
        .find("];")
        .unwrap_or_else(|| panic!("{worker} REGISTRY slice terminates"));
    let block = &block[..end];
    let mut entries = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find("code:") {
        let after = &rest[at + "code:".len()..];
        let open = after
            .find('"')
            .unwrap_or_else(|| panic!("{worker} registry code opens quote"));
        let close = after[open + 1..]
            .find('"')
            .unwrap_or_else(|| panic!("{worker} registry code closes quote"))
            + open
            + 1;
        let code = after[open + 1..close].to_string();
        let tail = &after[close + 1..];
        let status_at = tail
            .find("status:")
            .unwrap_or_else(|| panic!("{worker} registry status for {code}"));
        let digits: String = tail[status_at + "status:".len()..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let status: u16 = digits
            .parse()
            .unwrap_or_else(|_| panic!("{worker} registry status parses for {code}"));
        let retry_at = tail
            .find("retryable:")
            .unwrap_or_else(|| panic!("{worker} registry retryable for {code}"));
        let value = tail[retry_at + "retryable:".len()..].trim_start();
        let retryable = if value.starts_with("true") {
            true
        } else if value.starts_with("false") {
            false
        } else {
            panic!("{worker} registry retryable parses for {code}");
        };
        entries.push(Declaration {
            worker,
            code,
            status,
            retryable,
        });
        rest = &value[4..];
    }
    entries
}

/// Pure cross-worker consistency check over parsed tables: one message per
/// problem, so the live pin and the synthetic cases below share the logic.
/// A conflicting duplicate names every declaring worker.
fn check_consistency(rows: &[DocRow], decls: &[Declaration]) -> Vec<String> {
    let mut errors = Vec::new();
    // Docs rows must only name workers with a codified registry. This keeps
    // repair's absence explicit: a `repair` owner fails here instead of
    // passing vacuously in every per-worker suite.
    for row in rows {
        for owner in &row.owners {
            if !KNOWN_WORKERS.contains(&owner.as_str()) {
                errors.push(format!(
                    "code {:?}: docs names unknown owning worker {:?}; registries exist only for api, ingest, mb, stream (repair has none)",
                    row.code, owner
                ));
            }
        }
    }
    // Every docs row names exactly the declaring workers, with matching
    // status and retryable flags.
    for row in rows {
        let declarers: Vec<&Declaration> = decls.iter().filter(|d| d.code == row.code).collect();
        let mut declaring_workers: Vec<&str> = declarers.iter().map(|d| d.worker).collect();
        declaring_workers.sort_unstable();
        declaring_workers.dedup();
        let mut owners: Vec<&str> = row.owners.iter().map(String::as_str).collect();
        owners.sort_unstable();
        if declaring_workers != owners {
            errors.push(format!(
                "code {:?}: docs owning workers {:?} != declaring workers {:?}",
                row.code, owners, declaring_workers
            ));
        }
        for d in &declarers {
            if d.status != row.status || d.retryable != row.retryable {
                errors.push(format!(
                    "code {:?}: worker {:?} declares status={} retryable={} but docs/api-errors.md pins status={} retryable={}",
                    row.code, d.worker, d.status, d.retryable, row.status, row.retryable
                ));
            }
        }
    }
    // A code declared by two workers must agree on status/retryable: group
    // by code and fail naming every declaring worker on any split.
    let mut by_code: BTreeMap<&str, Vec<&Declaration>> = BTreeMap::new();
    for d in decls {
        by_code.entry(d.code.as_str()).or_default().push(d);
    }
    for (code, group) in &by_code {
        let mut seen: Vec<(u16, bool)> = Vec::new();
        for d in group {
            let flags = (d.status, d.retryable);
            if !seen.contains(&flags) {
                seen.push(flags);
            }
        }
        if seen.len() > 1 {
            let detail: Vec<String> = group
                .iter()
                .map(|d| {
                    format!(
                        "worker {:?} declares status={} retryable={}",
                        d.worker, d.status, d.retryable
                    )
                })
                .collect();
            errors.push(format!(
                "code {code:?}: conflicting duplicate declarations: {}",
                detail.join("; ")
            ));
        }
    }
    // Every declared code must appear in the docs registry.
    for d in decls {
        if !rows.iter().any(|r| r.code == d.code) {
            errors.push(format!(
                "code {:?}: declared by worker {:?} but missing from docs/api-errors.md registry",
                d.code, d.worker
            ));
        }
    }
    errors
}

fn live_tables() -> (Vec<DocRow>, Vec<Declaration>) {
    let rows = parse_docs_table(DOCS);
    let mut decls = Vec::new();
    for (worker, src) in [
        ("api", API_SRC),
        ("ingest", INGEST_SRC),
        ("mb", MB_SRC),
        ("stream", STREAM_SRC),
    ] {
        decls.extend(parse_registry(src, worker));
    }
    (rows, decls)
}

fn doc_row(code: &str, status: u16, retryable: bool, owners: &[&str]) -> DocRow {
    DocRow {
        code: code.to_string(),
        status,
        retryable,
        owners: owners.iter().map(|o| o.to_string()).collect(),
    }
}

fn decl(worker: &'static str, code: &str, status: u16, retryable: bool) -> Declaration {
    Declaration {
        worker,
        code: code.to_string(),
        status,
        retryable,
    }
}

#[test]
fn docs_registry_matches_all_worker_registries() {
    let (rows, decls) = live_tables();
    // Guard against a silently-empty parse passing vacuously. Counts mirror
    // the registries today; bump them when the "Adding a code" procedure in
    // docs/api-errors.md lands a new row.
    assert_eq!(rows.len(), 20, "docs registry row count");
    for (worker, expected) in [("api", 12), ("ingest", 9), ("mb", 4), ("stream", 13)] {
        let count = decls.iter().filter(|d| d.worker == worker).count();
        assert_eq!(count, expected, "{worker} REGISTRY entry count");
    }
    let errors = check_consistency(&rows, &decls);
    assert!(
        errors.is_empty(),
        "cross-worker api-errors drift:\n{}",
        errors.join("\n")
    );
}

#[test]
fn agreeing_duplicates_across_workers_pass() {
    // Table-driven: every shared code with identical flags stays green.
    for (code, status, retryable, owners) in [
        (
            "not_found",
            404,
            false,
            vec!["api", "ingest", "mb", "stream"],
        ),
        ("rate_limited", 429, true, vec!["api", "ingest", "stream"]),
        (
            "internal_error",
            500,
            true,
            vec!["api", "ingest", "mb", "stream"],
        ),
    ] {
        let rows = vec![doc_row(code, status, retryable, &owners)];
        let decls: Vec<Declaration> = owners
            .iter()
            .map(|w| decl(w, code, status, retryable))
            .collect();
        assert!(
            check_consistency(&rows, &decls).is_empty(),
            "agreeing duplicate {code} must pass"
        );
    }
}

#[test]
fn conflicting_duplicate_status_fails_naming_both_workers() {
    let rows = vec![doc_row(
        "plan_limit_exceeded",
        402,
        false,
        &["api", "stream"],
    )];
    let decls = vec![
        decl("api", "plan_limit_exceeded", 402, false),
        decl("stream", "plan_limit_exceeded", 429, false),
    ];
    let errors = check_consistency(&rows, &decls);
    assert!(!errors.is_empty(), "conflicting duplicate status must fail");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("conflicting duplicate declarations")
                && e.contains("worker \"api\"")
                && e.contains("worker \"stream\"")),
        "conflict must name both modules, got: {}",
        errors.join("\n")
    );
}

#[test]
fn conflicting_duplicate_retryable_fails_naming_both_workers() {
    let rows = vec![doc_row("rate_limited", 429, true, &["api", "stream"])];
    let decls = vec![
        decl("api", "rate_limited", 429, true),
        decl("stream", "rate_limited", 429, false),
    ];
    let errors = check_consistency(&rows, &decls);
    assert!(
        !errors.is_empty(),
        "conflicting duplicate retryable must fail"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("conflicting duplicate declarations")
                && e.contains("worker \"api\"")
                && e.contains("worker \"stream\"")),
        "conflict must name both modules, got: {}",
        errors.join("\n")
    );
}

#[test]
fn docs_owner_set_must_match_declarers_exactly() {
    // Docs names only api, but stream also declares the code.
    let rows = vec![doc_row("conflict", 409, false, &["api"])];
    let decls = vec![
        decl("api", "conflict", 409, false),
        decl("ingest", "conflict", 409, false),
    ];
    let errors = check_consistency(&rows, &decls);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("conflict") && e.contains("api") && e.contains("ingest")),
        "owner mismatch must name the code and both sides, got: {}",
        errors.join("\n")
    );
}

#[test]
fn declared_code_missing_from_docs_fails() {
    let rows = vec![doc_row("not_found", 404, false, &["mb"])];
    let decls = vec![
        decl("mb", "not_found", 404, false),
        decl("mb", "stale_code", 400, false),
    ];
    let errors = check_consistency(&rows, &decls);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("stale_code") && e.contains("mb")),
        "undocumented declaration must name code and worker, got: {}",
        errors.join("\n")
    );
}

#[test]
fn docs_row_with_no_declarer_fails() {
    let rows = vec![
        doc_row("not_found", 404, false, &["mb"]),
        doc_row("orphan_code", 400, false, &["mb"]),
    ];
    let decls = vec![decl("mb", "not_found", 404, false)];
    let errors = check_consistency(&rows, &decls);
    assert!(
        errors.iter().any(|e| e.contains("orphan_code")),
        "docs row with no declarer must fail, got: {}",
        errors.join("\n")
    );
}

#[test]
fn repair_has_no_registry_rows_or_module() {
    // Repair is explicitly out of scope: the docs carry no repair rows and
    // workers/repair carries no api_error registry. No repair codes are
    // invented in any fixture above, either.
    let (rows, _) = live_tables();
    for row in &rows {
        assert!(
            !row.owners.iter().any(|o| o == "repair"),
            "docs row {:?} must not name repair as an owning worker",
            row.code
        );
    }
    let module = Path::new(env!("CARGO_MANIFEST_DIR")).join("../repair/src/api_error.rs");
    assert!(
        !module.exists(),
        "workers/repair has no api_error registry; update scope deliberately if one lands"
    );
}
