//! Shared producer/consumer wire tests; no Worker I/O or provider operation.

use nntune_core::pool_operation::{
    OperationReply, Output, Served, INGEST_BODY_PATH, INGEST_STAT_PATH,
};
use nntune_ingest::checkpoint::initial;
use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::fixtures::{SMALL_NZB, SMALL_NZB_SEGMENTS};
use nntune_ingest::jobs::JobCheckpoint;
use nntune_ingest::pool_attribution::{PoolObservations, MAX_OBSERVED_PROVIDERS};
use nntune_ingest::pool_body::{
    classify_account_body_reply, BodyFetch, PoolBodyError, POOL_BODY_URL,
};
use nntune_ingest::pool_probe::{classify_account_probe_reply, PoolProbeError, POOL_PROBE_URL};
use nntune_ingest::probe::{build_probe_request, ProbeRequest};

const ID: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";

fn req() -> ProbeRequest {
    let msg = build_upload_dispatch("user_one", SMALL_NZB).unwrap();
    build_probe_request(&msg, 7, &SMALL_NZB_SEGMENTS).unwrap()
}

#[test]
fn paths_match_shared_ingress_not_legacy_user_provider_routes() {
    assert_eq!(
        POOL_BODY_URL,
        format!("https://nntp-pool{INGEST_BODY_PATH}")
    );
    assert_eq!(
        POOL_PROBE_URL,
        format!("https://nntp-pool{INGEST_STAT_PATH}")
    );
}

#[test]
fn body_round_trip_preserves_bytes_and_actual_account() {
    let reply = OperationReply::Served(Served {
        provider_id: OTHER.into(),
        output: Output::Body(vec![0, 255, 17]),
    })
    .into_response();
    let fetched = classify_account_body_reply(
        reply.status,
        reply.content_type,
        reply.provider_id.as_deref(),
        None,
        reply.body,
    )
    .unwrap();
    assert_eq!(fetched.provider_id.as_deref(), Some(OTHER));
    assert_eq!(fetched.outcome, BodyFetch::Bytes(vec![0, 255, 17]));
}

