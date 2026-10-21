//! Acceptance wiring pins: the Postgres repository must actually USE the
//! deadline/stage seam specified in `db_deadline_stage_acceptance.rs`.
//!
//! This file deliberately references no new library API, so it compiles
//! and fails by assertion on the pre-change baseline (genuine RED, not a
//! missing-symbol compile error), and keeps passing only while the
//! production call sites stay wired:
//!
//! - the Hyperdrive `connect_raw` future and every repository query
//!   (`lookup`, `search`, and both `release_recordings` queries) run
//!   under `with_db_deadline` — five call sites in the HTTP layer, one
//!   finite budget each, fed by the `DB_OPERATION_DEADLINE_MS` default;
//! - driver/connect failures are staged (`RepoStage::Connect` /
//!   `::Query`), row decoding failures are `RepoStage::Decode`, and no
//!   unit-shaped `RepoError` construction survives in production code;
//! - exactly the `mb.db.failed` event is emitted through the shared
//!   `DB_FAILED_EVENT` / `db_failed_fields` seam (the behavioral suite
//!   pins the rendered line; this file pins that production emits via
//!   that seam, inside the HTTP layer where the trace id lives);
//! - `workers/mb/README.md` documents the deadline and the staged
//!   failure line instead of the old unlabelled "generic 500" story.
//!
//! Protected acceptance file: the implementing change must pass it
//! unchanged. Standalone integration target: no existing suite is edited.

const LIB_RS: &str = include_str!("../src/lib.rs");
const README: &str = include_str!("../README.md");

/// Production source with every `#[cfg(test)]` module removed, so literals
/// inside tests can never satisfy these pins.
fn production_source() -> String {
    let mut out = String::new();
    let rest = LIB_RS;
    match rest.split_once("#[cfg(test)]") {
        Some((head, tail)) => {
            out.push_str(head);
            // Skip to the end of the test module: the next top-level
            // `#[cfg(test)]` or end of file. Test modules in this file
            // are the trailing items, so dropping the remainder after
            // the first test module is the established slice used by
            // the in-module build-sha pin.
            let _ = tail;
        }
        None => {
            out.push_str(rest);
        }
    }
    out
}

fn http_layer_source() -> String {
    let production = production_source();
    let (_, http) = production
        .split_once("mod http_layer")
        .expect("production source must contain the HTTP layer");
    http.to_string()
}

#[test]
fn every_connect_and_query_call_site_uses_the_deadline_helper() {
    let http = http_layer_source();
    // One for `connect_raw`, one each for the lookup and search queries,
    // and two for release_recordings (existence check + recordings list):
    // each operation gets its own budget, started before dispatch.
    let call_sites = http.matches("with_db_deadline(").count();
    assert!(
        call_sites >= 5,
        "expected at least 5 with_db_deadline call sites in the HTTP layer \
         (connect + lookup + search + 2x release_recordings), found {call_sites}"
    );
    assert!(
        http.contains("DB_OPERATION_DEADLINE_MS"),
        "production call sites must draw their budget from the documented \
         DB_OPERATION_DEADLINE_MS default, not ad-hoc literals"
    );
    assert!(
        http.contains("connect_raw"),
        "premise: the Hyperdrive connect_raw call site must still exist"
    );
}

#[test]
fn production_failures_are_staged_and_no_unit_repo_error_survives() {
    let production = production_source();
    let http = http_layer_source();
    assert!(
        !production.contains("map_err(|_| RepoError)"),
        "unit-shaped RepoError construction must not survive: every \
         production failure carries a stage"
    );
    assert!(
        http.matches("RepoStage::Connect").count() >= 2,
        "binding/socket/config/connect failures must be staged Connect"
    );
    assert!(
        http.matches("RepoStage::Query").count() >= 4,
        "the four query call sites must stage driver failures as Query"
    );
    assert!(
        http.matches("RepoStage::Decode").count() >= 3,
        "row decoding (entity, recording, search hit) must be staged Decode"
    );
    assert!(
        production.matches("RepoStage::Timeout").count() >= 1,
        "deadline expiry must surface as the Timeout stage"
    );
}

#[test]
fn production_emits_mb_db_failed_through_the_shared_seam() {
    let production = production_source();
    let http = http_layer_source();
    assert!(
        production.contains("\"mb.db.failed\""),
        "the staged failure event literal mb.db.failed must exist"
    );
    assert!(
        http.contains("DB_FAILED_EVENT") && http.contains("db_failed_fields("),
        "the HTTP layer (which owns the trace id and the emitter) must \
         emit mb.db.failed via DB_FAILED_EVENT + db_failed_fields, so the \
         rendered line is exactly what the behavioral suite pins"
    );
}

#[test]
fn readme_documents_the_deadline_and_staged_failure_line() {
    let readme = README.to_ascii_lowercase();
    assert!(
        readme.contains("mb.db.failed"),
        "README must name the staged failure event operators will see"
    );
    assert!(
        readme.contains("deadline") && readme.contains("timeout"),
        "README must document the per-operation deadline and that a \
         timeout stays the generic 500, replacing the unlabelled story"
    );
    assert!(
        readme.contains("stage"),
        "README must explain the failure stage labels"
    );
}
