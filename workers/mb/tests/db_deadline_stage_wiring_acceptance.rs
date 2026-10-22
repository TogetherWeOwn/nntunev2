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
//!   under `with_db_deadline` — exactly five call sites in the HTTP
//!   layer, each fed by `Self::db_budget()` (itself drawn from the
//!   `DB_OPERATION_DEADLINE_MS` default); `connect` keeps ONE deadline
//!   covering `connect_raw` + the driver spawn + `SET statement_timeout`;
//! - driver/connect failures are staged (`RepoStage::Connect`, exactly 5
//!   setup sites in `connect`), row decoding failures are
//!   `RepoStage::Decode` (exactly 22: entity 11 + recording 6 + search
//!   hit 5), query failures are `RepoStage::Query` (exactly 4), and no
//!   unit-shaped `RepoError` construction survives in production code;
//! - exactly three `self.emit_db_failed(err.stage())` calls exist, one in
//!   each of `lookup` / `search` / `release_recordings`, emitting the
//!   `mb.db.failed` event through the shared `DB_FAILED_EVENT` /
//!   `db_failed_fields` seam (the behavioral suite pins the rendered
//!   line; this file pins that production emits via that seam, inside
//!   the HTTP layer where the trace id lives);
//! - `workers/mb/README.md` documents the deadline and the staged
//!   failure line instead of the old unlabelled "generic 500" story.
//!
//! Sensitivity (each verified to turn this suite red): deleting the
//! `search` emit, flipping the five `hit_from_row` Decode stages to
//! Query, or inflating one call-site budget away from `db_budget`.
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

/// Source slice from the `start` marker (inclusive) to the `end` marker
/// (exclusive), so per-site pins cannot be satisfied by a neighboring
/// function. Both markers must exist exactly once in the HTTP layer.
fn slice_between<'a>(http: &'a str, start: &str, end: &str) -> &'a str {
    let s = http
        .find(start)
        .unwrap_or_else(|| panic!("production source must contain {start}"));
    let after_start = s + start.len();
    let e = http[after_start..]
        .find(end)
        .unwrap_or_else(|| panic!("production source must contain {end} after {start}"));
    &http[s..after_start + e]
}

