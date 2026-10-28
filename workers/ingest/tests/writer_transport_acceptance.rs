//! Native behavioral acceptance for PgWriterDb using a real tokio-postgres
//! client over an in-memory PostgreSQL protocol peer. No WriterDb mock, live
//! database, credentials, environment skip, or source-shape assertions.
//!
//! This proves driver-boundary behavior, NOT PostgreSQL statement execution,
//! server-side cancellation, edge integration, or staging publication.

#[path = "support/pg_wire_peer.rs"]
mod pg_wire_peer;

use std::cell::{Cell as Counter, RefCell};
use std::collections::BTreeMap;
use std::future::{pending, ready, Future, Pending};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use nntune_ingest::ingest_jobs;
use nntune_ingest::retry::{JobError, ERR_ACCOUNT_DELETING, ERR_WRITE_FAILED};
use nntune_ingest::track_writers::{self, TrackRow};
use nntune_ingest::transcode;
use nntune_ingest::writer_execution::{self, DbFailure, MediaPublication, Value, WriterDb};
use nntune_ingest::writer_transport::PgWriterDb;
use pg_wire_peer::{
    bounded, Cell, Column, GateControl, Param, Session, Stage, Step, BOOL, BYTEA, DIAGNOSTIC, INT4,
    INT8, TEXT, UUID,
};

const OWNER: &str = "wire-owner";
const JOB: &str = "wire_job-7";
const KEY: &str = "ingest:wire-owner:archive-7";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
// Distinct, hand-derived canonical UUIDs and binary forms catch text-vs-UUID
// typing as well as swapped identities. Schema UUID OIDs are never text-cast.
const TRACK: &str = "00112233-4455-4677-8899-aabbccddeeff";
const TRACK_BYTES: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];
const OTHER_TRACK: &str = "ffeeddcc-bbaa-4988-8766-554433221100";
const OTHER_TRACK_BYTES: [u8; 16] = [
    0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x49, 0x88, 0x87, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00,
];
const PROVIDER: &str = "01234567-89ab-4cde-8fab-0123456789ab";
const PROVIDER_BYTES: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0x4c, 0xde, 0x8f, 0xab, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
];
const FIRST_PROVIDER: &str = "fedcba98-7654-4321-8fed-cba987654321";
const FIRST_PROVIDER_BYTES: [u8; 16] = [
    0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x43, 0x21, 0x8f, 0xed, 0xcb, 0xa9, 0x87, 0x65, 0x43, 0x21,
];
const RELEASE: &str = "10213243-5465-4768-89ab-cdef01234567";
const RECORDING: &str = "11223344-5566-4788-99aa-bbccddeeff00";
const OTHER_RECORDING: &str = "22334455-6677-4899-aabb-ccddeeff0011";
const TIMESTAMP: i64 = 4_294_967_299;
const LABEL: &str = "wire 'payload' — 音楽";
const INSERT_PROBE: &str =
    "INSERT INTO wire_probe (label, accepted_at, medium_no) VALUES ($1, $2, $3)";
const SELECT_PROBE: &str = "SELECT label, accepted_at, medium_no FROM wire_probe WHERE label = $1";
const SELECT_BAD: &str = "SELECT selected, payload FROM wire_probe";

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn never(calls: Rc<Counter<usize>>) -> impl Fn() -> Pending<()> {
    move || {
        calls.set(calls.get() + 1);
        pending()
    }
}

fn probe_params() -> Vec<Value> {
    vec![text(LABEL), Value::I64(TIMESTAMP), Value::I32(70_001)]
}

fn probe_execute(affected: u64) -> Step {
    Step::execute(
        INSERT_PROBE,
        vec![
            Param::Text(LABEL),
            Param::I64(TIMESTAMP),
            Param::I32(70_001),
        ],
        affected,
    )
}

fn probe_query() -> Step {
    Step::query(
        SELECT_PROBE,
        vec![Param::Text(LABEL)],
        vec![
            Column::new("label", TEXT),
            Column::new("accepted_at", INT8),
            Column::new("medium_no", INT4),
        ],
        vec![vec![
            Cell::Text(LABEL),
            Cell::I64(TIMESTAMP),
            Cell::I32(70_001),
        ]],
    )
}

