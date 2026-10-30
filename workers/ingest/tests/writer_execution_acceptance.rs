//! Behavioral acceptance below the production native writer sequence.
//!
//! Only `WriterDb` is scripted: SQL, typed ordered binds, affected rows and
//! selected columns cross the real executor boundary. Expectations below are
//! hand-derived from the declared writer contracts, not executor source text.
//! In particular, a successful no-op executor cannot satisfy a script.
//!
//! This bank does NOT prove artifact/R2/blob ordering, Queue ack/DLQ policy,
//! durable checkpoint/provider/file-map lifecycle, or live deletion-lock races.
//! The edge adapters must consume this same sequence before runtime acceptance.
//! Text UUID values here also do not prove PostgreSQL driver compatibility:
//! track-ID projections and media/grant UUID casts need coordinated SQL-fixture
//! verification when the actual transport is wired.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Debug;

use nntune_ingest::retry::JobError;
use nntune_ingest::track_writers::TrackRow;
use nntune_ingest::writer_execution::{
    admit_job, materialize_tracks, publish_media, settle_job, DbFailure, MediaPublication, Value,
    WriterDb,
};

const USER: &str = "writer_owner";
const OTHER_USER: &str = "other_writer_owner";
const JOB: &str = "job_writer_0123456789";
const OTHER_JOB: &str = "job_other_9876543210";
const KEY: &str =
    "ingest:writer_owner:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:upload";
const OTHER_KEY: &str =
    "ingest:writer_owner:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:upload";
const TIME: i64 = 1_730_000_123;
const RELEASE: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const RECORDING: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const TRACK: &str = "cccccccc-3333-4333-8333-333333333333";
const OTHER_TRACK: &str = "dddddddd-4444-4444-8444-444444444444";
const PROVIDER: &str = "eeeeeeee-5555-4555-8555-555555555555";
const OTHER_PROVIDER: &str = "ffffffff-6666-4666-8666-666666666666";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

// These are emitted-statement expectations at the transport boundary, not
// source-shape pins. Keep UUID cast/projection changes coordinated with the
// PostgreSQL fixtures; do not mistake these strings for driver acceptance.
const ADMIT_WRITE: &str = "INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status) SELECT $1, $2, $3, $4, 'queued' FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (id) DO NOTHING";
const ADMIT_SELECT: &str = "SELECT ingest_jobs.user_id, ingest_jobs.idempotency_key FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id WHERE ingest_jobs.id = $1 AND users.deleting_at IS NULL FOR SHARE OF users";
const TERMINAL_WRITE: &str = "UPDATE ingest_jobs SET status = $3 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE ingest_jobs.id = $1 AND ingest_jobs.user_id = $2 AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running')";
const TERMINAL_SELECT: &str = "SELECT status FROM ingest_jobs WHERE id = $1 AND user_id = $2";
const TRACK_WRITE: &str = "INSERT INTO tracks (mbid, release_id, title, medium_no, track_no) SELECT $1::text::uuid, $2::text::uuid, $3, $4, $5 FROM (SELECT id FROM users WHERE id = $6 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (release_id, medium_no, track_no) DO NOTHING";
const TRACK_SELECT: &str =
    "SELECT id FROM tracks WHERE release_id = $1::text::uuid AND medium_no = $2 AND track_no = $3";
const MEDIA_WRITE: &str = "INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at) SELECT $1, $2, $3, $4, 'opus', 64, $5 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, track_id, source_job_id, content_hash) DO NOTHING";
const MEDIA_SELECT: &str = "SELECT user_id, track_id, source_job_id, content_hash, format, bitrate FROM track_media WHERE user_id = $1 AND track_id = $2 AND source_job_id = $3 AND content_hash = $4";
const GRANT_WRITE: &str = "INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at) SELECT $1, $2, $3, $4 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, content_hash) DO NOTHING";
const GRANT_SELECT: &str = "SELECT user_id, content_hash, provider_id FROM user_cache_grants WHERE user_id = $1 AND content_hash = $2";

#[derive(Debug)]
enum Step {
    Execute {
        sql: &'static str,
        params: Vec<Value>,
        reply: Result<u64, DbFailure>,
    },
    Query {
        sql: &'static str,
        params: Vec<Value>,
        reply: Result<Vec<Vec<Value>>, DbFailure>,
    },
}

#[derive(Debug, Default)]
struct ScriptDb {
    pending: RefCell<VecDeque<Step>>,
}