#[test]
fn body_success_without_canonical_account_or_binary_type_is_rejected() {
    for id in [
        None,
        Some(""),
        Some("not-an-account"),
        Some("AAAAAAAA-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
    ] {
        assert_eq!(
            classify_account_body_reply(200, "application/octet-stream", id, None, vec![1]),
            Err(PoolBodyError::Rejected)
        );
    }
    assert_eq!(
        classify_account_body_reply(200, "audio/flac", Some(ID), None, vec![1]),
        Err(PoolBodyError::Rejected)
    );
    assert_eq!(
        classify_account_body_reply(
            200,
            "application/json",
            Some(ID),
            None,
            br#"{"present":[],"missing":[]}"#.to_vec()
        ),
        Err(PoolBodyError::Rejected)
    );
}

#[test]
fn body_data_outcomes_have_no_attribution_and_route_miss_is_not_missing() {
    for (status, expected) in [(410, BodyFetch::Missing), (413, BodyFetch::TooLarge)] {
        let fetched = classify_account_body_reply(
            status,
            "anything",
            Some(ID),
            None,
            b"provider text".to_vec(),
        )
        .unwrap();
        assert!(fetched.provider_id.is_none());
        assert_eq!(fetched.outcome, expected);
    }
    assert_eq!(
        classify_account_body_reply(
            404,
            "application/json",
            Some(ID),
            None,
            br#"{"error":"missing"}"#.to_vec()
        ),
        Err(PoolBodyError::Retry { after_secs: 5 })
    );
    assert_eq!(
        classify_account_body_reply(503, "anything", Some(ID), Some("99999"), vec![]),
        Err(PoolBodyError::Retry { after_secs: 300 })
    );
    assert_eq!(
        classify_account_body_reply(401, "anything", Some(ID), Some("99999"), vec![]),
        Err(PoolBodyError::Rejected)
    );
}

#[test]
fn body_reply_size_cap_is_shared() {
    let fetched = classify_account_body_reply(
        200,
        "application/octet-stream",
        Some(ID),
        None,
        vec![0; nntune_core::pool_operation::MAX_BODY_BYTES],
    )
    .unwrap();
    assert!(matches!(fetched.outcome, BodyFetch::Bytes(_)));
    assert_eq!(
        classify_account_body_reply(
            200,
            "application/octet-stream",
            Some(ID),
            None,
            vec![0; nntune_core::pool_operation::MAX_BODY_BYTES + 1]
        ),
        Err(PoolBodyError::Rejected)
    );
}

#[test]
fn stat_round_trip_correlates_partition_and_retains_actual_account() {
    let req = req();
    let reply = OperationReply::Served(Served {
        provider_id: OTHER.into(),
        output: Output::Stat {
            present: vec![req.segment_ids[0].clone()],
            missing: vec![req.segment_ids[1].clone()],
        },
    })
    .into_response();
    let served = classify_account_probe_reply(
        &req,
        reply.status,
        reply.content_type,
        reply.provider_id.as_deref(),
        None,
        reply.body,
    )
    .unwrap();
    assert_eq!(served.provider_id, OTHER);
    assert_eq!(served.outcome.batch_index, req.batch_index);
    assert_eq!(served.outcome.present, vec![req.segment_ids[0].clone()]);
}

#[test]
fn duplicate_stat_occurrences_can_change_availability() {
    let mut req = req();
    req.segment_ids = vec![req.segment_ids[0].clone(); 2];
    let reply = OperationReply::Served(Served {
        provider_id: ID.into(),
        output: Output::Stat {
            present: vec![req.segment_ids[0].clone()],
            missing: vec![req.segment_ids[0].clone()],
        },
    })
    .into_response();
    assert!(classify_account_probe_reply(
        &req,
        200,
        reply.content_type,
        Some(ID),
        None,
        reply.body
    )
    .is_ok());
}

#[test]
fn stat_unattributed_foreign_short_padded_unknown_duplicate_and_array_answers_refuse() {
    let req = req();
    let body = serde_json::json!({"present": req.segment_ids, "missing":[]})
        .to_string()
        .into_bytes();
    assert_eq!(
        classify_account_probe_reply(&req, 200, "application/json", None, None, body),
        Err(PoolProbeError::Rejected)
    );
    for value in [
        serde_json::json!({"present":["foreign@example.invalid"],"missing":[req.segment_ids[1]]}),
        serde_json::json!({"present":[req.segment_ids[0]],"missing":[]}),
        serde_json::json!({"present":req.segment_ids,"missing":[req.segment_ids[0]]}),
        serde_json::json!({"present":req.segment_ids,"missing":[],"batch_index":7}),
        serde_json::json!([req.segment_ids, []]),
    ] {
        assert_eq!(
            classify_account_probe_reply(
                &req,
                200,
                "application/json",
                Some(ID),
                None,
                serde_json::to_vec(&value).unwrap()
            ),
            Err(PoolProbeError::Rejected)
        );
    }
    let raw = br#"{"present":[],"present":[],"missing":[]}"#.to_vec();
    assert_eq!(
        classify_account_probe_reply(&req, 200, "application/json", Some(ID), None, raw),
        Err(PoolProbeError::Rejected)
    );
}

#[test]
fn stat_failures_never_record_attribution_or_parse_error_text() {
    let req = req();
    assert_eq!(
        classify_account_probe_reply(&req, 503, "anything", Some(ID), Some("0"), vec![255]),
        Err(PoolProbeError::Retry { after_secs: 1 })
    );
    assert_eq!(
        classify_account_probe_reply(&req, 403, "anything", Some(ID), Some("99"), vec![255]),
        Err(PoolProbeError::Rejected)
    );
}

#[test]
fn checkpoint_does_not_treat_queued_hint_or_first_observed_account_as_whole_job_source() {
    let mut msg = build_upload_dispatch("user_one", SMALL_NZB).unwrap();
    msg.provider_id = Some(ID.into());
    let mut cp = initial(&msg);
    assert!(cp.provider_id.is_none());
    cp.pool_observations.record(OTHER).unwrap();
    assert!(cp.provider_id.is_none());
    cp.pool_observations.record(ID).unwrap();
    cp.pool_observations.record(ID).unwrap();
    assert_eq!(cp.pool_observations.provider_ids.len(), 2);
    assert!(cp.provider_id.is_none());
    let restored: JobCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&cp).unwrap()).unwrap();
    assert_eq!(restored, cp);
}

#[test]
fn old_checkpoint_loads_without_fabricated_observations() {
    let cp = initial(&build_upload_dispatch("user_one", SMALL_NZB).unwrap());
    let mut value = serde_json::to_value(cp).unwrap();
    value.as_object_mut().unwrap().remove("pool_observations");
    let restored: JobCheckpoint = serde_json::from_value(value).unwrap();
    assert_eq!(restored.pool_observations, PoolObservations::default());
}

#[test]
fn observations_union_deduplicates_rejects_bad_ids_and_caps_growth() {
    let mut a = PoolObservations::default();
    assert!(a.record("bad-account").is_err());
    for n in 0..MAX_OBSERVED_PROVIDERS {
        a.record(&format!("{n:08x}-1234-4234-8234-123456789abc"))
            .unwrap();
    }
    assert!(!a.truncated);
    let mut b = PoolObservations::default();
    b.record(ID).unwrap();
    a.merge(&b);
    assert!(a.truncated);
    assert_eq!(a.provider_ids.len(), MAX_OBSERVED_PROVIDERS);
    let round_trip: PoolObservations =
        serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert_eq!(a, round_trip);
}
