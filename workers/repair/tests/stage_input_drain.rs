//! Acceptance tests: `stage_input` must cancel the Container
//! PUT reply body (or read it under a small fixed cap) instead of
//! collecting it whole and throwing the collection away.
//!
//! Contract under test (`workers/repair/src/runtime.rs`, `stage_input`):
//! the PUT's reply body is never used — only its status is — so the
//! reply must be disposed of without retaining it: cancelled/aborted,
//! dropped unread, or streamed under a small fixed cap. Collecting it
//! with a whole-body read (`bytes()`/`text()`/`json()` on
//! `worker::Response` folds the entire stream into one allocation)
//! retains an unbounded, attacker-influenced (Container-chosen) body
//! only to discard it, and also stalls the round until the whole body
//! has arrived.
//!
//! The `runtime` module compiles only for `wasm32`, so a native
//! integration target cannot drive it; following
//! `health_budget_literals_acceptance.rs`, these pins read `runtime.rs`
//! as source text via `include_str!` and scope assertions to the
//! `stage_input` function (whitespace-normalised, so `rustfmt` layout
//! is irrelevant). Anything in the function that collects, or the
//! absence of any cancel/cap disposal, turns a pin red regardless of
//! which of the accepted disposal shapes the implementation picks.
//!
//! RED on the base SHA (the drain is `let _ = response.bytes().await;`):
//!
//! - `stage_input_does_not_collect_the_put_reply_body`
//! - `stage_input_cancels_or_caps_the_put_reply_body`
//! - `runtime_has_no_fourth_whole_body_collect` (guards against the
//!   collect simply moving into a helper)
//!
//! Green controls (adjacent behaviour that must not move):
//!
//! - the `201` / `400 | 413` / other status mapping;
//! - the PUT request shape (method, explicit length, input checks);
//! - the neighbouring capped whole-body readers, `container_json`'s
//!   streamed cap, and `fetch_output`'s streaming copy.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-repair --test stage_input_drain
//! ```

/// `runtime.rs` source, read as text (NOT a crate import — the module is
/// `cfg(target_arch = "wasm32")` and unreachable from native tests).
const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

/// Collapse every whitespace run to one space so pins match the token
/// stream, not the formatting.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The whitespace-normalised text of one function: from its signature
/// marker up to (not including) the next function's marker.
fn fn_text(start_marker: &str, end_marker: &str) -> String {
    let start = RUNTIME_RS
        .find(start_marker)
        .unwrap_or_else(|| panic!("runtime.rs no longer contains `{start_marker}`"));
    let rest = &RUNTIME_RS[start..];
    let end = rest.find(end_marker).unwrap_or_else(|| {
        panic!("runtime.rs no longer contains `{end_marker}` after `{start_marker}`")
    });
    normalized(&rest[..end])
}

fn stage_input() -> String {
    fn_text("async fn stage_input", "async fn fetch_output")
}

fn container_json() -> String {
    fn_text("async fn container_json", "async fn read_object_capped")
}

#[test]
fn stage_input_does_not_collect_the_put_reply_body() {
    let body = stage_input();
    for collector in [".bytes()", ".text()", ".json()"] {
        assert!(
            !body.contains(collector),
            "stage_input must not drain the Container PUT reply with `{collector}`: \
             on worker::Response that folds the whole (unbounded, unused) reply \
             body into one retained allocation before discarding it; cancel the \
             body or read it under a small fixed cap instead"
        );
    }
}

#[test]
fn stage_input_cancels_or_caps_the_put_reply_body() {
    let body = stage_input().to_ascii_lowercase();
    // Cancel side: an explicit stream/fetch cancellation, or dropping
    // the unread body/response/stream outright. Word-level markers also
    // cover a disposal helper named for what it does.
    let cancels = [
        "cancel",
        "abort",
        "drop(response",
        "drop(stream",
        "drop(body",
    ]
    .iter()
    .any(|marker| body.contains(marker));
    // Cap side: streamed reading bounded by a named cap/limit/early
    // exit, or a named cap constant / capped helper applied directly.
    let streams_bounded = body.contains(".stream()")
        && [
            "max_",
            "_cap",
            "capped",
            "limit",
            ".take(",
            "take_while",
            "break",
        ]
        .iter()
        .any(|marker| body.contains(marker));
    let named_cap = ["max_", "_cap", "capped"]
        .iter()
        .any(|marker| body.contains(marker));
    assert!(
        cancels || streams_bounded || named_cap,
        "stage_input must dispose of the Container PUT reply by cancelling it \
         (cancel/abort/drop of the unread body) or by reading it under a small \
         fixed cap; found neither in stage_input"
    );
}