impl ScriptDb {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            pending: RefCell::new(steps.into()),
        }
    }

    #[track_caller]
    fn next(&self) -> Step {
        self.pending
            .borrow_mut()
            .pop_front()
            .expect("unexpected DB call: no write, probe, retry or downstream write was authorized by the script")
    }

    #[track_caller]
    fn assert_complete(&self) {
        assert!(
            self.pending.borrow().is_empty(),
            "the executor skipped required transport steps: {:?}",
            self.pending.borrow()
        );
    }
}

impl WriterDb for ScriptDb {
    async fn execute(&self, sql: &'static str, params: &[Value]) -> Result<u64, DbFailure> {
        match self.next() {
            Step::Execute {
                sql: expected_sql,
                params: expected_params,
                reply,
            } => {
                assert_eq!(sql, expected_sql, "wrong write or sequence");
                assert_eq!(params, expected_params, "wrong typed write binds");
                reply
            }
            step => panic!("expected {step:?}, got execute({sql}, {params:?})"),
        }
    }

    async fn query(
        &self,
        sql: &'static str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DbFailure> {
        match self.next() {
            Step::Query {
                sql: expected_sql,
                params: expected_params,
                reply,
            } => {
                assert_eq!(sql, expected_sql, "wrong PK probe or sequence");
                assert_eq!(params, expected_params, "wrong typed PK probe binds");
                reply
            }
            step => panic!("expected {step:?}, got query({sql}, {params:?})"),
        }
    }
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn execute(sql: &'static str, params: Vec<Value>, reply: Result<u64, DbFailure>) -> Step {
    Step::Execute { sql, params, reply }
}

fn query(sql: &'static str, params: Vec<Value>, reply: Result<Vec<Vec<Value>>, DbFailure>) -> Step {
    Step::Query { sql, params, reply }
}

#[track_caller]
fn error<T: Debug>(result: Result<T, JobError>, transient: bool) -> JobError {
    let error = result.expect_err("failed or unverified persistence must never be accepted");
    assert_eq!(error.transient, transient, "wrong retry policy: {error:?}");
    assert!(!error.made_progress, "a DB failure is not a continuation");
    assert_eq!(error.retry_after_secs, None);
    error
}

#[track_caller]
fn account_deleting<T: Debug>(result: Result<T, JobError>) {
    assert_eq!(error(result, false).code, "account_deleting");
}

fn admission_write(reply: Result<u64, DbFailure>) -> Step {
    execute(
        ADMIT_WRITE,
        vec![text(JOB), text(USER), text(KEY), Value::I64(TIME)],
        reply,
    )
}

fn admission_select(reply: Result<Vec<Vec<Value>>, DbFailure>) -> Step {
    query(ADMIT_SELECT, vec![text(JOB)], reply)
}

fn terminal_write(status: &str, reply: Result<u64, DbFailure>) -> Step {
    execute(
        TERMINAL_WRITE,
        vec![text(JOB), text(USER), text(status)],
        reply,
    )
}

fn terminal_select(reply: Result<Vec<Vec<Value>>, DbFailure>) -> Step {
    query(TERMINAL_SELECT, vec![text(JOB), text(USER)], reply)
}

fn track_row() -> TrackRow<'static> {
    TrackRow {
        release_id: RELEASE,
        title: "First matched recording",
        medium_no: 1,
        track_no: 3,
        mbid: RECORDING,
        file_ordinal: 2,
    }
}

fn track_write(row: &TrackRow<'_>, reply: Result<u64, DbFailure>) -> Step {
    execute(
        TRACK_WRITE,
        vec![
            text(row.mbid),
            text(row.release_id),
            text(row.title),
            Value::I32(row.medium_no),
            Value::I32(row.track_no),
            text(USER),
        ],
        reply,
    )
}

fn track_select(row: &TrackRow<'_>, reply: Result<Vec<Vec<Value>>, DbFailure>) -> Step {
    query(
        TRACK_SELECT,
        vec![
            text(row.release_id),
            Value::I32(row.medium_no),
            Value::I32(row.track_no),
        ],
        reply,
    )
}

fn publication() -> MediaPublication {
    MediaPublication {
        user_id: USER.to_owned(),
        track_id: TRACK.to_owned(),
        source_job_id: JOB.to_owned(),
        content_hash: HASH.to_owned(),
        provider_id: PROVIDER.to_owned(),
        verified_at: TIME,
    }
}

fn media_write(publication: &MediaPublication, reply: Result<u64, DbFailure>) -> Step {
    execute(
        MEDIA_WRITE,
        vec![
            text(&publication.user_id),
            text(&publication.track_id),
            text(&publication.source_job_id),
            text(&publication.content_hash),
            Value::I64(publication.verified_at),
        ],
        reply,
    )
}

