//! Reply-site caps: every Container reply and artifact read streams
//! through the capped reader — no unbounded `.bytes()` before the cap.
//!
//! `runtime` compiles only for `wasm32`, so these pins read its
//! source (normalised) instead of calling it.

const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

fn normalized() -> String {
    RUNTIME_RS.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Source slice of one `async fn` (up to the next `async fn`).
fn fn_slice(name: &str) -> String {
    let text = normalized();
    let start = text
        .find(&format!("async fn {name}"))
        .unwrap_or_else(|| panic!("runtime.rs has no async fn {name}"));
    let rest = &text[start + 10..];
    let end = rest.find("async fn ").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn container_json_reads_through_the_capped_reader() {
    let helper = fn_slice("container_json");
    assert!(
        helper.contains("read_response_capped(&mut response, MAX_RESULT_BYTES)"),
        "container_json must read through read_response_capped with MAX_RESULT_BYTES"
    );
    assert!(
        !helper.contains(".bytes()"),
        "container_json must not materialise an unbounded .bytes() body"
    );
}

#[test]
fn artifact_fetch_reads_through_the_artifact_cap() {
    let helper = fn_slice("fetch_artifact");
    // The 32 MiB artifact ceiling — not the 1 MiB result cap — bounds
    // this read; anything else either rejects valid artifacts or
    // buffers past the profile ceiling.
    assert!(
        helper.contains("read_response_capped(&mut response, MAX_ARTIFACT_BYTES)"),
        "fetch_artifact must read through read_response_capped with MAX_ARTIFACT_BYTES"
    );
    assert!(
        !helper.contains(".bytes()"),
        "fetch_artifact must not materialise an unbounded .bytes() body"
    );
}

#[test]
fn scratch_marker_read_checks_metadata_before_materialising() {
    let helper = fn_slice("read_object_capped");
    // The R2 size check must precede the only `.bytes()` call: the
    // boundary is strict `>`, so exactly-at-cap stays admissible.
    let check = helper
        .find("object.size() > cap")
        .expect("read_object_capped must compare object.size() against cap");
    let materialise = helper
        .find(".bytes()")
        .expect("read_object_capped must materialise via .bytes()");
    assert!(
        check < materialise,
        "the metadata size check must precede .bytes()"
    );
    let branch_start = helper[check..].find('{').unwrap() + check + 1;
    let mut depth = 1;
    let branch_end = helper[branch_start..]
        .char_indices()
        .find_map(|(offset, ch)| {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(branch_start + offset)
        })
        .expect("metadata oversize branch must close");
    let branch = &helper[branch_start..branch_end];
    assert!(
        branch_end < materialise,
        "oversize marker must return before .bytes()"
    );
    assert_eq!(
        branch.trim(),
        "return Err(ApiFault::new(500, \"transcode_unavailable\"));",
        "stored-marker oversize is a retryable server fault, not a permanent request-body 413"
    );
}
