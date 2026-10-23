//! Container fetch deadline wiring: one 10 s base knob for small
//! exchanges, the 330 s knob for `/transcode`, size-scaled budgets
//! for streamed bodies, and deadline-first races everywhere.
//!
//! `runtime` compiles only for `wasm32`, so these pins read its
//! source (normalised) instead of calling it.

const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

fn normalized() -> String {
    RUNTIME_RS.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn deadline_knobs_are_single_named_constants() {
    let text = normalized();
    assert!(
        text.contains("const CONTAINER_FETCH_DEADLINE_MS: u64 = 10_000;"),
        "small exchanges must arm from the single 10 s knob"
    );
    assert!(
        text.contains("const CONTAINER_TRANSCODE_DEADLINE_MS: u64 = 330_000;"),
        "the encode exchange must arm from the single 330 s knob"
    );
    // `/transcode` arms the long knob, everything else the base.
    assert!(
        text.contains(
            "if path == \"/transcode\" { CONTAINER_TRANSCODE_DEADLINE_MS } \
             else { CONTAINER_FETCH_DEADLINE_MS }"
        ),
        "container_json must select the deadline by path"
    );
}

#[test]
fn streamed_bodies_scale_atop_the_base() {
    let text = normalized();
    // ~1 MB/s floor: a large healthy transfer must not expire
    // mid-stream, while retries restart from byte 0.
    assert!(
        text.contains("CONTAINER_FETCH_DEADLINE_MS.saturating_add(size.saturating_div(1024))"),
        "stage_input must scale the budget with the input size"
    );
    assert!(
        text.contains(
            "CONTAINER_FETCH_DEADLINE_MS.saturating_add(expected_size.saturating_div(1024))"
        ),
        "fetch_artifact must scale the budget with the validated reply size"
    );
}

#[test]
fn every_race_polls_the_deadline_first() {
    let text = normalized();
    // Six raced awaits: stage_input upload, fetch_artifact fetch +
    // capped read + R2 put, container_json fetch + capped read. Each
    // must poll the deadline before the operation so an already-fired
    // deadline aborts even a ready future.
    let races = text
        .matches("Future::poll(deadline.as_mut(), cx).is_ready()")
        .count();
    assert_eq!(
        races, 6,
        "all six Container races must poll the deadline first"
    );
}