fn media_select(publication: &MediaPublication, reply: Result<Vec<Vec<Value>>, DbFailure>) -> Step {
    query(
        MEDIA_SELECT,
        vec![
            text(&publication.user_id),
            text(&publication.track_id),
            text(&publication.source_job_id),
            text(&publication.content_hash),
        ],
        reply,
    )
}

fn grant_write(publication: &MediaPublication, reply: Result<u64, DbFailure>) -> Step {
    execute(
        GRANT_WRITE,
        vec![
            text(&publication.user_id),
            text(&publication.content_hash),
            text(&publication.provider_id),
            Value::I64(publication.verified_at),
        ],
        reply,
    )
}

fn grant_select(publication: &MediaPublication, reply: Result<Vec<Vec<Value>>, DbFailure>) -> Step {
    query(
        GRANT_SELECT,
        vec![text(&publication.user_id), text(&publication.content_hash)],
        reply,
    )
}

fn stored_media() -> Vec<Value> {
    vec![
        text(USER),
        text(TRACK),
        text(JOB),
        text(HASH),
        text("opus"),
        Value::I32(64),
    ]
}

fn stored_grant(provider: &str) -> Vec<Value> {
    vec![text(USER), text(HASH), text(provider)]
}

#[tokio::test]
async fn admission_persists_fenced_job_owner_key_time_without_a_provider_bind() {
    let db = ScriptDb::new(vec![admission_write(Ok(1))]);
    assert_eq!(admit_job(&db, JOB, USER, KEY, TIME).await, Ok(()));
    db.assert_complete();
}

#[tokio::test]
async fn admission_zero_rows_reselects_job_pk_and_accepts_only_exact_replay_identity() {
    let db = ScriptDb::new(vec![
        admission_write(Ok(0)),
        admission_select(Ok(vec![vec![text(USER), text(KEY)]])),
    ]);
    assert_eq!(admit_job(&db, JOB, USER, KEY, TIME).await, Ok(()));
    db.assert_complete();
}

#[tokio::test]
async fn admission_absent_after_zero_rows_is_permanently_fenced() {
    let db = ScriptDb::new(vec![admission_write(Ok(0)), admission_select(Ok(vec![]))]);
    account_deleting(admit_job(&db, JOB, USER, KEY, TIME).await);
    db.assert_complete();
}

