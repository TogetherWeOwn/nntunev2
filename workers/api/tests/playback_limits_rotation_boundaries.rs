use futures_util::FutureExt;
use nntune_api::{
    handlers::StoreError,
    meter_sink::{self, SinkError},
    original_source::{
        read_original_source, FieldEnvelope, SourceMetadata, SourceReadAuthorization,
        SourceRepository, SourceServiceError,
    },
    plan_limits, play_grant,
    playback::{LeaseHeartbeat, LeaseRow},
    rate_limit::{check_dual_throttle, ThrottleDecision, TokenBucket},
    rotation::{
        run_rewrap_pass, validate_new_run, CasRewrapOutcome, JobError, KekSource, RewrapClaim,
        RewrapParams, RewrapStore, RewrappedEnvelope, RunRecord,
    },
    target_identity::{canonical_target_id, valid_target_id},
    vault::encrypt_vault,
};
use serde_json::json;
use std::cell::{Cell, RefCell};

const NOW: u64 = 1_800_000_000;
const KEY: &[u8; 32] = &[7; 32];

#[test]
fn meter_sink_rejects_timestamp_aliases() {
    let body = br#"{"windows":[]}"#;
    let header = meter_sink::authorization_header(Some(KEY), NOW, body).unwrap();
    assert!(meter_sink::verify(Some(KEY), Some(&header), body, NOW).is_ok());
    for prefix in ["+", "0"] {
        let alias = header.replacen("MeterSink ", &format!("MeterSink {prefix}"), 1);
        assert_eq!(
            meter_sink::verify(Some(KEY), Some(&alias), body, NOW),
            Err(SinkError::Unauthorized)
        );
    }
}

#[test]
fn meter_sink_refuses_ambiguous_or_oversized_batches() {
    let first = json!({
        "user_id": "user-a", "track_id": "track-a", "play_id": "play-a",
        "union_bytes": 20, "file_bytes": 100, "started_at": NOW
    });
    let mut second = first.clone();
    second["user_id"] = json!("user-b");
    let batch = |second: &serde_json::Value| json!({"windows": [&first, second]}).to_string();
    assert_eq!(
        meter_sink::parse_batch(batch(&second).as_bytes()),
        Err(SinkError::Malformed)
    );
    second["play_id"] = json!("play-b");
    let mut body = batch(&second).into_bytes();
    body.resize(16 * 1024, b' ');
    assert_eq!(meter_sink::parse_batch(&body).unwrap().len(), 2);
    body.push(b' ');
    assert_eq!(meter_sink::parse_batch(&body), Err(SinkError::Malformed));
}

struct Deny;

impl SourceReadAuthorization for Deny {
    fn authorize(&self, _: &str) -> Result<(), SourceServiceError> {
        Err(SourceServiceError::Denied)
    }
}

impl SourceRepository for Deny {
    fn metadata(&mut self, _: &str) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        panic!("unauthorized source must never read metadata")
    }

    fn envelope(
        &mut self,
        _: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        panic!("unauthorized source must never read the envelope")
    }
}

struct TestKeys;

impl KekSource for TestKeys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        (version <= 2).then_some(KEY)
    }
}

#[test]
fn original_source_checks_shape_before_authorization_and_storage() {
    let track = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    assert!(matches!(
        read_original_source(&Deny, &mut Deny, &TestKeys, &track.to_ascii_uppercase()),
        Err(SourceServiceError::Invalid)
    ));
    assert!(matches!(
        read_original_source(&Deny, &mut Deny, &TestKeys, track),
        Err(SourceServiceError::Denied)
    ));
}

#[test]
fn overflowing_plan_hours_grant_no_playback() {
    let max_hours = u64::MAX / 3600;
    let caps = plan_limits::caps_from_plan_row(max_hours, 1);
    assert!(plan_limits::check_stream_hours_with_caps(&caps, max_hours * 3600 - 1).is_allow());
    assert!(!plan_limits::check_stream_hours_with_caps(&caps, max_hours * 3600).is_allow());
    let invalid = plan_limits::caps_from_plan_row(max_hours + 1, 1);
    assert!(!plan_limits::check_stream_hours_with_caps(&invalid, 0).is_allow());
}

#[test]
fn leases_and_grants_require_a_persistable_epoch() {
    for epoch in [0, 1, i64::MAX as u64, i64::MAX as u64 + 1] {
        let valid = epoch == 1 || epoch == i64::MAX as u64;
        let row = LeaseRow {
            session_id: "session-a".into(),
            track_id: "track-a".into(),
            play_id: "play-a".into(),
            epoch,
            expires_at: NOW + 45,
            continuation_deadline: NOW + 20,
        };
        let continuing = row.is_continuation("session-a", "play-a", "track-a", NOW);
        assert_eq!(continuing, valid);
        assert_eq!(row.retry_after_secs(NOW), if valid { 20 } else { 0 });
        let heartbeat = if valid {
            LeaseHeartbeat::Active
        } else {
            LeaseHeartbeat::Expired
        };
        let state = LeaseHeartbeat::classify(Some(&row), "session-a", "play-a", epoch, NOW);
        assert_eq!(state, heartbeat);
        let grant =
            play_grant::issue_v2_at("user-a", "track-a", "play-a", epoch, NOW, 20, Some(KEY));
        assert_eq!(grant.is_ok(), valid);
    }
}