fn admission(affected: u64) -> Step {
    Step::execute(
        ingest_jobs::upsert_ingest_job_sql(),
        vec![
            Param::Text(JOB),
            Param::Text(OWNER),
            Param::Text(KEY),
            Param::I64(TIMESTAMP),
        ],
        affected,
    )
}

fn admission_identity(rows: Vec<Vec<Cell>>) -> Step {
    Step::query(
        ingest_jobs::select_ingest_job_identity_sql(),
        vec![Param::Text(JOB)],
        vec![
            Column::new("user_id", TEXT),
            Column::new("idempotency_key", TEXT),
        ],
        rows,
    )
}

fn publication() -> MediaPublication {
    MediaPublication {
        user_id: OWNER.to_owned(),
        track_id: TRACK.to_owned(),
        source_job_id: JOB.to_owned(),
        content_hash: HASH.to_owned(),
        provider_id: PROVIDER.to_owned(),
        verified_at: TIMESTAMP,
    }
}

fn media_insert(affected: u64) -> Step {
    Step::execute(
        transcode::upsert_track_media_sql(),
        vec![
            Param::Text(OWNER),
            Param::Uuid(TRACK, TRACK_BYTES),
            Param::Text(JOB),
            Param::Text(HASH),
            Param::I64(TIMESTAMP),
        ],
        affected,
    )
}

fn media_reselect() -> Step {
    Step::query(
        transcode::select_track_media_by_pk_sql(),
        vec![
            Param::Text(OWNER),
            Param::Uuid(TRACK, TRACK_BYTES),
            Param::Text(JOB),
            Param::Text(HASH),
        ],
        vec![
            Column::new("user_id", TEXT),
            Column::new("track_id", UUID),
            Column::new("source_job_id", TEXT),
            Column::new("content_hash", TEXT),
            Column::new("format", TEXT),
            Column::new("bitrate", INT4),
        ],
        vec![vec![
            Cell::Text(OWNER),
            Cell::Uuid(TRACK, TRACK_BYTES),
            Cell::Text(JOB),
            Cell::Text(HASH),
            Cell::Text("opus"),
            Cell::I32(64),
        ]],
    )
}

fn grant_insert(affected: u64) -> Step {
    Step::execute(
        transcode::upsert_cache_grant_sql(),
        vec![
            Param::Text(OWNER),
            Param::Text(HASH),
            Param::Uuid(PROVIDER, PROVIDER_BYTES),
            Param::I64(TIMESTAMP),
        ],
        affected,
    )
}

fn first_grant_reselect() -> Step {
    Step::query(
        transcode::select_cache_grant_by_pk_sql(),
        vec![Param::Text(OWNER), Param::Text(HASH)],
        vec![
            Column::new("user_id", TEXT),
            Column::new("content_hash", TEXT),
            Column::new("provider_id", UUID),
        ],
        vec![vec![
            Cell::Text(OWNER),
            Cell::Text(HASH),
            Cell::Uuid(FIRST_PROVIDER, FIRST_PROVIDER_BYTES),
        ]],
    )
}

fn assert_sanitized(error: &DbFailure, state: Option<&str>) {
    assert_eq!(
        error,
        &DbFailure {
            sqlstate: state.map(str::to_owned)
        }
    );
    let diagnostic = format!("{error:?}");
    for private in [
        DIAGNOSTIC,
        "private_fixture",
        "db.example.invalid",
        "fixture_payload",
    ] {
        assert!(
            !diagnostic.contains(private),
            "driver diagnostic escaped: {diagnostic}"
        );
    }
}

fn assert_classified(error: &JobError, transient: bool) {
    assert_eq!(error.code, ERR_WRITE_FAILED);
    assert_eq!(
        error.transient, transient,
        "SQLSTATE classification changed: {error:?}"
    );
    for private in [
        DIAGNOSTIC,
        "private_fixture",
        "db.example.invalid",
        "fixture_payload",
    ] {
        assert!(
            !format!("{error:?}").contains(private),
            "writer echoed driver data"
        );
    }
}

