//! Pin scratch-manifest hostile sizes fail closed.
//!
//! [`parse_manifest`] rejects absurd `size` values with 400
//! `invalid_manifest` before any streaming: two files at `u64::MAX` each
//! (checked-add overflow) and a single `u64::MAX` file (over the total
//! ceiling) both fail, while small sizes still parse.

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair::contract::parse_manifest;

fn manifest_json_for_sizes(sizes: &[u64]) -> Vec<u8> {
    let files: Vec<serde_json::Value> = sizes
        .iter()
        .enumerate()
        .map(|(i, size)| {
            let is_last = i == sizes.len() - 1;
            let file_name = if is_last {
                "set.par2".to_string()
            } else {
                format!("track{i:03}.flac")
            };
            let role = if is_last { "par2" } else { "data" };
            serde_json::json!({
                "file_name": file_name,
                "key": object_key_for_file_name(&file_name),
                "size": size,
                "role": role,
            })
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({
        "staging_id": "staging-0000000000000001",
        "files": files,
    }))
    .unwrap()
}

#[test]
fn two_max_sizes_fail_closed() {
    let body = manifest_json_for_sizes(&[u64::MAX, u64::MAX]);
    let err = parse_manifest(&body).unwrap_err();
    assert_eq!((err.status, err.code), (400, "invalid_manifest"));
}

#[test]
fn single_max_size_fails() {
    let body = manifest_json_for_sizes(&[u64::MAX]);
    let err = parse_manifest(&body).unwrap_err();
    assert_eq!((err.status, err.code), (400, "invalid_manifest"));
}

#[test]
fn small_sizes_parse() {
    let body = manifest_json_for_sizes(&[7, 8]);
    assert!(parse_manifest(&body).is_ok());
}
