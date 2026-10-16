//! Acceptance contracts for metered ingest admission.
//!
//! These source-contract checks intentionally fail until both routes call one
//! durable admission boundary. The boundary must own the (job_id, stage)
//! ledger/outbox transaction and only report queued after Queue acceptance.
//! Analytics Engine remains a projection; it is not the dedupe authority.
//!
//! The current crate exposes no durable admission seam to an integration test,
//! and its route response helper is private. These checks pin the required
//! wiring without pretending to exercise unimplemented runtime behavior. The
//! implementation must add behavioral ledger/reload/concurrency/failure tests
//! at the new seam; the existing meter_edge_write tests retain the exact event
//! shape and WORKFLOW_STEPS contract.

use std::fs;
use std::path::PathBuf;

fn source(path: &str) -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest.join(path)).expect("read ingest source")
}

fn function_body<'a>(text: &'a str, signature: &str) -> &'a str {
    let start = text
        .find(signature)
        .unwrap_or_else(|| panic!("missing route/helper signature: {signature}"));
    let open = text[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("function body starts");
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + 1..index];
                }
            }
            _ => {}
        }
    }
    panic!("unclosed function body: {signature}");
}

#[test]
fn search_and_upload_use_the_durable_idempotent_admission_boundary() {
    let lib = source("src/lib.rs");
    let search = function_body(&lib, "async fn search(");
    let upload = function_body(&lib, "async fn upload_nzb(");

    for (name, route) in [("search", search), ("upload", upload)] {
        assert!(
            route.contains("admit_metered_job("),
            "{name} route must use the durable idempotent admission boundary"
        );
        assert!(
            !route.contains("try_meter(") && !route.contains("try_enqueue("),
            "{name} route must not independently meter/enqueue around the ledger"
        );
    }

    assert!(
        lib.contains("trait AdmissionLedger")
            && lib.contains("record_stage_once")
            && lib.contains("job_id")
            && lib.contains("stage"),
        "admission must use an authoritative durable ledger keyed by (job_id, stage)"
    );
    assert!(
        lib.contains("pending") && lib.contains("queued"),
        "a durable outbox must distinguish pending delivery from Queue acceptance"
    );
}

#[test]
fn durable_admission_contract_covers_reload_races_and_failure_boundaries() {
    let lib = source("src/lib.rs");

    // The named seam is required to be backed by a durable ledger/outbox, not
    // a per-process set or eventually-consistent KV marker. Its tests must
    // exercise reload, concurrent insert, commit failure, and send failure.
    for contract in [
        "AdmissionLedger",
        "record_stage_once",
        "reload",
        "competing",
        "commit_failure",
        "queue_failure",
        "MeterOutcome::Ok",
    ] {
        assert!(
            lib.contains(contract),
            "durable admission boundary/test seam must cover {contract}"
        );
    }
    assert!(
        !lib.contains("Analytics Engine dedupe")
            && !lib.contains("HashSet<String>"),
        "telemetry projection or process-local state must not be the idempotency authority"
    );
}

#[test]
fn source_to_stage_helper_maps_sources_to_workflow_steps() {
    let lib = source("src/lib.rs");
    let helper = function_body(&lib, "fn meter_entry_stage(");
    let queue = source("src/queue.rs");

    assert!(helper.contains("JobSource::IndexerSearch => \"search\""));
    assert!(helper.contains("JobSource::NzbUpload => \"parse\""));
    assert!(queue.contains("\"search\", \"parse\", \"probe\", \"verify\", \"repair\", \"match\", \"write\"));
    assert!(
        function_body(&lib, "async fn search(").contains("meter_entry_stage(msg.source)")
            && function_body(&lib, "async fn upload_nzb(")
                .contains("meter_entry_stage(msg.source)"),
        "both routes must exercise the source-to-stage helper"
    );
}

#[test]
fn meter_http_mapping_is_consumed_by_route_error_response() {
    let lib = source("src/lib.rs");
    let response = function_body(&lib, "fn meter_failure_response(");
    let unavailable = function_body(&lib, "fn service_unavailable(");

    assert!(
        response.contains("err.http_status()"),
        "the actual route response must derive its status from MeterWriteError::http_status()"
    );
    assert!(
        unavailable.contains("retry-after") && unavailable.contains("503"),
        "retryable meter failure must retain 503 and Retry-After"
    );
}
