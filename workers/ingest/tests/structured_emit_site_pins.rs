//! Builder-owned pins for the structured-emission migration.
//!
//! The paired logging-standard acceptance file (Test Author-owned, protected)
//! pins the envelope shape and the migration at file granularity. These pins
//! bind the four properties review showed were still mutable without a red
//! suite (PR findings 1-4): the crate-wide raw-console scan must see
//! production code that lives *after* a test module (`main()` sits between
//! two of them in `lib.rs`); every migrated event must fire from its own
//! call site with the exact trace expression in scope there; the
//! size-refused terminal emit must keep its `> 0` guard; and returned
//! error strings must never be built from an error value via `to_string()`.
//! Round 2 hardens the pins themselves: the `{}` sink shape is allow-listed
//! by exact spelling, file, and crate-wide count (a second sink-shaped line
//! anywhere is a violation), and multi-event functions pin their event
//! source order (swapping two names inside one function keeps every count
//! at 1 but flips an adjacent pair).
//!
//! This file is intentionally separate from the protected acceptance
//! file: it strengthens the same claims without touching Test Author tests.

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Match a delimiter pair without counting comment or string contents.
/// (Same proven shape as the admission guards' scanner.)
fn balanced_end(source: &str, open: usize, left: u8, right: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            i += source[i..].find('\n').unwrap_or(bytes.len() - i);
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let mut comments = 1;
            i += 2;
            while i < bytes.len() && comments > 0 {
                if bytes[i..].starts_with(b"/*") {
                    comments += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    comments -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i] == b'"' {
            let hashes = bytes[..i].iter().rev().take_while(|b| **b == b'#').count();
            let raw = i > hashes && bytes[i - hashes - 1] == b'r';
            i += 1;
            if raw {
                let suffix = format!("\"{}", "#".repeat(hashes));
                i += source[i..].find(&suffix)? + suffix.len();
            } else {
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            continue;
        }
        if bytes[i] == left {
            depth += 1;
        } else if bytes[i] == right {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

/// Remove every `#[cfg(test)] mod … { … }` span, brace-matched, keeping
/// production code on both sides (including `main()`, which lives after
/// the first test module in `lib.rs`). A `#[cfg(test)]` marker that is
/// not followed by a module (e.g. inside a string literal) is skipped.
fn strip_test_modules(source: &str) -> String {
    let mut kept = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(attr) = rest.find("#[cfg(test)]") {
        // Do not treat the marker inside a string literal or comment as an
        // attribute: only accept it when the text before it on its line is
        // blank (attributes sit on their own line in this crate).
        let line_start = rest[..attr].rfind('\n').map_or(0, |pos| pos + 1);
        if !rest[line_start..attr].trim().is_empty() {
            kept.push_str(&rest[..attr + "#[cfg(test)]".len()]);
            rest = &rest[attr + "#[cfg(test)]".len()..];
            continue;
        }
        let after = &rest[attr + "#[cfg(test)]".len()..];
        let mod_kw = match after.find("mod") {
            Some(pos) => pos,
            None => {
                kept.push_str(rest);
                rest = "";
                break;
            }
        };
        // Only a module attribute (whitespace between marker and `mod`).
        if !after[..mod_kw].trim().is_empty() {
            kept.push_str(&rest[..attr + "#[cfg(test)]".len()]);
            rest = after;
            continue;
        }
        let after_mod = &after[mod_kw + "mod".len()..];
        let open_rel = match after_mod.find('{') {
            Some(pos) => pos,
            None => {
                kept.push_str(rest);
                rest = "";
                break;
            }
        };
        // The module name sits between `mod` and `{` with nothing else.
        if after_mod[..open_rel]
            .trim()
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
            || after_mod[..open_rel].trim().is_empty()
        {
            kept.push_str(&rest[..attr + "#[cfg(test)]".len()]);
            rest = after;
            continue;
        }
        let open = attr + "#[cfg(test)]".len() + mod_kw + "mod".len() + open_rel;
        let end = balanced_end(rest, open, b'{', b'}').expect("test module must close");
        kept.push_str(&rest[..attr]);
        rest = &rest[end..];
    }
    kept.push_str(rest);
    kept
}

fn production(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{rel} must be readable"));
    strip_test_modules(&source)
}

fn fn_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("fn {name} must exist"));
    let open = source[start..].find('{').expect("fn body must open") + start;
    &source[open..balanced_end(source, open, b'{', b'}').expect("fn body must close")]
}

fn compact(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Byte start of a look-behind window of `len` bytes ending at `end`,
/// aligned up to a char boundary so slicing never panics on multibyte
/// text (em-dashes in comments) above the call.
fn window_start(slice: &str, end: usize, len: usize) -> usize {
    let mut start = end.saturating_sub(len);
    while !slice.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// Collect the `emit(` call(s) in `slice` carrying `event`, balanced-paren
/// matched so the whole call (level, trace, name, fields) is pinned at once.
fn emit_calls_with<'a>(slice: &'a str, event: &str) -> Vec<&'a str> {
    let needle = format!("\"{event}\"");
    slice
        .match_indices("emit(")
        .filter(|(i, _)| {
            *i == 0
                || !slice.as_bytes()[i - 1].is_ascii_alphanumeric()
                    && slice.as_bytes()[i - 1] != b'_'
        })
        .filter_map(|(i, _)| {
            let open = i + "emit".len();
            let end = balanced_end(slice, open, b'(', b')')?;
            Some(&slice[i..end])
        })
        .filter(|call| call.contains(&needle))
        .collect()
}

// ---------------------------------------------------------------------------
// Finding 1: the crate-wide raw-console scan must see production code that
// lives after a test module.
// ---------------------------------------------------------------------------

/// A raw console line added to production after the first test module
/// (e.g. in `main()`) must fail the scan: regression fixture first.
#[test]
fn test_module_stripping_keeps_later_production() {
    let fixture = "\
fn before() {}
#[cfg(test)]
mod tests {
    #[test]
    fn t() { worker::console_log!(\"test {}\", 1); }
}
pub async fn main() {
    worker::console_log!(\"raw {:?}\", err);
}
";
    let stripped = strip_test_modules(fixture);
    assert!(
        stripped.contains("pub async fn main"),
        "production after a test module must survive stripping"
    );
    assert!(
        stripped.contains("raw {:?}"),
        "a raw console line after a test module must stay visible to the scan"
    );
    assert!(
        !stripped.contains("mod tests"),
        "test module bodies must not leak into the production scan"
    );
}

#[test]
fn no_raw_console_anywhere_in_production() {
    // The only permitted console emissions in production are the two
    // structured-sink lines, pinned by exact spelling and file: `emit()`
    // in lib.rs renders the six-key record; the verify helper renders the
    // size-refused line. Exempting the `{}` shape alone lets a raw
    // `console_log!("{}", req.path())` pass anywhere (e.g. in `main()`),
    // so any other console line — including a second sink-shaped line —
    // is a violation, and the crate-wide sink count is pinned at 2.
    // `src/repair.rs` is excluded: a separate slice owns that file.
    const LIB_SINK: &str = "console_log!(\"{}\", record.to_json_line());";
    const VERIFY_SINK: &str = "worker::console_log!(\"{}\", line);";
    const CONSOLE_MACROS: &[&str] = &[
        "console_log!",
        "console_error!",
        "console_warn!",
        "console_debug!",
    ];
    let src_dir = manifest_dir().join("src");
    let mut violations: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    let mut sinks = 0usize;
    let entries = std::fs::read_dir(&src_dir).expect("src directory must be readable");
    for entry in entries {
        let path = entry.expect("src entry must be readable").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let name = path
            .file_name()
            .expect("source file must have a name")
            .to_string_lossy()
            .into_owned();
        if name == "repair.rs" {
            continue;
        }
        scanned += 1;
        let stripped = production(&format!("src/{name}"));
        for line in stripped.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || !CONSOLE_MACROS.iter().any(|m| line.contains(m)) {
                continue;
            }
            let shape = line.trim();
            let allowed = (name == "lib.rs" && shape == LIB_SINK)
                || (name == "verify.rs" && shape == VERIFY_SINK);
            if allowed {
                sinks += 1;
            } else {
                violations.push(format!("src/{name}: {shape}"));
            }
        }
    }
    assert!(scanned >= 10, "scan must cover the crate sources");
    // The scan is only stronger than a truncate-at-first-test-module scan
    // if post-test-module production is actually in the scanned text.
    assert!(
        production("src/lib.rs").contains("pub async fn main"),
        "lib.rs main() (after the test modules) must be part of the scan"
    );
    assert_eq!(
        sinks, 2,
        "exactly the two structured sinks (lib.rs emit, verify helper) \
         may write to the console; a copy elsewhere must fail"
    );
    assert!(
        violations.is_empty(),
        "production code must emit structured events, never raw console lines; found:\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Finding 2: every migrated event is pinned to its call site with the exact
// trace expression in scope there (not "event string somewhere in the file"
// plus "the word trace somewhere in the window").
// ---------------------------------------------------------------------------

/// (event, file, enclosing fn, exact compacted trace argument).
/// `mark_queued`/`upsert` resolve inside their Edge/Pg impl slices (see below).
const SITE_PINS: &[(&str, &str, &str, &str)] = &[
    (
        "ingest.queue.enqueue_failed",
        "src/lib.rs",
        "try_enqueue",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.queue.binding_missing",
        "src/lib.rs",
        "try_enqueue",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.meter.binding_missing",
        "src/lib.rs",
        "try_meter",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.meter.write_failed",
        "src/lib.rs",
        "try_meter",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.library_db.binding_missing",
        "src/lib.rs",
        "library_db_client",
        "trace_id",
    ),
    (
        "ingest.library_db.connection_failed",
        "src/lib.rs",
        "library_db_client",
        "&trace_id",
    ),
    (
        "ingest.admission.commit_failed",
        "src/lib.rs",
        "mark_queued",
        "&self.trace_id",
    ),
    (
        "ingest.admission_checkpoint.read_failed",
        "src/lib.rs",
        "ensure_admission_checkpoint",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.admission_checkpoint.write_failed",
        "src/lib.rs",
        "ensure_admission_checkpoint",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.admission_checkpoint.redrive_enqueue_failed",
        "src/lib.rs",
        "ensure_admission_checkpoint",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.replay_checkpoint.read_failed",
        "src/lib.rs",
        "admit_metered_job",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.payload.write_failed",
        "src/lib.rs",
        "store_upload_payload",
        "trace_id",
    ),
    (
        "ingest.payload.put_failed",
        "src/lib.rs",
        "store_upload_payload",
        "trace_id",
    ),
    (
        "ingest.payload.binding_missing",
        "src/lib.rs",
        "store_upload_payload",
        "trace_id",
    ),
    (
        "ingest.payload.read_failed",
        "src/queue_consumer.rs",
        "load_upload_parse",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.payload.binding_missing",
        "src/queue_consumer.rs",
        "load_upload_parse",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.verify_progress.read_failed",
        "src/queue_consumer.rs",
        "run_verifying",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.scratch.binding_missing",
        "src/queue_consumer.rs",
        "run_verifying",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.verify_progress.write_failed",
        "src/queue_consumer.rs",
        "run_verifying",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.verify.size_refused_terminal",
        "src/queue_consumer.rs",
        "run_verifying",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.match_payload.read_failed",
        "src/queue_consumer.rs",
        "load_payload_for_match",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.library.upsert_failed",
        "src/queue_consumer.rs",
        "upsert",
        "self.trace_id",
    ),
    (
        "ingest.checkpoint.write_failed",
        "src/queue_consumer.rs",
        "persist",
        "trace_id",
    ),
    (
        "ingest.checkpoint.read_failed",
        "src/queue_consumer.rs",
        "process_message",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.continuation.enqueue_failed",
        "src/queue_consumer.rs",
        "send_continuation",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.continuation.binding_missing",
        "src/queue_consumer.rs",
        "send_continuation",
        "&nntune_logging::resolve_trace_id(msg.trace_id.as_deref())",
    ),
    (
        "ingest.dlq.send_failed",
        "src/queue_consumer.rs",
        "send_to_dlq",
        "trace_id",
    ),
    (
        "ingest.dlq.binding_missing",
        "src/queue_consumer.rs",
        "send_to_dlq",
        "trace_id",
    ),
];

fn site_slice<'a>(production: &'a str, func: &str) -> &'a str {
    // `mark_queued`/`upsert` exist in several impls: resolve inside the
    // production impl slice, mirroring the admission guards.
    if func == "mark_queued" {
        let after_impl = production
            .split_once("impl AdmissionLedger for PgAdmissionLedger")
            .expect("PgAdmissionLedger impl must exist")
            .1;
        let bounded = after_impl
            .split_once("async fn release_claim")
            .expect("release_claim must bound mark_queued")
            .0;
        return fn_body(bounded, "mark_queued");
    }
    if func == "upsert" {
        let edge_impl = production
            .split_once("impl MatchWriteRuntime for EdgeMatchWrite")
            .expect("edge upsert impl must exist")
            .1;
        return fn_body(edge_impl, "upsert");
    }
    fn_body(production, func)
}

#[test]
fn migrated_events_fire_from_their_own_call_site_with_its_trace() {
    let lib = production("src/lib.rs");
    let consumer = production("src/queue_consumer.rs");
    assert_eq!(
        SITE_PINS.len(),
        28,
        "one pin per migrated call site (the shared binding_missing event pins both files)"
    );
    for (event, rel, func, trace) in SITE_PINS {
        let source = if *rel == "src/lib.rs" {
            &lib
        } else {
            &consumer
        };
        let slice = site_slice(source, func);
        let calls = emit_calls_with(slice, event);
        assert_eq!(
            calls.len(),
            1,
            "{rel}::{func} must emit {event} exactly once (swapping two event names must fail)"
        );
        let call = compact(calls[0]);
        assert!(
            call.starts_with("emit(") || call.starts_with("crate::emit("),
            "{rel}::{func} must emit {event} through the structured emitter: {call}"
        );
        let head = format!("Level::Error,{trace},\"{event}\",");
        assert!(
            call.contains(&head),
            "{rel}::{func} must keep Error level and the originating trace for {event} \
             (dropping the trace to None must fail): {call}"
        );
        assert!(
            call.contains("\"build_sha\":"),
            "{rel}::{func} must carry the build SHA for {event}: {call}"
        );
    }
}

// ---------------------------------------------------------------------------
// Round 2: two event names inside one multi-event function can still be
// swapped undetected: every per-function count stays 1 when both sites
// share the function, the trace argument, and identical fields (e.g. the
// write/put pair in `store_upload_payload`). The source order of the
// pinned events per function closes it: any swap flips an adjacent pair.
// Single-event functions need no entry: count == 1 already pins them.
// ---------------------------------------------------------------------------

/// (file, enclosing fn, pinned events in source order).
const ORDER_PINS: &[(&str, &str, &[&str])] = &[
    (
        "src/lib.rs",
        "try_enqueue",
        &[
            "ingest.queue.enqueue_failed",
            "ingest.queue.binding_missing",
        ],
    ),
    (
        "src/lib.rs",
        "try_meter",
        &["ingest.meter.binding_missing", "ingest.meter.write_failed"],
    ),
    (
        "src/lib.rs",
        "library_db_client",
        &[
            "ingest.library_db.binding_missing",
            "ingest.library_db.connection_failed",
        ],
    ),
    (
        "src/lib.rs",
        "ensure_admission_checkpoint",
        &[
            "ingest.admission_checkpoint.read_failed",
            "ingest.admission_checkpoint.write_failed",
            "ingest.admission_checkpoint.redrive_enqueue_failed",
        ],
    ),
    (
        "src/lib.rs",
        "store_upload_payload",
        &[
            "ingest.payload.write_failed",
            "ingest.payload.put_failed",
            "ingest.payload.binding_missing",
        ],
    ),
    (
        "src/queue_consumer.rs",
        "load_upload_parse",
        &[
            "ingest.payload.read_failed",
            "ingest.payload.binding_missing",
        ],
    ),
    (
        "src/queue_consumer.rs",
        "run_verifying",
        &[
            "ingest.verify_progress.read_failed",
            "ingest.scratch.binding_missing",
            "ingest.verify_progress.write_failed",
            "ingest.verify.size_refused_terminal",
        ],
    ),
    (
        "src/queue_consumer.rs",
        "send_continuation",
        &[
            "ingest.continuation.enqueue_failed",
            "ingest.continuation.binding_missing",
        ],
    ),
    (
        "src/queue_consumer.rs",
        "send_to_dlq",
        &["ingest.dlq.send_failed", "ingest.dlq.binding_missing"],
    ),
];

#[test]
fn multi_event_functions_keep_their_source_order() {
    let lib = production("src/lib.rs");
    let consumer = production("src/queue_consumer.rs");
    for (rel, func, expected) in ORDER_PINS {
        let source = if *rel == "src/lib.rs" {
            &lib
        } else {
            &consumer
        };
        let slice = site_slice(source, func);
        let mut offsets = Vec::with_capacity(expected.len());
        for event in expected.iter() {
            let calls = emit_calls_with(slice, event);
            assert_eq!(
                calls.len(),
                1,
                "{rel}::{func} must emit {event} exactly once"
            );
            offsets.push(calls[0].as_ptr() as usize - slice.as_ptr() as usize);
        }
        for (pair, offs) in expected.windows(2).zip(offsets.windows(2)) {
            assert!(
                offs[0] < offs[1],
                "{rel}::{func} must emit {} before {} (swapping the two names must fail)",
                pair[0],
                pair[1]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Finding 3: the size-refused terminal emit fires exactly for a terminal
// size refusal (guarded by `*size_refused_blocks > 0` in the Unrecoverable
// arm), never for a zero-refusal outcome.
// ---------------------------------------------------------------------------

#[test]
fn size_refused_terminal_keeps_its_positive_refusal_guard() {
    let consumer = production("src/queue_consumer.rs");
    let slice = site_slice(&consumer, "run_verifying");
    let calls = emit_calls_with(slice, "ingest.verify.size_refused_terminal");
    assert_eq!(
        calls.len(),
        1,
        "run_verifying must emit the terminal event exactly once"
    );
    let call = compact(calls[0]);
    assert!(
        call.contains("\"size_refused_blocks\":"),
        "the terminal event must carry the refusal count: {call}"
    );
    // The guard precedes the call: inverting `> 0` to `== 0` must fail.
    let call_start = slice.find(calls[0]).expect("call must sit in its slice");
    let guard_window = compact(&slice[window_start(slice, call_start, 900)..call_start]);
    assert!(
        guard_window.contains("VerifyOutcome::Unrecoverable"),
        "the terminal emit must sit in the Unrecoverable arm"
    );
    assert!(
        guard_window.contains("*size_refused_blocks>0"),
        "the terminal emit must keep its positive-refusal guard \
         (inverting to == 0 must fail): …{}",
        &guard_window[window_start(&guard_window, guard_window.len(), 160)..]
    );
}

// ---------------------------------------------------------------------------
// Finding 4: returned error strings are fixed categories, never built from
// an error value (`e.to_string()` bypasses a `format!`-only scan).
// ---------------------------------------------------------------------------

#[test]
fn migrated_files_never_serialize_an_error_value_into_a_string() {
    for rel in ["src/lib.rs", "src/queue_consumer.rs"] {
        let source = production(rel);
        for (index, _) in source.match_indices(".to_string()") {
            let before = &source[..index];
            let start = before
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(0, |pos| pos + 1);
            let receiver = &before[start..];
            assert!(
                !matches!(receiver, "e" | "error" | "err"),
                "{rel} builds a string from the error binding `{receiver}`; \
                 returned error strings are fixed categories only"
            );
        }
    }
    // Both meter failure strings stay pinned literals.
    let lib = production("src/lib.rs");
    assert_eq!(
        lib.matches("\"meter_write_failed\"").count(),
        1,
        "the meter write failure must stay the fixed category string"
    );
    assert_eq!(
        lib.matches("\"meter_binding_missing\"").count(),
        1,
        "the meter binding failure must stay the fixed category string"
    );
}
