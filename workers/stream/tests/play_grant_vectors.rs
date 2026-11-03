//! Shared play-grant vectors: the api Worker's minter
//! (`play_grant::issue_v2_at`) and this crate's `grant` module must
//! agree byte-exactly on the v2 token shape. `fixtures/play-grant/
//! vectors.json` was generated with Python's hmac (independent of both
//! crates); the api suite pins minting against it and this suite pins
//! both minting and verification, so a format drift on either side
//! fails instead of silently refusing every play at the route.

use nntune_stream::grant::{issue_v2_at, verify, GrantError};

fn vectors() -> Vec<serde_json::Value> {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/play-grant/vectors.json"))
            .expect("vectors parse");
    let vectors = doc["vectors"].as_array().expect("vectors array").clone();
    assert!(!vectors.is_empty(), "vector file must not be empty");
    vectors
}

#[test]
fn stream_issue_v2_at_reproduces_the_shared_vectors() {
    for v in vectors() {
        let name = v["name"].as_str().expect("name");
        let token = issue_v2_at(
            v["user_id"].as_str().expect("user"),
            v["track_id"].as_str().expect("track"),
            v["play_id"].as_str().expect("play"),
            v["lease_epoch"].as_u64().expect("epoch"),
            v["now_unix"].as_u64().expect("now"),
            v["ttl_secs"].as_u64().expect("ttl"),
            v["secret"].as_str().expect("secret").as_bytes(),
        );
        assert_eq!(
            token,
            v["token"].as_str().expect("token"),
            "{name}: stream issuer must reproduce the shared vector byte-exactly"
        );
    }
}

#[test]
fn stream_verify_accepts_the_shared_vectors() {
    for v in vectors() {
        let name = v["name"].as_str().expect("name");
        let secret = v["secret"].as_str().expect("secret").as_bytes();
        let token = v["token"].as_str().expect("token");
        let now = v["now_unix"].as_u64().expect("now");
        let grant = verify(token, secret, now)
            .unwrap_or_else(|e| panic!("{name}: vector must verify at issuance: {e:?}"));
        assert_eq!(
            grant.user_id,
            v["user_id"].as_str().expect("user"),
            "{name}"
        );
        assert_eq!(
            grant.track_id,
            v["track_id"].as_str().expect("track"),
            "{name}"
        );
        assert_eq!(
            grant.play_id.as_deref(),
            Some(v["play_id"].as_str().expect("play")),
            "{name}"
        );
        assert_eq!(
            grant.lease_epoch,
            Some(v["lease_epoch"].as_u64().expect("epoch")),
            "{name}"
        );
        assert_eq!(
            grant.exp_unix,
            v["exp_unix"].as_u64().expect("exp"),
            "{name}"
        );
        // The same token is dead at its expiry instant.
        assert_eq!(
            verify(token, secret, grant.exp_unix),
            Err(GrantError::Expired),
            "{name}: expired at exp"
        );
    }
}