async fn affected_rows_case(affected: u64) {
    let session = Session::start(vec![probe_execute(affected)]).await;
    let calls = Rc::new(Counter::new(0));
    let db = PgWriterDb::new(&session.client, never(calls.clone()));
    let result = bounded(db.execute(INSERT_PROBE, &probe_params())).await;
    session.finish().await;
    assert_eq!(
        result,
        Ok(affected),
        "must preserve CommandComplete affected rows"
    );
    assert_eq!(calls.get(), 1);
}

#[tokio::test]
async fn execute_binds_text_i64_i32_in_order_and_returns_zero() {
    affected_rows_case(0).await;
}

#[tokio::test]
async fn execute_binds_text_i64_i32_in_order_and_returns_one() {
    affected_rows_case(1).await;
}

#[tokio::test]
async fn query_preserves_row_column_order_integer_widths_and_uuid_identity() {
    const SQL: &str = "SELECT title, accepted_at, track_no, id FROM wire_probe WHERE title = $1";
    let step = Step::query(
        SQL,
        vec![Param::Text(LABEL)],
        vec![
            Column::new("title", TEXT),
            Column::new("accepted_at", INT8),
            Column::new("track_no", INT4),
            Column::new("id", UUID),
        ],
        vec![
            vec![
                Cell::Text(LABEL),
                Cell::I64(TIMESTAMP),
                Cell::I32(70_001),
                Cell::Uuid(TRACK, TRACK_BYTES),
            ],
            vec![
                Cell::Text("second"),
                Cell::I64(i64::MIN + 17),
                Cell::I32(i32::MIN + 11),
                Cell::Uuid(OTHER_TRACK, OTHER_TRACK_BYTES),
            ],
        ],
    );
    let session = Session::start(vec![step]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(db.query(SQL, &[text(LABEL)])).await;
    session.finish().await;
    assert_eq!(
        result,
        Ok(vec![
            vec![
                text(LABEL),
                Value::I64(TIMESTAMP),
                Value::I32(70_001),
                text(TRACK)
            ],
            vec![
                text("second"),
                Value::I64(i64::MIN + 17),
                Value::I32(i32::MIN + 11),
                text(OTHER_TRACK)
            ],
        ])
    );
}

#[tokio::test]
async fn query_zero_rows_is_empty_not_a_fabricated_row_or_failure() {
    let step = admission_identity(vec![]);
    let session = Session::start(vec![step]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result =
        bounded(db.query(ingest_jobs::select_ingest_job_identity_sql(), &[text(JOB)])).await;
    session.finish().await;
    assert_eq!(result, Ok(vec![]));
}

#[tokio::test]
async fn admitted_job_insert_crosses_the_real_driver_before_success() {
    let session = Session::start(vec![admission(1)]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    session.finish().await;
    assert_eq!(result, Ok(()));
}

#[tokio::test]
async fn admitted_job_zero_rows_reselects_the_exact_stored_identity() {
    let session = Session::start(vec![
        admission(0),
        admission_identity(vec![vec![Cell::Text(OWNER), Cell::Text(KEY)]]),
    ])
    .await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    session.finish().await;
    assert_eq!(result, Ok(()));
}

#[tokio::test]
async fn admitted_job_missing_after_zero_rows_is_account_deleting_not_transport_retry() {
    let session = Session::start(vec![admission(0), admission_identity(vec![])]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    session.finish().await;
    let error = result.expect_err("missing replay row must not be admitted");
    assert_eq!(error.code, ERR_ACCOUNT_DELETING);
    assert!(!error.transient);
}

#[tokio::test]
async fn publication_inserts_uuid_media_then_uuid_grant_with_exact_binds() {
    let session = Session::start(vec![media_insert(1), grant_insert(1)]).await;
    let calls = Rc::new(Counter::new(0));
    let db = PgWriterDb::new(&session.client, never(calls.clone()));
    let result = bounded(writer_execution::publish_media(&db, &publication())).await;
    session.finish().await;
    assert_eq!(result, Ok(()));
    assert_eq!(calls.get(), 2, "each adapter method gets its own deadline");
}

#[tokio::test]
async fn publication_zero_rows_decodes_uuid_reselects_and_keeps_first_provider() {
    let session = Session::start(vec![
        media_insert(0),
        media_reselect(),
        grant_insert(0),
        first_grant_reselect(),
    ])
    .await;
    let calls = Rc::new(Counter::new(0));
    let db = PgWriterDb::new(&session.client, never(calls.clone()));
    let result = bounded(writer_execution::publish_media(&db, &publication())).await;
    session.finish().await;
    assert_eq!(
        result,
        Ok(()),
        "a different valid first provider is converged, not rewritten"
    );
    assert_eq!(calls.get(), 4);
}

#[tokio::test]
async fn materialize_decodes_uuid_ids_into_the_original_sparse_ordinal_map() {
    let rows = [
        TrackRow {
            release_id: RELEASE,
            title: "second backed recording",
            medium_no: 3,
            track_no: 2,
            mbid: OTHER_RECORDING,
            file_ordinal: 9,
        },
        TrackRow {
            release_id: RELEASE,
            title: LABEL,
            medium_no: 1,
            track_no: 70_001,
            mbid: RECORDING,
            file_ordinal: 2,
        },
    ];
    let session = Session::start(vec![
        Step::execute(
            track_writers::upsert_track_sql(),
            vec![
                Param::Text(OTHER_RECORDING),
                Param::Text(RELEASE),
                Param::Text("second backed recording"),
                Param::I32(3),
                Param::I32(2),
                Param::Text(OWNER),
            ],
            0,
        ),
        Step::query(
            track_writers::select_track_id_by_position_sql(),
            vec![Param::Text(RELEASE), Param::I32(3), Param::I32(2)],
            vec![Column::new("id", UUID)],
            vec![vec![Cell::Uuid(OTHER_TRACK, OTHER_TRACK_BYTES)]],
        ),
        Step::execute(
            track_writers::upsert_track_sql(),
            vec![
                Param::Text(RECORDING),
                Param::Text(RELEASE),
                Param::Text(LABEL),
                Param::I32(1),
                Param::I32(70_001),
                Param::Text(OWNER),
            ],
            1,
        ),
        Step::query(
            track_writers::select_track_id_by_position_sql(),
            vec![Param::Text(RELEASE), Param::I32(1), Param::I32(70_001)],
            vec![Column::new("id", UUID)],
            vec![vec![Cell::Uuid(TRACK, TRACK_BYTES)]],
        ),
    ])
    .await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::materialize_tracks(&db, OWNER, &rows)).await;
    session.finish().await;
    assert_eq!(
        result,
        Ok(BTreeMap::from([
            (2, TRACK.to_owned()),
            (9, OTHER_TRACK.to_owned())
        ]))
    );
}

#[derive(Clone, Copy, Debug)]
enum Method {
    Execute,
    Query,
}

#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Affected(u64),
    Selected(Vec<Vec<Value>>),
}

impl Method {
    fn step(self) -> Step {
        match self {
            Self::Execute => probe_execute(1),
            Self::Query => probe_query(),
        }
    }

    async fn call(self, db: &impl WriterDb) -> Result<Answer, DbFailure> {
        match self {
            Self::Execute => db
                .execute(INSERT_PROBE, &probe_params())
                .await
                .map(Answer::Affected),
            Self::Query => db
                .query(SELECT_PROBE, &[text(LABEL)])
                .await
                .map(Answer::Selected),
        }
    }

    fn late_step(self) -> Step {
        match self {
            Self::Execute => probe_execute(1),
            Self::Query => Step::query(
                SELECT_PROBE,
                vec![Param::Text(LABEL)],
                vec![
                    Column::new("label", TEXT),
                    Column::new("accepted_at", INT8),
                    Column::new("medium_no", INT4),
                ],
                vec![vec![
                    Cell::Text("late result must not leak"),
                    Cell::I64(17),
                    Cell::I32(9),
                ]],
            ),
        }
    }

    fn fresh_step(self) -> Step {
        match self {
            Self::Execute => probe_execute(0),
            Self::Query => probe_query(),
        }
    }

    fn fresh_answer(self) -> Answer {
        match self {
            Self::Execute => Answer::Affected(0),
            Self::Query => Answer::Selected(vec![vec![
                text(LABEL),
                Value::I64(TIMESTAMP),
                Value::I32(70_001),
            ]]),
        }
    }
}

async fn sqlstate_case(method: Method, stage: Stage, state: &'static str, transient: bool) {
    let mut steps = vec![method.step().error(stage, state)];
    match method {
        Method::Execute => steps.push(admission(1).error(stage, state)),
        Method::Query => {
            steps.push(admission(0));
            steps.push(admission_identity(vec![]).error(stage, state));
        }
    }
    let expected_calls = steps.len();
    let session = Session::start(steps).await;
    let calls = Rc::new(Counter::new(0));
    let db = PgWriterDb::new(&session.client, never(calls.clone()));
    let direct = bounded(method.call(&db)).await;
    let classified = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    session.finish().await;
    assert_sanitized(
        &direct.expect_err("server error must not succeed"),
        Some(state),
    );
    assert_classified(
        &classified.expect_err("writer must preserve driver failure"),
        transient,
    );
    assert_eq!(
        calls.get(),
        expected_calls,
        "factory must also run for failing methods"
    );
}

macro_rules! sqlstate_tests {
    ($execute:ident, $query:ident, $stage:ident, $state:literal, $transient:literal) => {
        #[tokio::test]
        async fn $execute() {
            sqlstate_case(Method::Execute, Stage::$stage, $state, $transient).await;
        }
        #[tokio::test]
        async fn $query() {
            sqlstate_case(Method::Query, Stage::$stage, $state, $transient).await;
        }
    };
}

sqlstate_tests!(
    execute_undefined_table_is_sanitized_and_permanent,
    query_undefined_table_is_sanitized_and_permanent,
    Prepare,
    "42P01",
    false
);
sqlstate_tests!(
    execute_missing_schema_is_sanitized_and_permanent,
    query_missing_schema_is_sanitized_and_permanent,
    Prepare,
    "3F000",
    false
);
sqlstate_tests!(
    execute_unique_violation_is_sanitized_and_permanent,
    query_unique_violation_is_sanitized_and_permanent,
    Execute,
    "23505",
    false
);
sqlstate_tests!(
    execute_connection_sqlstate_is_not_relabelled_deadline,
    query_connection_sqlstate_is_not_relabelled_deadline,
    Execute,
    "08006",
    true
);
sqlstate_tests!(
    execute_serialization_failure_remains_transient,
    query_serialization_failure_remains_transient,
    Execute,
    "40001",
    true
);

async fn disconnected_case(method: Method) {
    let session = Session::start(vec![method.step().disconnect()]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let direct = bounded(method.call(&db)).await;
    session.finish().await;
    assert_sanitized(&direct.expect_err("disconnect must fail closed"), None);

    let steps = match method {
        Method::Execute => vec![admission(1).disconnect()],
        Method::Query => vec![admission(0), admission_identity(vec![]).disconnect()],
    };
    let session = Session::start(steps).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let classified = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    session.finish().await;
    assert_classified(
        &classified.expect_err("disconnect must remain a retryable writer failure"),
        true,
    );
}

#[tokio::test]
async fn execute_without_sqlstate_is_transient_and_sanitized() {
    disconnected_case(Method::Execute).await;
}
#[tokio::test]
async fn query_without_sqlstate_is_transient_and_sanitized() {
    disconnected_case(Method::Query).await;
}

async fn bad_selection(oid: u32, valid: Cell, invalid: Cell) {
    let session = Session::start(vec![Step::query(
        SELECT_BAD,
        vec![],
        vec![Column::new("selected", oid), Column::new("payload", TEXT)],
        vec![
            vec![valid, Cell::Text("private_fixture_value")],
            vec![invalid, Cell::Text("private_fixture_value")],
        ],
    )])
    .await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(db.query(SELECT_BAD, &[])).await;
    session.finish().await;
    assert_sanitized(
        &result.expect_err(
            "bad selected value must reject the whole result, not return a prefix/default",
        ),
        Some("22000"),
    );
}

macro_rules! bad_selection_test {
    ($name:ident, $oid:ident, $valid:expr, $invalid:expr) => {
        #[tokio::test]
        async fn $name() {
            bad_selection($oid, $valid, $invalid).await;
        }
    };
}

bad_selection_test!(
    query_invalid_utf8_is_sanitized_data_exception,
    TEXT,
    Cell::Text("valid"),
    Cell::Raw(vec![0xff, 0xfe])
);
bad_selection_test!(
    query_short_i32_is_sanitized_data_exception,
    INT4,
    Cell::I32(7),
    Cell::Raw(vec![0, 0, 7])
);
bad_selection_test!(
    query_short_i64_is_sanitized_data_exception,
    INT8,
    Cell::I64(7),
    Cell::Raw(vec![0; 7])
);
bad_selection_test!(
    query_short_uuid_is_sanitized_data_exception,
    UUID,
    Cell::Uuid(TRACK, TRACK_BYTES),
    Cell::Raw(vec![0; 15])
);
bad_selection_test!(
    query_null_text_is_not_fabricated_empty_text,
    TEXT,
    Cell::Text("valid"),
    Cell::Null
);
bad_selection_test!(
    query_null_i32_is_not_fabricated_zero,
    INT4,
    Cell::I32(7),
    Cell::Null
);
bad_selection_test!(
    query_null_i64_is_not_fabricated_zero,
    INT8,
    Cell::I64(7),
    Cell::Null
);
bad_selection_test!(
    query_null_uuid_is_not_fabricated_identity,
    UUID,
    Cell::Uuid(TRACK, TRACK_BYTES),
    Cell::Null
);
bad_selection_test!(
    query_unsupported_bool_is_not_coerced,
    BOOL,
    Cell::Raw(vec![1]),
    Cell::Raw(vec![0])
);
bad_selection_test!(
    query_unsupported_bytea_is_not_coerced,
    BYTEA,
    Cell::Raw(vec![1, 2]),
    Cell::Raw(vec![3, 4])
);

#[derive(Clone, Default)]
struct Deadline(Rc<DeadlineState>);

#[derive(Default)]
struct DeadlineState {
    fired: Counter<bool>,
    waker: RefCell<Option<Waker>>,
}

impl Deadline {
    fn fire(&self) {
        self.0.fired.set(true);
        if let Some(waker) = self.0.waker.borrow_mut().take() {
            waker.wake();
        }
    }

    fn reset(&self) {
        self.0.fired.set(false);
    }
}

impl Future for Deadline {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.fired.get() {
            Poll::Ready(())
        } else {
            *self.0.waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

fn controlled(deadline: Deadline, calls: Rc<Counter<usize>>) -> impl Fn() -> Deadline {
    move || {
        calls.set(calls.get() + 1);
        deadline.clone()
    }
}

async fn enter_gate<F: Future<Output = Result<Answer, DbFailure>>>(
    operation: Pin<&mut F>,
    entered: tokio::sync::oneshot::Receiver<()>,
) {
    bounded(async {
        tokio::select! {
            biased;
            result = operation => panic!("adapter returned before the controlled wire gate: {result:?}"),
            result = entered => result.expect("peer reached the controlled protocol stage"),
        }
    }).await;
}

async fn ready_deadline_case(method: Method) {
    let session = Session::start(vec![]).await;
    let calls = Rc::new(Counter::new(0));
    let counted = calls.clone();
    let db = PgWriterDb::new(&session.client, move || {
        counted.set(counted.get() + 1);
        ready(())
    });
    let result = bounded(method.call(&db)).await;
    session.finish().await;
    assert_sanitized(
        &result.expect_err("already-ready deadline cannot succeed"),
        None,
    );
    assert_eq!(
        calls.get(),
        1,
        "fail-closed stub without a timer is not deadline proof"
    );
}

async fn stalled_prepare_case(method: Method) {
    let (step, gate) = method.step().gated(Stage::Prepare);
    let session = Session::start(vec![step]).await;
    let calls = Rc::new(Counter::new(0));
    let deadline = Deadline::default();
    let db = PgWriterDb::new(&session.client, controlled(deadline.clone(), calls.clone()));
    let mut operation = Box::pin(method.call(&db));
    enter_gate(operation.as_mut(), gate.entered).await;
    deadline.fire();
    let result = bounded(operation.as_mut()).await;
    assert_sanitized(
        &result.expect_err("method deadline must include stalled prepare"),
        None,
    );
    // Completion is observed BEFORE the peer can finish prepare. Dropping the
    // caller await is not a claim that a PostgreSQL server cancelled anything.
    gate.release
        .send(())
        .expect("release late prepare response");
    session.barrier().await;
    assert_eq!(calls.get(), 1);
    drop(operation);
    session.finish_abandoned_prepare().await;
}

async fn stalled_execute_case(method: Method) {
    let (step, gate) = method.late_step().gated(Stage::Execute);
    let session = Session::start(vec![step, method.fresh_step()]).await;
    let calls = Rc::new(Counter::new(0));
    let deadline = Deadline::default();
    let db = PgWriterDb::new(&session.client, controlled(deadline.clone(), calls.clone()));
    let mut operation = Box::pin(method.call(&db));
    enter_gate(operation.as_mut(), gate.entered).await;
    deadline.fire();
    let result = bounded(operation.as_mut()).await;
    assert_sanitized(
        &result.expect_err("stalled driver operation must return before late response"),
        None,
    );
    gate.release
        .send(())
        .expect("release late execution response");
    session.barrier().await;
    deadline.reset();
    let fresh = bounded(method.call(&db)).await;
    assert_eq!(
        fresh,
        Ok(method.fresh_answer()),
        "late success must not leak into the next method"
    );
    assert_eq!(calls.get(), 2);
    drop(operation);
    session.finish().await;
}

async fn ready_tie_case(method: Method) {
    let (step, GateControl { entered, release }) = method.step().gated(Stage::Execute);
    let session = Session::start(vec![step]).await;
    let calls = Rc::new(Counter::new(0));
    let deadline = Deadline::default();
    let db = PgWriterDb::new(&session.client, controlled(deadline.clone(), calls.clone()));
    let mut operation = Box::pin(method.call(&db));
    enter_gate(operation.as_mut(), entered).await;
    release.send(()).expect("make driver success ready");
    // Keep the adapter future unpolled until its real driver response has been
    // drained. The next poll sees BOTH a ready success and a ready deadline.
    session.barrier().await;
    deadline.fire();
    let result = bounded(operation.as_mut()).await;
    assert_sanitized(
        &result.expect_err("deadline must win an actual ready/ready tie"),
        None,
    );
    assert_eq!(calls.get(), 1);
    drop(operation);
    session.finish().await;
}

macro_rules! deadline_tests {
    ($execute:ident, $query:ident, $case:ident) => {
        #[tokio::test]
        async fn $execute() {
            $case(Method::Execute).await;
        }
        #[tokio::test]
        async fn $query() {
            $case(Method::Query).await;
        }
    };
}

deadline_tests!(
    execute_already_ready_deadline_fails_closed,
    query_already_ready_deadline_fails_closed,
    ready_deadline_case
);
deadline_tests!(
    execute_deadline_bounds_stalled_prepare,
    query_deadline_bounds_stalled_prepare,
    stalled_prepare_case
);
deadline_tests!(
    execute_late_response_cannot_reverse_timeout,
    query_late_response_cannot_reverse_timeout,
    stalled_execute_case
);
deadline_tests!(
    execute_deadline_wins_ready_driver_tie,
    query_deadline_wins_ready_driver_tie,
    ready_tie_case
);

// These self-checks establish the peer can drive the actual dependency before
// attributing RED to PgWriterDb. They are not writer-adapter acceptance proof.
#[tokio::test]
async fn peer_self_check_drives_real_client_affected_rows_and_typed_rows() {
    let session = Session::start(vec![probe_execute(1), probe_query()]).await;
    let affected = bounded(
        session
            .client
            .execute(INSERT_PROBE, &[&LABEL, &TIMESTAMP, &70_001_i32]),
    )
    .await
    .expect("peer executes real driver request");
    let rows = bounded(session.client.query(SELECT_PROBE, &[&LABEL]))
        .await
        .expect("peer query");
    assert_eq!(affected, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].try_get::<_, String>(0).expect("text"), LABEL);
    assert_eq!(rows[0].try_get::<_, i64>(1).expect("int8"), TIMESTAMP);
    assert_eq!(rows[0].try_get::<_, i32>(2).expect("int4"), 70_001);
    session.barrier().await;
    session.finish().await;
}

#[tokio::test]
async fn peer_self_check_selected_uuid_has_uuid_oid_and_binary_bytes() {
    use std::error::Error;
    use tokio_postgres::types::{FromSql, Type};

    #[derive(Debug)]
    struct RawUuid(Vec<u8>);
    impl<'a> FromSql<'a> for RawUuid {
        fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
            Ok(Self(raw.to_vec()))
        }
        fn accepts(ty: &Type) -> bool {
            *ty == Type::UUID
        }
    }
    const SQL: &str = "SELECT id FROM wire_probe";
    let session = Session::start(vec![Step::query(
        SQL,
        vec![],
        vec![Column::new("id", UUID)],
        vec![vec![Cell::Uuid(TRACK, TRACK_BYTES)]],
    )])
    .await;
    let rows = bounded(session.client.query(SQL, &[]))
        .await
        .expect("real UUID row");
    assert_eq!(rows[0].columns()[0].type_(), &Type::UUID);
    assert!(
        rows[0].try_get::<_, String>(0).is_err(),
        "fixture must not disguise UUID as text"
    );
    assert_eq!(
        rows[0].try_get::<_, RawUuid>(0).expect("raw UUID").0,
        TRACK_BYTES
    );
    session.finish().await;
}

#[tokio::test]
async fn peer_self_check_drives_prepare_and_execute_errors_without_losing_sqlstate() {
    for (stage, state) in [(Stage::Prepare, "42P01"), (Stage::Execute, "23505")] {
        let session = Session::start(vec![
            probe_execute(1).error(stage, state),
            probe_query().error(stage, state),
        ])
        .await;
        let execute = bounded(
            session
                .client
                .execute(INSERT_PROBE, &[&LABEL, &TIMESTAMP, &70_001_i32]),
        )
        .await
        .expect_err("server execution error fixture");
        let query = bounded(session.client.query(SELECT_PROBE, &[&LABEL]))
            .await
            .expect_err("server query error fixture");
        assert_eq!(execute.code().map(|code| code.code()), Some(state));
        assert_eq!(query.code().map(|code| code.code()), Some(state));
        assert_eq!(
            execute
                .as_db_error()
                .expect("database diagnostic")
                .message(),
            DIAGNOSTIC
        );
        assert_eq!(
            query.as_db_error().expect("database diagnostic").message(),
            DIAGNOSTIC
        );
        session.finish().await;
    }
}

#[tokio::test]
async fn peer_self_check_barrier_makes_unpolled_driver_success_ready() {
    let (step, gate) = probe_query().gated(Stage::Execute);
    let session = Session::start(vec![step]).await;
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&LABEL];
    let mut operation = Box::pin(session.client.query(SELECT_PROBE, &params));
    bounded(async {
        tokio::select! {
            biased;
            result = operation.as_mut() => panic!("query completed before gate: {result:?}"),
            result = gate.entered => result.expect("driver reached Execute"),
        }
    })
    .await;
    gate.release.send(()).expect("release driver result");
    session.barrier().await;
    let rows = match futures_util::poll!(operation.as_mut()) {
        Poll::Ready(Ok(rows)) => rows,
        other => panic!("barrier must make the final driver await ready: {other:?}"),
    };
    assert_eq!(rows[0].try_get::<_, String>(0).expect("ready row"), LABEL);
    drop(operation);
    session.finish().await;
}

#[tokio::test]
async fn peer_self_check_abandoned_prepare_can_drain_without_bind() {
    let (step, gate) = probe_query().gated(Stage::Prepare);
    let session = Session::start(vec![step]).await;
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&LABEL];
    let mut operation = Box::pin(session.client.query(SELECT_PROBE, &params));
    bounded(async {
        tokio::select! {
            biased;
            result = operation.as_mut() => panic!("query completed before prepare gate: {result:?}"),
            result = gate.entered => result.expect("driver reached Parse"),
        }
    }).await;
    drop(operation);
    gate.release.send(()).expect("release abandoned prepare");
    session.barrier().await;
    session.finish_abandoned_prepare().await;
}