#[test]
fn nonfinite_bucket_settings_cannot_admit_unlimited_requests() {
    let mut ip = TokenBucket::new(f64::INFINITY, 1.0, NOW);
    let mut account = TokenBucket::new(1.0, f64::INFINITY, NOW);
    let decision = check_dual_throttle(&mut ip, &mut account, NOW);
    assert!(matches!(decision, ThrottleDecision::Deny { .. }));
    assert!(account.take(NOW, 1.0));
    assert!(!account.take(NOW + 1, 1.0));
}

#[test]
fn target_ids_keep_opaque_case_and_reject_invalid_shapes() {
    for invalid in ["", "track/one", "track\0one", "tracké", &"a".repeat(513)] {
        assert!(!valid_target_id(invalid));
    }
    assert!(valid_target_id(&"a".repeat(512)));
    assert_eq!(canonical_target_id("Job_A-7"), "Job_A-7");
    assert_eq!(
        canonical_target_id("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"),
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    );
}

#[derive(Default)]
struct RotationStore {
    batch: RefCell<Vec<RewrapClaim>>,
    reload: RefCell<Option<RewrapClaim>>,
    claims: Cell<u32>,
    writes: Cell<u32>,
}

impl RewrapStore for RotationStore {
    async fn claim_batch(
        &self,
        _: u32,
        _: Option<&str>,
        _: u32,
    ) -> Result<Vec<RewrapClaim>, StoreError> {
        self.claims.set(self.claims.get() + 1);
        Ok(self.batch.take())
    }

    async fn reload_row(&self, _: &str) -> Result<Option<RewrapClaim>, StoreError> {
        Ok(self.reload.take())
    }

    async fn cas_rewrap(
        &self,
        _: &str,
        _: &RewrapClaim,
        _: &RewrappedEnvelope,
        _: u64,
    ) -> Result<CasRewrapOutcome, StoreError> {
        self.writes.set(self.writes.get() + 1);
        Ok(CasRewrapOutcome::ClaimLost)
    }

    async fn open_run(&self, _: &str, _: u32, _: u32, _: u64) -> Result<RunRecord, JobError> {
        panic!("the pass never opens a run")
    }

    async fn load_open_run(&self, _: &str, _: u32) -> Result<Option<RunRecord>, StoreError> {
        panic!("the pass already has a run")
    }

    async fn save_progress(&self, _: &RunRecord) -> Result<(), StoreError> {
        panic!("a refused batch or reload must never save progress")
    }

    async fn close_run(&self, _: &str, _: u64) -> Result<(), StoreError> {
        panic!("the pass never closes a run")
    }
}

fn claim(user: &str, version: u32) -> RewrapClaim {
    let blob = encrypt_vault(KEY, version, user, b"synthetic").unwrap();
    RewrapClaim {
        user_id: user.into(),
        kek_version: version,
        dek_wrapped: blob.dek_wrapped_b64url,
        dek_wrapped_nonce: blob.dek_wrapped_nonce_b64url,
    }
}

fn rotation_pass(store: &RotationStore, run: &mut RunRecord, limit: u32) -> Result<(), JobError> {
    let params = RewrapParams {
        to_version: 2,
        batch_limit: limit,
        max_batches: 1,
        dry_run: false,
        now_secs: NOW,
    };
    run_rewrap_pass(store, &TestKeys, run, &params)
        .now_or_never()
        .expect("offline store completes synchronously")
        .map(|_| ())
}

fn rotation_run() -> RunRecord {
    RunRecord {
        id: "run".into(),
        ..RunRecord::transient("test", 1, 2, NOW)
    }
}

#[test]
fn rotation_checks_run_state_and_target_version() {
    let store = RotationStore::default();
    let mut run = rotation_run();
    assert_eq!(rotation_pass(&store, &mut run, 2), Ok(()));
    run.finished_at = Some(NOW);
    assert!(matches!(
        rotation_pass(&store, &mut run, 2),
        Err(JobError::InvalidParams(_))
    ));
    assert_eq!(store.claims.get(), 1);
    assert!(validate_new_run("test", 0, i32::MAX as u32).is_ok());
    assert!(validate_new_run("test", 1, i32::MAX as u32 + 1).is_err());
}

#[test]
fn rotation_refuses_malformed_claim_pages_before_rewrapping() {
    for (batch, limit) in [
        (vec![claim("user-a", 1), claim("user-b", 1)], 1),
        (vec![claim("user-a", 1), claim("user-a", 1)], 2),
        (vec![claim("user-a", 1), claim("user-b", 2)], 2),
        (vec![claim("user-a", 1), claim("", 1)], 2),
        (vec![claim("user-prev", 1)], 2),
    ] {
        let store = RotationStore {
            batch: RefCell::new(batch),
            ..RotationStore::default()
        };
        let mut run = rotation_run();
        run.cursor = Some("user-prev".into());
        let before = run.clone();
        assert_eq!(rotation_pass(&store, &mut run, limit), Err(JobError::Store));
        assert_eq!(store.writes.get(), 0);
        assert_eq!(run, before);
    }
}

#[test]
fn rotation_rejects_a_reload_for_another_user() {
    let store = RotationStore {
        batch: RefCell::new(vec![claim("user-a", 1)]),
        reload: RefCell::new(Some(claim("user-b", 1))),
        ..RotationStore::default()
    };
    let mut run = rotation_run();
    let before = run.clone();
    assert_eq!(rotation_pass(&store, &mut run, 2), Err(JobError::Store));
    assert_eq!(store.writes.get(), 1, "only the original CAS was attempted");
    assert_eq!(run, before);
}
