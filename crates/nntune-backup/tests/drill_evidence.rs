//! Evidence-contract tests: the drill output that lands on the card
//! carries timestamps, object counts and the hash match — and nothing
//! else. A new secret-adjacent field fails here before it can leak.

use std::collections::BTreeSet;

use nntune_backup::drill;

#[test]
fn evidence_field_set_is_pinned() {
    let pt: Vec<u8> = (0..drill::MIN_FIXTURE_BYTES as usize)
        .map(|i| (i % 251) as u8)
        .collect();
    let (ev, bundle_dir, _) = drill::run_synthetic_drill(&pt, "contract-test").unwrap();
    let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
    let got: BTreeSet<String> = v.as_object().unwrap().keys().cloned().collect();
    let want: BTreeSet<String> = [
        "tool",
        "bundle_format_version",
        "env",
        "started_at_unix",
        "finished_at_unix",
        "tables",
        "blob_bytes",
        "cipher_blocks",
        "keyring_versions",
        "plaintext_bytes",
        "plaintext_sha256",
        "restored_sha256",
        "hash_match",
        "bundle_manifest_sha256",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, want);
    assert!(ev.hash_match);
    assert_eq!(ev.env, "synthetic");
    assert!(ev.finished_at_unix >= ev.started_at_unix);
    let _ = std::fs::remove_dir_all(bundle_dir.parent().unwrap());
}

#[test]
fn evidence_never_carries_article_or_key_references() {
    let pt: Vec<u8> = (0..drill::MIN_FIXTURE_BYTES as usize)
        .map(|i| (i % 251) as u8)
        .collect();
    let (ev, bundle_dir, _) = drill::run_synthetic_drill(&pt, "contract-test").unwrap();
    let json = serde_json::to_string(&ev).unwrap().to_lowercase();
    for forbidden in [
        "message-id",
        "article",
        "subject",
        "segment",
        "nzb",
        "password",
        "secret",
        "kek_value",
        "dek_value",
        "nonce",
        "ciphertext",
        "wrap",
    ] {
        assert!(!json.contains(forbidden), "evidence leaks {forbidden}");
    }
    let _ = std::fs::remove_dir_all(bundle_dir.parent().unwrap());
}