#[test]
fn every_connect_and_query_call_site_uses_the_deadline_helper() {
    let http = http_layer_source();
    // One for `connect_raw` (covering the driver spawn + SET
    // statement_timeout under the same deadline), one each for the
    // lookup and search queries, and two for release_recordings
    // (existence check + recordings list): each operation gets its own
    // budget, started before dispatch.
    let call_sites = http.matches("with_db_deadline(").count();
    assert!(
        call_sites == 5,
        "expected exactly 5 with_db_deadline call sites in the HTTP layer \
         (connect + lookup + search + 2x release_recordings), found {call_sites}"
    );
    let budgeted = http.matches("with_db_deadline(Self::db_budget()").count();
    assert!(
        budgeted == 5,
        "every deadline call site must use the shared Self::db_budget() shape, \
         found {budgeted} of 5"
    );
    let deadline_ms = http.matches("DB_OPERATION_DEADLINE_MS").count();
    assert!(
        deadline_ms == 2,
        "DB_OPERATION_DEADLINE_MS must appear exactly twice in the HTTP layer \
         (db_budget + the SET statement_timeout value), found {deadline_ms}"
    );
    assert!(
        http.contains("connect_raw"),
        "premise: the Hyperdrive connect_raw call site must still exist"
    );

    // Per-site: connect keeps ONE deadline covering connect_raw + the
    // driver spawn + SET statement_timeout.
    let connect = slice_between(&http, "async fn connect(", "impl MbRepo for PgRepo");
    assert!(
        connect.contains("connect_raw"),
        "the connect slice must still contain the connect_raw call site"
    );
    assert!(
        connect.contains("statement_timeout"),
        "the connect slice must still SET statement_timeout under its deadline"
    );
    let connect_deadlines = connect.matches("with_db_deadline(").count();
    assert!(
        connect_deadlines == 1,
        "connect must keep exactly one deadline call, found {connect_deadlines}"
    );
    let connect_budgeted = connect
        .matches("with_db_deadline(Self::db_budget()")
        .count();
    assert!(
        connect_budgeted == 1,
        "connect must feed its deadline from Self::db_budget(), found {connect_budgeted}"
    );
    let connect_ms = connect.matches("DB_OPERATION_DEADLINE_MS").count();
    assert!(
        connect_ms == 1,
        "connect must reference DB_OPERATION_DEADLINE_MS once (the SET value), \
         found {connect_ms}"
    );

    // Per-site: each repository function carries its own deadline call.
    let lookup = slice_between(&http, "async fn lookup(", "async fn search(");
    let lookup_deadlines = lookup.matches("with_db_deadline(").count();
    assert!(
        lookup_deadlines == 1,
        "lookup must carry exactly one deadline call, found {lookup_deadlines}"
    );
    let lookup_budgeted = lookup.matches("with_db_deadline(Self::db_budget()").count();
    assert!(
        lookup_budgeted == 1,
        "lookup must feed its deadline from Self::db_budget(), found {lookup_budgeted}"
    );
    let search = slice_between(&http, "async fn search(", "async fn release_recordings(");
    let search_deadlines = search.matches("with_db_deadline(").count();
    assert!(
        search_deadlines == 1,
        "search must carry exactly one deadline call, found {search_deadlines}"
    );
    let search_budgeted = search.matches("with_db_deadline(Self::db_budget()").count();
    assert!(
        search_budgeted == 1,
        "search must feed its deadline from Self::db_budget(), found {search_budgeted}"
    );
    let release = slice_between(&http, "async fn release_recordings(", "fn entity_from_row");
    let release_deadlines = release.matches("with_db_deadline(").count();
    assert!(
        release_deadlines == 2,
        "release_recordings must carry exactly two deadline calls \
         (existence check + recordings list), found {release_deadlines}"
    );
    let release_budgeted = release
        .matches("with_db_deadline(Self::db_budget()")
        .count();
    assert!(
        release_budgeted == 2,
        "release_recordings must feed both deadlines from Self::db_budget(), \
         found {release_budgeted}"
    );

    // No ad-hoc Duration budget outside db_budget: both Duration
    // spellings in the HTTP layer live in the db_budget definition.
    let db_budget = slice_between(&http, "fn db_budget()", "fn emit_db_failed");
    let total_duration = http.matches("Duration").count();
    assert!(
        total_duration == 2,
        "expected exactly 2 Duration spellings in the HTTP layer, found {total_duration}"
    );
    let budget_duration = db_budget.matches("Duration").count();
    assert!(
        budget_duration == 2,
        "both Duration spellings must live in db_budget, found {budget_duration} there"
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
    let connect_stages = http.matches("RepoStage::Connect").count();
    assert!(
        connect_stages == 5,
        "binding/socket/config/connect_raw/SET failures must be staged Connect: \
         expected exactly 5, found {connect_stages}"
    );
    let query_stages = http.matches("RepoStage::Query").count();
    assert!(
        query_stages == 4,
        "the four query call sites must stage driver failures as Query: \
         expected exactly 4, found {query_stages}"
    );
    let decode_stages = http.matches("RepoStage::Decode").count();
    assert!(
        decode_stages == 22,
        "row decoding (entity 11 + recording 6 + search hit 5) must be staged Decode: \
         expected exactly 22, found {decode_stages}"
    );
    let timeout_stages = production.matches("RepoStage::Timeout").count();
    assert!(
        timeout_stages >= 1,
        "deadline expiry must surface as the Timeout stage"
    );

    // Per-site staging: all Connect labels live in connect, each query
    // site stages its own driver failure as Query, and the search-hit
    // decoder keeps its five Decode labels.
    let connect = slice_between(&http, "async fn connect(", "impl MbRepo for PgRepo");
    let connect_in_connect = connect.matches("RepoStage::Connect").count();
    assert!(
        connect_in_connect == 5,
        "all 5 Connect stages must live in the connect slice, found {connect_in_connect}"
    );
    let lookup = slice_between(&http, "async fn lookup(", "async fn search(");
    let lookup_query = lookup.matches("RepoStage::Query").count();
    assert!(
        lookup_query == 1,
        "lookup must stage its driver failure as Query once, found {lookup_query}"
    );
    let search = slice_between(&http, "async fn search(", "async fn release_recordings(");
    let search_query = search.matches("RepoStage::Query").count();
    assert!(
        search_query == 1,
        "search must stage its driver failure as Query once, found {search_query}"
    );
    let release = slice_between(&http, "async fn release_recordings(", "fn entity_from_row");
    let release_query = release.matches("RepoStage::Query").count();
    assert!(
        release_query == 2,
        "release_recordings must stage two driver failures as Query, found {release_query}"
    );
    let hit = slice_between(&http, "fn hit_from_row", "fn json_headers");
    let hit_decode = hit.matches("RepoStage::Decode").count();
    assert!(
        hit_decode == 5,
        "hit_from_row must stage five decodes as Decode, found {hit_decode}"
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
    let emits = http.matches("self.emit_db_failed(err.stage())").count();
    assert!(
        emits == 3,
        "expected exactly 3 staged failure emits in the HTTP layer \
         (lookup + search + release_recordings), found {emits}"
    );
    let lookup = slice_between(&http, "async fn lookup(", "async fn search(");
    let lookup_emits = lookup.matches("self.emit_db_failed(err.stage())").count();
    assert!(
        lookup_emits == 1,
        "lookup must emit its staged failure once, found {lookup_emits}"
    );
    let search = slice_between(&http, "async fn search(", "async fn release_recordings(");
    let search_emits = search.matches("self.emit_db_failed(err.stage())").count();
    assert!(
        search_emits == 1,
        "search must emit its staged failure once, found {search_emits}"
    );
    let release = slice_between(&http, "async fn release_recordings(", "fn entity_from_row");
    let release_emits = release.matches("self.emit_db_failed(err.stage())").count();
    assert!(
        release_emits == 1,
        "release_recordings must emit its staged failure once, found {release_emits}"
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