#[test]
fn runtime_has_no_fourth_whole_body_collect() {
    // The single legitimate whole-body collect, under an explicit
    // size contract: read_object_capped (R2 size checked against
    // its cap before the read). Both result replies — the front-door
    // DO reply and container_json — stream through the shared
    // read_response_capped helper (check-before-extend via
    // push_chunk_capped), so neither keeps a `.bytes()` by design.
    // A second `.bytes()` is the stage_input drain — possibly moved
    // into a helper — or a front-door/container_json regression,
    // and must go.
    let collects = RUNTIME_RS.matches(".bytes()").count();
    assert_eq!(
        collects, 1,
        "runtime.rs must keep exactly the 1 capped `.bytes()` collect \
         (read_object_capped); a 2nd is an uncapped collect \
         (stage_input drain) or a front-door/container_json regression \
         to whole-body buffering instead of streaming under the cap"
    );
    let body = container_json();
    assert!(
        body.contains("read_response_capped") && body.contains("MAX_RESULT_BYTES"),
        "container_json must read the Container-chosen reply through the \
         shared capped reader bounded by MAX_RESULT_BYTES, not a \
         whole-body collect"
    );
    assert!(
        !body.contains(".bytes()") && !body.contains(".text()"),
        "container_json must not whole-collect the Container-chosen reply \
         with `.bytes()`/`.text()` before checking the cap"
    );
    let helper = fn_text("async fn read_response_capped", "async fn fetch");
    assert!(
        helper.contains(".stream()") && helper.contains("push_chunk_capped"),
        "read_response_capped must be the streamed reader: `.stream()` chunks \
         admitted via push_chunk_capped (check-before-extend), so at most \
         the cap is ever materialised"
    );
}

#[test]
fn stage_input_status_mapping_is_unchanged() {
    let body = stage_input();
    assert!(
        body.contains("let status = response.status_code();"),
        "stage_input must still key its verdict on the reply status"
    );
    assert!(
        body.contains("match status { 201 => Ok(())"),
        "201 must still mean the input was staged"
    );
    assert!(
        body.contains("400 | 413 => Err(ApiFault::new(status, \"scratch_input_mismatch\"))"),
        "400/413 must still map to scratch_input_mismatch at their own status"
    );
    assert!(
        body.contains("_ => Err(ApiFault::new(502, \"container_unavailable\"))"),
        "any other status must still be the retryable 502 container_unavailable"
    );
}

#[test]
fn stage_input_put_request_shape_is_unchanged() {
    let body = stage_input();
    assert!(
        body.contains("if object.size() != file.size"),
        "the staged-object size check against the manifest must stay"
    );
    assert!(
        body.contains(".with_method(Method::Put)"),
        "inputs must still be staged with PUT"
    );
    assert!(
        body.contains("headers.set(\"content-length\", &file.size.to_string())"),
        "the PUT must still carry the explicit content-length"
    );
    assert!(
        body.contains("\"http://container/jobs/{}/r{}/in/{}\""),
        "the PUT must still target the per-job/per-round input path"
    );
    assert!(
        body.contains("container .fetch_request(request)"),
        "the PUT must still go through the Container fetcher"
    );
}

#[test]
fn neighbouring_body_readers_still_collect_under_their_caps() {
    let whole = normalized(RUNTIME_RS);
    // The front-door DO reply streams through the shared capped
    // reader instead of an inline whole-body guard.
    let front = fn_text("#[event(fetch)]", "pub struct RepairContainer");
    assert!(
        front.contains("read_response_capped") && front.contains("MAX_RESULT_BYTES"),
        "the DO reply forwarded to ingest must stay size-checked: read it \
         through the shared capped reader bounded by MAX_RESULT_BYTES"
    );
    assert!(
        !front.contains(".bytes()") && !front.contains(".text()"),
        "the front-door DO reply must stream under the cap, not buffer \
         whole with `.bytes()`/`.text()` before checking it"
    );
    let helper = fn_text("async fn read_response_capped", "async fn fetch");
    assert!(
        helper.contains(".stream()") && helper.contains("push_chunk_capped"),
        "read_response_capped must cap while streaming: read chunks via \
         `.stream()` and admit them via push_chunk_capped so the cap \
         bounds the read itself"
    );
    let container_json = container_json();
    assert!(
        container_json.contains("read_response_capped")
            && container_json.contains("MAX_RESULT_BYTES")
            && container_json.contains("502")
            && container_json.contains("container_unavailable"),
        "container_json must cap while streaming: delegate to \
         read_response_capped bounded by MAX_RESULT_BYTES and map \
         over-cap/unreadable to 502 container_unavailable"
    );
    assert!(
        !container_json.contains(".bytes()") && !container_json.contains(".text()"),
        "container_json must not buffer the whole reply (`.bytes()`/`.text()`) \
         before rejecting it over the cap"
    );
    assert!(
        whole.contains("if object.size() > cap as u64"),
        "read_object_capped must keep checking R2 metadata before reading"
    );
    let fetch_output = fn_text("async fn fetch_output", "fn leak_code");
    assert!(
        fetch_output.contains(".stream()")
            && fetch_output.contains("FixedLengthStream::wrap(stream, output.size)"),
        "fetch_output must keep streaming outputs into R2 at a fixed length"
    );
    assert!(
        !fetch_output.contains(".bytes()"),
        "fetch_output must not start collecting repaired outputs whole"
    );
}
