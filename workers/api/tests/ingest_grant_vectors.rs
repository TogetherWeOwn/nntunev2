//! TWO-331: api-minted ingest caller grants match the shared vectors that
//! ingest's verifier pins (`workers/ingest/src/caller_auth.rs`), so a drift
//! on either side fails that side's test.

use nntune_api::ingest_grant::{issue_at, MAX_GRANT_TTL};

#[test]
fn api_mints_the_shared_vectors_byte_for_byte() {
    let doc: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/ingest-caller-grant/vectors.json"
    ))
    .expect("vectors parse");
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(vectors.len() >= 4);
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        let now = v["now_unix"].as_u64().unwrap();
        let ttl = v["ttl_secs"].as_u64().unwrap();
        let exp = v["exp_unix"].as_u64().unwrap();
        assert_eq!(exp, now + ttl.min(MAX_GRANT_TTL), "{name}: exp");
        let token = issue_at(
            v["user_id"].as_str().unwrap(),
            now,
            ttl,
            Some(v["secret"].as_str().unwrap()),
        )
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(token, v["token"].as_str().unwrap(), "{name}");
    }
}
