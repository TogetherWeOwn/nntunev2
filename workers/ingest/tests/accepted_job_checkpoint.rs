//! Source-level guard for the accepted-job checkpoint ordering contract.
//!
//! The native ingest integration tests exercise domain helpers, not a Worker
//! runtime: `worker::Env` and `RouteContext` are runtime-provided bindings. Keep
//! these checks focused on the ordering barrier until the crate has a local
//! route/KV/Queue harness.

use std::path::Path;

fn source(path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_at = source
        .find(start)
        .unwrap_or_else(|| panic!("source is missing {start:?}"));
    let tail = &source[start_at..];
    let end_at = tail.find(end).unwrap_or(tail.len());
    &tail[..end_at]
}

fn assert_initial_checkpoint_barrier(route: &str, name: &str) {
    let initial = route
        .find("initial_checkpoint")
        .unwrap_or_else(|| panic!("{name} does not construct the initial checkpoint"));
    let persist = route
        .find("checkpoint::put")
        .unwrap_or_else(|| panic!("{name} does not persist the initial checkpoint"));
    let enqueue = route
        .find("try_enqueue")
        .unwrap_or_else(|| panic!("{name} does not enqueue the accepted job"));
    let response = route
        .find("queued_envelope")
        .unwrap_or_else(|| panic!("{name} does not return the queue result"));

    assert!(
        initial < persist,
        "{name} must build the initial checkpoint before persisting it"
    );
    assert!(
        persist < enqueue,
        "{name} must persist the initial checkpoint before Queue delivery can progress"
    );
    assert!(
        route[persist..enqueue].contains(".await"),
        "{name} must wait for the checkpoint write before enqueueing"
    );
    assert!(
        enqueue < response,
        "{name} must not return accepted before the Queue result"
    );
}

#[test]
fn accepted_search_persists_queued_checkpoint_before_enqueue() {
    let lib = source("src/lib.rs");
    let route = section(&lib, "async fn search(", "async fn upload_nzb(");
    assert_initial_checkpoint_barrier(route, "search");
}

#[test]
fn accepted_upload_persists_queued_checkpoint_before_enqueue() {
    let lib = source("src/lib.rs");
    let route = section(&lib, "async fn upload_nzb(", "async fn job_status(");
    assert_initial_checkpoint_barrier(route, "upload_nzb");
}

#[test]
fn initial_checkpoint_starts_queued_without_consumer_progress() {
    let consumer = source("src/queue_consumer.rs");
    let initial = section(
        &consumer,
        "fn initial_checkpoint(",
        "fn checkpoint_matches(",
    );
    for field in [
        "state: JobState::Queued",
        "attempt: 1",
        "repair_rounds: 0",
        "failure_reason: None",
    ] {
        assert!(
            initial.contains(field),
            "initial checkpoint is missing {field:?}"
        );
    }
}

#[test]
fn job_status_reads_the_checkpoint_instead_of_reporting_a_missing_job() {
    let lib = source("src/lib.rs");
    let status = section(&lib, "async fn job_status(", "#[cfg(test)]");

    for behavior in [
        "checkpoint::get",
        "Ok(Some(checkpoint)) => Response::from_json(&checkpoint)",
        "Ok(None) => json_error(\"unknown job id\", 404)",
    ] {
        assert!(
            status.contains(behavior),
            "job status handler is missing {behavior:?}"
        );
    }
    assert!(
        !status.contains("not_wired"),
        "job status must not expose a placeholder response"
    );
}