#[tokio::test]
async fn admission_never_adopts_a_job_for_another_owner_or_key() {
    for identity in [
        vec![text(OTHER_USER), text(KEY)],
        vec![text(USER), text(OTHER_KEY)],
        vec![text(OTHER_USER), text(OTHER_KEY)],
    ] {
        let db = ScriptDb::new(vec![
            admission_write(Ok(0)),
            admission_select(Ok(vec![identity])),
        ]);
        error(admit_job(&db, JOB, USER, KEY, TIME).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn admission_malformed_or_ambiguous_identity_rows_fail_closed() {
    for rows in [
        vec![vec![]],
        vec![vec![text(USER)]],
        vec![vec![text(USER), Value::I64(1)]],
        vec![vec![text(USER), text(KEY), text("extra")]],
        vec![vec![text(USER), text(KEY)], vec![text(USER), text(KEY)]],
    ] {
        let db = ScriptDb::new(vec![admission_write(Ok(0)), admission_select(Ok(rows))]);
        error(admit_job(&db, JOB, USER, KEY, TIME).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn admission_validates_inputs_before_any_database_operation() {
    let long_job = "j".repeat(65);
    let long_user = "u".repeat(129);
    let long_key = format!("ingest:writer_owner:{}", "a".repeat(768));
    for (job, user, key, time) in [
        ("", USER, KEY, TIME),
        ("bad job!", USER, KEY, TIME),
        (long_job.as_str(), USER, KEY, TIME),
        (JOB, "", KEY, TIME),
        (JOB, long_user.as_str(), KEY, TIME),
        (JOB, USER, "", TIME),
        (JOB, USER, "not-an-ingest-key", TIME),
        (JOB, USER, "ingest:missing-second-delimiter", TIME),
        (JOB, USER, long_key.as_str(), TIME),
        (JOB, USER, KEY, -1),
        (JOB, USER, KEY, 253_402_300_800),
    ] {
        let db = ScriptDb::default();
        error(admit_job(&db, job, user, key, time).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn admission_timestamp_bounds_are_inclusive_and_bind_as_bigint() {
    for time in [0, 253_402_300_799] {
        let db = ScriptDb::new(vec![execute(
            ADMIT_WRITE,
            vec![text(JOB), text(USER), text(KEY), Value::I64(time)],
            Ok(1),
        )]);
        assert_eq!(admit_job(&db, JOB, USER, KEY, time).await, Ok(()));
        db.assert_complete();
    }
}

#[tokio::test]
async fn terminal_writes_both_terminal_statuses_with_the_owner_fence() {
    for status in ["done", "failed"] {
        let db = ScriptDb::new(vec![terminal_write(status, Ok(1))]);
        assert_eq!(settle_job(&db, JOB, USER, status).await, Ok(()));
        db.assert_complete();
    }
}

#[tokio::test]
async fn terminal_zero_rows_converges_on_either_stored_terminal_status() {
    // An opposite terminal value is also Converged, not a new conflict policy.
    for requested in ["done", "failed"] {
        for stored in ["done", "failed"] {
            let db = ScriptDb::new(vec![
                terminal_write(requested, Ok(0)),
                terminal_select(Ok(vec![vec![text(stored)]])),
            ]);
            assert_eq!(settle_job(&db, JOB, USER, requested).await, Ok(()));
            db.assert_complete();
        }
    }
}

#[tokio::test]
async fn terminal_missing_or_nonterminal_rows_are_permanent_not_converged() {
    for rows in [
        vec![],
        vec![vec![text("queued")]],
        vec![vec![text("running")]],
        vec![vec![text("unknown")]],
        vec![vec![text("")]],
    ] {
        let db = ScriptDb::new(vec![
            terminal_write("done", Ok(0)),
            terminal_select(Ok(rows)),
        ]);
        error(settle_job(&db, JOB, USER, "done").await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn terminal_malformed_or_ambiguous_status_rows_cannot_claim_convergence() {
    for rows in [
        vec![vec![]],
        vec![vec![Value::I32(1)]],
        vec![vec![text("done"), text("extra")]],
        vec![vec![text("done")], vec![text("failed")]],
    ] {
        let db = ScriptDb::new(vec![
            terminal_write("failed", Ok(0)),
            terminal_select(Ok(rows)),
        ]);
        error(settle_job(&db, JOB, USER, "failed").await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn terminal_rejects_invalid_identity_or_nonterminal_target_before_transport() {
    let long_job = "j".repeat(65);
    let long_user = "u".repeat(129);
    for (job, user, status) in [
        ("", USER, "done"),
        ("bad job!", USER, "failed"),
        (long_job.as_str(), USER, "done"),
        (JOB, "", "done"),
        (JOB, long_user.as_str(), "failed"),
        (JOB, USER, "queued"),
        (JOB, USER, "running"),
        (JOB, USER, "DONE"),
        (JOB, USER, ""),
        (JOB, USER, "done\0"),
    ] {
        let db = ScriptDb::default();
        error(settle_job(&db, job, user, status).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn tracks_always_select_database_ids_and_keep_sparse_original_file_ordinals() {
    let first = track_row();
    // Same recording at a different album position: MBID is not the row ID,
    // and neither album position nor vector position is the file ordinal.
    let second = TrackRow {
        title: "Second matched occurrence",
        medium_no: 2,
        track_no: 9,
        file_ordinal: 7,
        ..first.clone()
    };
    let db = ScriptDb::new(vec![
        track_write(&first, Ok(1)),
        track_select(&first, Ok(vec![vec![text(TRACK)]])),
        track_write(&second, Ok(1)),
        track_select(&second, Ok(vec![vec![text(OTHER_TRACK)]])),
    ]);
    assert_eq!(
        materialize_tracks(&db, USER, &[first, second]).await,
        Ok(BTreeMap::from([
            (2, TRACK.to_owned()),
            (7, OTHER_TRACK.to_owned()),
        ]))
    );
    db.assert_complete();
}

#[tokio::test]
async fn tracks_zero_insert_reselects_the_existing_natural_key_id() {
    let row = track_row();
    let db = ScriptDb::new(vec![
        track_write(&row, Ok(0)),
        track_select(&row, Ok(vec![vec![text(TRACK)]])),
    ]);
    assert_eq!(
        materialize_tracks(&db, USER, &[row]).await,
        Ok(BTreeMap::from([(2, TRACK.to_owned())]))
    );
    db.assert_complete();
}

#[tokio::test]
async fn tracks_zero_insert_without_a_row_is_account_deleting() {
    let row = track_row();
    let db = ScriptDb::new(vec![
        track_write(&row, Ok(0)),
        track_select(&row, Ok(vec![])),
    ]);
    account_deleting(materialize_tracks(&db, USER, &[row]).await);
    db.assert_complete();
}

#[tokio::test]
async fn tracks_successful_insert_without_a_selected_id_never_fabricates_an_id() {
    let row = track_row();
    let db = ScriptDb::new(vec![
        track_write(&row, Ok(1)),
        track_select(&row, Ok(vec![])),
    ]);
    error(materialize_tracks(&db, USER, &[row]).await, false);
    db.assert_complete();
}

#[tokio::test]
async fn tracks_reject_malformed_or_ambiguous_database_ids_for_inserts_and_replays() {
    let row = track_row();
    for affected in [0, 1] {
        for rows in [
            vec![vec![]],
            vec![vec![text("")]],
            vec![vec![text("not-a-uuid")]],
            vec![vec![Value::I64(7)]],
            vec![vec![text(TRACK), text("extra")]],
            vec![vec![text(TRACK)], vec![text(OTHER_TRACK)]],
            vec![vec![text(TRACK)], vec![text(TRACK)]],
        ] {
            let db = ScriptDb::new(vec![
                track_write(&row, Ok(affected)),
                track_select(&row, Ok(rows)),
            ]);
            error(
                materialize_tracks(&db, USER, std::slice::from_ref(&row)).await,
                false,
            );
            db.assert_complete();
        }
    }
}

#[tokio::test]
async fn tracks_empty_derived_rows_returns_an_empty_map_without_database_calls() {
    let db = ScriptDb::default();
    assert_eq!(
        materialize_tracks(&db, USER, &[]).await,
        Ok(BTreeMap::new())
    );
    db.assert_complete();
}

#[tokio::test]
async fn tracks_validate_each_row_before_transport() {
    let good = track_row();
    for bad in [
        TrackRow {
            release_id: "bad",
            ..good.clone()
        },
        TrackRow {
            mbid: "bad",
            ..good.clone()
        },
        TrackRow {
            medium_no: 0,
            ..good.clone()
        },
        TrackRow {
            track_no: -1,
            ..good.clone()
        },
        TrackRow {
            title: "",
            ..good.clone()
        },
        TrackRow {
            title: "  ",
            ..good.clone()
        },
        TrackRow {
            title: "bad\0title",
            ..good.clone()
        },
    ] {
        let db = ScriptDb::default();
        error(materialize_tracks(&db, USER, &[bad]).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn tracks_prevalidate_the_whole_batch_before_writing_its_valid_prefix() {
    let good = track_row();
    let bad = TrackRow {
        mbid: "not-a-uuid",
        file_ordinal: 7,
        ..good.clone()
    };
    let db = ScriptDb::default();
    error(materialize_tracks(&db, USER, &[good, bad]).await, false);
    db.assert_complete();
}

#[tokio::test]
async fn tracks_validate_the_fence_owner_before_transport() {
    let long_user = "u".repeat(129);
    for user in ["", "owner:confused", "owner\0bad", long_user.as_str()] {
        let db = ScriptDb::default();
        error(materialize_tracks(&db, user, &[track_row()]).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_writes_literal_opus64_media_then_the_own_provider_grant() {
    let publication = publication();
    let db = ScriptDb::new(vec![
        media_write(&publication, Ok(1)),
        grant_write(&publication, Ok(1)),
    ]);
    assert_eq!(publish_media(&db, &publication).await, Ok(()));
    db.assert_complete();
}

#[tokio::test]
async fn publication_media_zero_rows_probes_its_full_pk_before_writing_the_grant() {
    let publication = publication();
    let db = ScriptDb::new(vec![
        media_write(&publication, Ok(0)),
        media_select(&publication, Ok(vec![stored_media()])),
        grant_write(&publication, Ok(1)),
    ]);
    assert_eq!(publish_media(&db, &publication).await, Ok(()));
    db.assert_complete();
}

#[tokio::test]
async fn publication_grant_zero_rows_probes_its_user_hash_pk() {
    let publication = publication();
    let db = ScriptDb::new(vec![
        media_write(&publication, Ok(1)),
        grant_write(&publication, Ok(0)),
        grant_select(&publication, Ok(vec![stored_grant(PROVIDER)])),
    ]);
    assert_eq!(publish_media(&db, &publication).await, Ok(()));
    db.assert_complete();
}

#[tokio::test]
async fn publication_both_duplicates_converge_even_when_first_provider_differs() {
    for provider in [PROVIDER, OTHER_PROVIDER] {
        let publication = publication();
        let db = ScriptDb::new(vec![
            media_write(&publication, Ok(0)),
            media_select(&publication, Ok(vec![stored_media()])),
            grant_write(&publication, Ok(0)),
            grant_select(&publication, Ok(vec![stored_grant(provider)])),
        ]);
        assert_eq!(publish_media(&db, &publication).await, Ok(()));
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_missing_media_after_zero_rows_refuses_without_a_grant_write() {
    let publication = publication();
    let db = ScriptDb::new(vec![
        media_write(&publication, Ok(0)),
        media_select(&publication, Ok(vec![])),
    ]);
    account_deleting(publish_media(&db, &publication).await);
    db.assert_complete();
}

#[tokio::test]
async fn publication_missing_grant_after_zero_rows_is_account_deleting() {
    let publication = publication();
    let db = ScriptDb::new(vec![
        media_write(&publication, Ok(1)),
        grant_write(&publication, Ok(0)),
        grant_select(&publication, Ok(vec![])),
    ]);
    account_deleting(publish_media(&db, &publication).await);
    db.assert_complete();
}

#[tokio::test]
async fn publication_media_probe_cannot_adopt_another_identity_or_profile() {
    for (column, wrong) in [
        (0, text(OTHER_USER)),
        (1, text(OTHER_TRACK)),
        (2, text(OTHER_JOB)),
        (3, text(OTHER_HASH)),
        (4, text("mp3")),
        (5, Value::I32(96)),
        (5, Value::I64(64)),
    ] {
        let publication = publication();
        let mut stored = stored_media();
        stored[column] = wrong;
        let db = ScriptDb::new(vec![
            media_write(&publication, Ok(0)),
            media_select(&publication, Ok(vec![stored])),
        ]);
        error(publish_media(&db, &publication).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_media_probe_rejects_malformed_or_ambiguous_rows() {
    for rows in [
        vec![vec![]],
        vec![vec![text(USER), text(TRACK), text(JOB), text(HASH)]],
        vec![vec![
            text(USER),
            text(TRACK),
            text(JOB),
            text(HASH),
            text("opus"),
            Value::I32(64),
            text("extra"),
        ]],
        vec![stored_media(), stored_media()],
    ] {
        let publication = publication();
        let db = ScriptDb::new(vec![
            media_write(&publication, Ok(0)),
            media_select(&publication, Ok(rows)),
        ]);
        error(publish_media(&db, &publication).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_grant_probe_cannot_adopt_another_owner_or_hash() {
    for stored in [
        vec![text(OTHER_USER), text(HASH), text(OTHER_PROVIDER)],
        vec![text(USER), text(OTHER_HASH), text(PROVIDER)],
    ] {
        let publication = publication();
        let db = ScriptDb::new(vec![
            media_write(&publication, Ok(1)),
            grant_write(&publication, Ok(0)),
            grant_select(&publication, Ok(vec![stored])),
        ]);
        error(publish_media(&db, &publication).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_grant_probe_rejects_malformed_or_ambiguous_rows() {
    for rows in [
        vec![vec![]],
        vec![vec![text(USER), text(HASH)]],
        vec![vec![text(USER), text(HASH), Value::I64(1)]],
        vec![vec![text(USER), text(HASH), text(PROVIDER), text("extra")]],
        vec![stored_grant(PROVIDER), stored_grant(OTHER_PROVIDER)],
    ] {
        let publication = publication();
        let db = ScriptDb::new(vec![
            media_write(&publication, Ok(1)),
            grant_write(&publication, Ok(0)),
            grant_select(&publication, Ok(rows)),
        ]);
        error(publish_media(&db, &publication).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_validates_all_inputs_before_even_the_media_write() {
    let good = publication();
    for bad in [
        MediaPublication {
            user_id: String::new(),
            ..good.clone()
        },
        MediaPublication {
            user_id: "u".repeat(129),
            ..good.clone()
        },
        MediaPublication {
            user_id: "owner:confused".to_owned(),
            ..good.clone()
        },
        MediaPublication {
            user_id: "owner\0bad".to_owned(),
            ..good.clone()
        },
        MediaPublication {
            track_id: "not-a-uuid".to_owned(),
            ..good.clone()
        },
        MediaPublication {
            source_job_id: String::new(),
            ..good.clone()
        },
        MediaPublication {
            source_job_id: "bad job!".to_owned(),
            ..good.clone()
        },
        MediaPublication {
            source_job_id: "j".repeat(65),
            ..good.clone()
        },
        MediaPublication {
            content_hash: String::new(),
            ..good.clone()
        },
        MediaPublication {
            content_hash: "a".repeat(63),
            ..good.clone()
        },
        MediaPublication {
            content_hash: "a".repeat(65),
            ..good.clone()
        },
        MediaPublication {
            content_hash: "z".repeat(64),
            ..good.clone()
        },
        MediaPublication {
            provider_id: "not-a-uuid".to_owned(),
            ..good.clone()
        },
        MediaPublication {
            verified_at: -1,
            ..good.clone()
        },
        MediaPublication {
            verified_at: 253_402_300_800,
            ..good.clone()
        },
    ] {
        let db = ScriptDb::default();
        error(publish_media(&db, &bad).await, false);
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_timestamp_bounds_bind_as_bigint_to_both_writers() {
    for verified_at in [0, 253_402_300_799] {
        let publication = MediaPublication {
            verified_at,
            ..publication()
        };
        let db = ScriptDb::new(vec![
            media_write(&publication, Ok(1)),
            grant_write(&publication, Ok(1)),
        ]);
        assert_eq!(publish_media(&db, &publication).await, Ok(()));
        db.assert_complete();
    }
}

#[tokio::test]
async fn publication_media_success_grant_failure_retries_both_then_fully_converges() {
    let publication = publication();
    let db = ScriptDb::new(vec![
        media_write(&publication, Ok(1)),
        grant_write(
            &publication,
            Err(DbFailure {
                sqlstate: Some("08006".to_owned()),
            }),
        ),
        media_write(&publication, Ok(0)),
        media_select(&publication, Ok(vec![stored_media()])),
        grant_write(&publication, Ok(1)),
        media_write(&publication, Ok(0)),
        media_select(&publication, Ok(vec![stored_media()])),
        grant_write(&publication, Ok(0)),
        grant_select(&publication, Ok(vec![stored_grant(PROVIDER)])),
    ]);
    assert_eq!(
        error(publish_media(&db, &publication).await, true).code,
        "write_failed"
    );
    assert_eq!(publish_media(&db, &publication).await, Ok(()));
    assert_eq!(publish_media(&db, &publication).await, Ok(()));
    db.assert_complete();
}

#[tokio::test]
async fn publication_existing_same_hash_grant_never_skips_a_missing_media_row() {
    let publication = MediaPublication {
        track_id: OTHER_TRACK.to_owned(),
        ..publication()
    };
    let db = ScriptDb::new(vec![
        media_write(&publication, Ok(1)),
        grant_write(&publication, Ok(0)),
        grant_select(&publication, Ok(vec![stored_grant(OTHER_PROVIDER)])),
    ]);
    assert_eq!(publish_media(&db, &publication).await, Ok(()));
    db.assert_complete();
}

#[tokio::test]
async fn publication_a_prior_users_same_hash_rows_never_skip_another_users_writes() {
    let first = publication();
    let second = MediaPublication {
        user_id: OTHER_USER.to_owned(),
        source_job_id: OTHER_JOB.to_owned(),
        provider_id: OTHER_PROVIDER.to_owned(),
        ..first.clone()
    };
    let db = ScriptDb::new(vec![
        media_write(&first, Ok(0)),
        media_select(&first, Ok(vec![stored_media()])),
        grant_write(&first, Ok(0)),
        grant_select(&first, Ok(vec![stored_grant(PROVIDER)])),
        media_write(&second, Ok(1)),
        grant_write(&second, Ok(1)),
    ]);
    assert_eq!(publish_media(&db, &first).await, Ok(()));
    assert_eq!(publish_media(&db, &second).await, Ok(()));
    db.assert_complete();
}

#[derive(Debug, Clone, Copy)]
enum FaultSite {
    AdmissionWrite,
    AdmissionSelect,
    TerminalWrite,
    TerminalSelect,
    TrackWrite,
    TrackSelect,
    TrackReplaySelect,
    MediaWrite,
    MediaSelect,
    GrantWrite,
    GrantSelect,
}

impl FaultSite {
    fn script(self, failure: DbFailure) -> Vec<Step> {
        let row = track_row();
        let publication = publication();
        match self {
            Self::AdmissionWrite => vec![admission_write(Err(failure))],
            Self::AdmissionSelect => vec![admission_write(Ok(0)), admission_select(Err(failure))],
            Self::TerminalWrite => vec![terminal_write("done", Err(failure))],
            Self::TerminalSelect => {
                vec![terminal_write("done", Ok(0)), terminal_select(Err(failure))]
            }
            Self::TrackWrite => vec![track_write(&row, Err(failure))],
            Self::TrackSelect => vec![track_write(&row, Ok(1)), track_select(&row, Err(failure))],
            Self::TrackReplaySelect => {
                vec![track_write(&row, Ok(0)), track_select(&row, Err(failure))]
            }
            Self::MediaWrite => vec![media_write(&publication, Err(failure))],
            Self::MediaSelect => vec![
                media_write(&publication, Ok(0)),
                media_select(&publication, Err(failure)),
            ],
            Self::GrantWrite => vec![
                media_write(&publication, Ok(1)),
                grant_write(&publication, Err(failure)),
            ],
            Self::GrantSelect => vec![
                media_write(&publication, Ok(1)),
                grant_write(&publication, Ok(0)),
                grant_select(&publication, Err(failure)),
            ],
        }
    }

    async fn run(self, db: &ScriptDb) -> Result<(), JobError> {
        match self {
            Self::AdmissionWrite | Self::AdmissionSelect => {
                admit_job(db, JOB, USER, KEY, TIME).await
            }
            Self::TerminalWrite | Self::TerminalSelect => settle_job(db, JOB, USER, "done").await,
            Self::TrackWrite | Self::TrackSelect | Self::TrackReplaySelect => {
                materialize_tracks(db, USER, &[track_row()])
                    .await
                    .map(|_| ())
            }
            Self::MediaWrite | Self::MediaSelect | Self::GrantWrite | Self::GrantSelect => {
                publish_media(db, &publication()).await
            }
        }
    }
}

async fn assert_database_failure_policy(sites: &[FaultSite]) {
    // Literal expected policy, deliberately NOT computed with the production
    // classifier: dropping any of 42/22/23/3F or catching 08/40/53/57 is a bug.
    for site in sites {
        for (sqlstate, transient) in [
            (Some("42P01"), false),
            (Some("42703"), false),
            (Some("42P10"), false),
            (Some("22003"), false),
            (Some("22P02"), false),
            (Some("23503"), false),
            (Some("23505"), false),
            (Some("23514"), false),
            (Some("3F000"), false),
            (None, true),
            (Some("08001"), true),
            (Some("08006"), true),
            (Some("40001"), true),
            (Some("40P01"), true),
            (Some("53300"), true),
            (Some("57P03"), true),
            (Some("57014"), true),
        ] {
            let db = ScriptDb::new(site.script(DbFailure {
                sqlstate: sqlstate.map(str::to_owned),
            }));
            let result = site.run(&db).await;
            let failure = result.expect_err("execute/query failures must reach the caller");
            assert_eq!(failure.code, "write_failed", "{site:?}, {sqlstate:?}");
            assert_eq!(
                failure.transient, transient,
                "{site:?}, {sqlstate:?}: {failure:?}"
            );
            assert!(!failure.made_progress, "{site:?}: no durable continuation");
            assert_eq!(failure.retry_after_secs, None, "{site:?}");
            // Even transient cases must have reached the actual scripted
            // failure; an unconditional 'unavailable' return is not evidence.
            db.assert_complete();
        }
    }
}

#[tokio::test]
async fn admission_write_and_identity_query_errors_use_the_real_sqlstate_policy() {
    assert_database_failure_policy(&[FaultSite::AdmissionWrite, FaultSite::AdmissionSelect]).await;
}

#[tokio::test]
async fn terminal_write_and_status_query_errors_use_the_real_sqlstate_policy() {
    assert_database_failure_policy(&[FaultSite::TerminalWrite, FaultSite::TerminalSelect]).await;
}

#[tokio::test]
async fn track_write_and_id_query_errors_use_the_real_sqlstate_policy() {
    assert_database_failure_policy(&[
        FaultSite::TrackWrite,
        FaultSite::TrackSelect,
        FaultSite::TrackReplaySelect,
    ])
    .await;
}

#[tokio::test]
async fn media_write_and_pk_query_errors_use_the_real_sqlstate_policy() {
    assert_database_failure_policy(&[FaultSite::MediaWrite, FaultSite::MediaSelect]).await;
}

#[tokio::test]
async fn grant_write_and_pk_query_errors_use_the_real_sqlstate_policy() {
    assert_database_failure_policy(&[FaultSite::GrantWrite, FaultSite::GrantSelect]).await;
}

#[tokio::test]
async fn database_failures_never_echo_unsanitized_driver_boundary_fields() {
    // A malformed transport field must not become a raw driver detail through
    // Debug/to_string formatting. This marker is synthetic, not a credential.
    let raw = "writer_detail_canary: synthetic driver diagnostic, not a SQLSTATE";
    for site in [
        FaultSite::AdmissionWrite,
        FaultSite::AdmissionSelect,
        FaultSite::TerminalWrite,
        FaultSite::TerminalSelect,
        FaultSite::TrackWrite,
        FaultSite::TrackSelect,
        FaultSite::TrackReplaySelect,
        FaultSite::MediaWrite,
        FaultSite::MediaSelect,
        FaultSite::GrantWrite,
        FaultSite::GrantSelect,
    ] {
        let db = ScriptDb::new(site.script(DbFailure {
            sqlstate: Some(raw.to_owned()),
        }));
        let failure = error(site.run(&db).await, true);
        assert_eq!(failure.code, "write_failed", "{site:?}");
        assert!(
            !failure.detail.contains("writer_detail_canary"),
            "{site:?}: {failure:?}"
        );
        db.assert_complete();
    }
}
