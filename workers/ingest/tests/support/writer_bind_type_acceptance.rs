//! Native behavioral acceptance for PgWriterDb bind-type validation.
//!
//! REAL PgWriterDb over a real tokio-postgres client and an in-memory
//! PostgreSQL protocol peer. No WriterDb mock, live database, credentials,
//! environment skip, or source-shape assertions.
//!
//! Contract: prepared OID versus Rust Value mismatch must reject
//! deterministically with sanitized data-exception 22000 before any wire
//! Bind/Execute, and through the real writer helper as permanent
//! write_failed. No raw driver diagnostics, DSNs, or canaries may escape.
//!
//! This proves driver-boundary typing, NOT PostgreSQL statement execution,
//! server-side cancellation, edge integration, or staging publication.

use super::pg_wire_peer;

use std::future::pending;

use nntune_ingest::ingest_jobs;
use nntune_ingest::retry::{JobError, ERR_WRITE_FAILED};
use nntune_ingest::writer_execution::{self, DbFailure, Value, WriterDb};
use nntune_ingest::writer_transport::PgWriterDb;
use pg_wire_peer::{
    bounded, Cell, Column, Param, Session, Step, BOOL, BPCHAR, BYTEA, DIAGNOSTIC, INT4, INT8, NAME,
    TEXT, UUID, VARCHAR,
};

const OWNER: &str = "wire-owner";
const JOB: &str = "wire_job-7";
const KEY: &str = "ingest:wire-owner:archive-7";
const TIMESTAMP: i64 = 4_294_967_299;
const INT32_PROBE: i32 = 70_001;
const LABEL: &str = "wire 'payload' — 音楽";
const TRACK: &str = "00112233-4455-4677-8899-aabbccddeeff";
const TRACK_BYTES: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];
const MALFORMED_UUID: &str = "not-a-canonical-uuid";
const BIND_EXECUTE: &str = "INSERT INTO wire_probe (probe) VALUES ($1)";
const BIND_QUERY: &str = "SELECT probe FROM wire_probe WHERE probe = $1";

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
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

#[derive(Clone, Copy, Debug)]
enum Method {
    Execute,
    Query,
}

async fn rejects_mismatch(value: Value, expected: Param, inferred: u32, method: Method) {
    match method {
        Method::Execute => {
            let step =
                Step::execute(BIND_EXECUTE, vec![expected], 1).with_param_oids(vec![inferred]);
            let session = Session::start(vec![step]).await;
            let db = PgWriterDb::new(&session.client, pending::<()>);
            let result = bounded(db.execute(BIND_EXECUTE, &[value])).await;
            let trace = session.finish_rejected_params().await;
            assert_sanitized(
                &result.expect_err("OID/Value mismatch must not succeed"),
                Some("22000"),
            );
            assert_eq!(trace.bound, 0, "rejected params must not Bind");
            assert_eq!(trace.completed, 0, "rejected params must not Execute");
        }
        Method::Query => {
            let step = Step::query(
                BIND_QUERY,
                vec![expected],
                vec![Column::new("probe", TEXT)],
                vec![vec![Cell::Text("never")]],
            )
            .with_param_oids(vec![inferred]);
            let session = Session::start(vec![step]).await;
            let db = PgWriterDb::new(&session.client, pending::<()>);
            let result = bounded(db.query(BIND_QUERY, &[value])).await;
            let trace = session.finish_rejected_params().await;
            assert_sanitized(
                &result.expect_err("OID/Value mismatch must not succeed"),
                Some("22000"),
            );
            assert_eq!(trace.bound, 0, "rejected params must not Bind");
            assert_eq!(trace.completed, 0, "rejected params must not Execute");
        }
    }
}

async fn accepts_execute(value: Value, expected: Param, inferred: Option<Vec<u32>>) {
    let mut step = Step::execute(BIND_EXECUTE, vec![expected], 1);
    if let Some(oids) = inferred {
        step = step.with_param_oids(oids);
    }
    let session = Session::start(vec![step]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(db.execute(BIND_EXECUTE, &[value])).await;
    let trace = session.finish().await;
    assert_eq!(result, Ok(1), "exact bind must succeed");
    assert_eq!(trace.bound, 1);
    assert_eq!(trace.completed, 1);
}

async fn accepts_query(
    value: Value,
    expected: Param,
    inferred: Option<Vec<u32>>,
    columns: Vec<Column>,
    rows: Vec<Vec<Cell>>,
    expected_rows: Vec<Vec<Value>>,
) {
    let mut step = Step::query(BIND_QUERY, vec![expected], columns, rows);
    if let Some(oids) = inferred {
        step = step.with_param_oids(oids);
    }
    let session = Session::start(vec![step]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(db.query(BIND_QUERY, &[value])).await;
    let trace = session.finish().await;
    assert_eq!(result, Ok(expected_rows), "exact bind must succeed");
    assert_eq!(trace.bound, 1);
    assert_eq!(trace.completed, 1);
}

async fn accepts_text_family(oid: u32, method: Method) {
    match method {
        Method::Execute => {
            accepts_execute(text(LABEL), Param::Text(LABEL), Some(vec![oid])).await;
        }
        Method::Query => {
            accepts_query(
                text(LABEL),
                Param::Text(LABEL),
                Some(vec![oid]),
                vec![Column::new("probe", TEXT)],
                vec![vec![Cell::Text(LABEL)]],
                vec![vec![text(LABEL)]],
            )
            .await;
        }
    }
}

macro_rules! mismatch_tests {
    ($execute:ident, $query:ident, $value:expr, $expected:expr, $inferred:ident) => {
        #[tokio::test]
        async fn $execute() {
            rejects_mismatch($value, $expected, $inferred, Method::Execute).await;
        }
        #[tokio::test]
        async fn $query() {
            rejects_mismatch($value, $expected, $inferred, Method::Query).await;
        }
    };
}

mismatch_tests!(
    execute_text_to_int8_rejects_with_data_exception,
    query_text_to_int8_rejects_with_data_exception,
    text(LABEL),
    Param::Text(LABEL),
    INT8
);
mismatch_tests!(
    execute_text_to_int4_rejects_with_data_exception,
    query_text_to_int4_rejects_with_data_exception,
    text(LABEL),
    Param::Text(LABEL),
    INT4
);
mismatch_tests!(
    execute_i64_to_text_rejects_with_data_exception,
    query_i64_to_text_rejects_with_data_exception,
    Value::I64(TIMESTAMP),
    Param::I64(TIMESTAMP),
    TEXT
);
mismatch_tests!(
    execute_i32_to_text_rejects_with_data_exception,
    query_i32_to_text_rejects_with_data_exception,
    Value::I32(INT32_PROBE),
    Param::I32(INT32_PROBE),
    TEXT
);
mismatch_tests!(
    execute_i64_to_int4_rejects_with_data_exception,
    query_i64_to_int4_rejects_with_data_exception,
    Value::I64(TIMESTAMP),
    Param::I64(TIMESTAMP),
    INT4
);
mismatch_tests!(
    execute_i32_to_int8_rejects_with_data_exception,
    query_i32_to_int8_rejects_with_data_exception,
    Value::I32(INT32_PROBE),
    Param::I32(INT32_PROBE),
    INT8
);
mismatch_tests!(
    execute_i64_to_uuid_rejects_with_data_exception,
    query_i64_to_uuid_rejects_with_data_exception,
    Value::I64(TIMESTAMP),
    Param::I64(TIMESTAMP),
    UUID
);
mismatch_tests!(
    execute_i32_to_uuid_rejects_with_data_exception,
    query_i32_to_uuid_rejects_with_data_exception,
    Value::I32(INT32_PROBE),
    Param::I32(INT32_PROBE),
    UUID
);
mismatch_tests!(
    execute_text_to_bool_rejects_with_data_exception,
    query_text_to_bool_rejects_with_data_exception,
    text(LABEL),
    Param::Text(LABEL),
    BOOL
);
mismatch_tests!(
    execute_i64_to_bool_rejects_with_data_exception,
    query_i64_to_bool_rejects_with_data_exception,
    Value::I64(TIMESTAMP),
    Param::I64(TIMESTAMP),
    BOOL
);
mismatch_tests!(
    execute_i32_to_bool_rejects_with_data_exception,
    query_i32_to_bool_rejects_with_data_exception,
    Value::I32(INT32_PROBE),
    Param::I32(INT32_PROBE),
    BOOL
);
mismatch_tests!(
    execute_text_to_bytea_rejects_with_data_exception,
    query_text_to_bytea_rejects_with_data_exception,
    text(LABEL),
    Param::Text(LABEL),
    BYTEA
);
mismatch_tests!(
    execute_i64_to_bytea_rejects_with_data_exception,
    query_i64_to_bytea_rejects_with_data_exception,
    Value::I64(TIMESTAMP),
    Param::I64(TIMESTAMP),
    BYTEA
);
mismatch_tests!(
    execute_i32_to_bytea_rejects_with_data_exception,
    query_i32_to_bytea_rejects_with_data_exception,
    Value::I32(INT32_PROBE),
    Param::I32(INT32_PROBE),
    BYTEA
);

#[tokio::test]
async fn execute_exact_int4_binds_and_returns_one() {
    accepts_execute(Value::I32(INT32_PROBE), Param::I32(INT32_PROBE), None).await;
}

#[tokio::test]
async fn query_exact_int4_binds_and_decodes() {
    accepts_query(
        Value::I32(INT32_PROBE),
        Param::I32(INT32_PROBE),
        None,
        vec![Column::new("probe", INT4)],
        vec![vec![Cell::I32(INT32_PROBE)]],
        vec![vec![Value::I32(INT32_PROBE)]],
    )
    .await;
}

#[tokio::test]
async fn execute_exact_int8_binds_and_returns_one() {
    accepts_execute(Value::I64(TIMESTAMP), Param::I64(TIMESTAMP), None).await;
}

#[tokio::test]
async fn query_exact_int8_binds_and_decodes() {
    accepts_query(
        Value::I64(TIMESTAMP),
        Param::I64(TIMESTAMP),
        None,
        vec![Column::new("probe", INT8)],
        vec![vec![Cell::I64(TIMESTAMP)]],
        vec![vec![Value::I64(TIMESTAMP)]],
    )
    .await;
}

#[tokio::test]
async fn execute_uuid_text_binds_uuid_oid() {
    accepts_execute(text(TRACK), Param::Uuid(TRACK, TRACK_BYTES), None).await;
}

#[tokio::test]
async fn query_uuid_text_binds_uuid_oid_and_decodes() {
    accepts_query(
        text(TRACK),
        Param::Uuid(TRACK, TRACK_BYTES),
        None,
        vec![Column::new("probe", UUID)],
        vec![vec![Cell::Uuid(TRACK, TRACK_BYTES)]],
        vec![vec![text(TRACK)]],
    )
    .await;
}

macro_rules! text_family_tests {
    ($execute:ident, $query:ident, $oid:ident) => {
        #[tokio::test]
        async fn $execute() {
            accepts_text_family($oid, Method::Execute).await;
        }
        #[tokio::test]
        async fn $query() {
            accepts_text_family($oid, Method::Query).await;
        }
    };
}

text_family_tests!(execute_text_to_text_binds, query_text_to_text_binds, TEXT);
text_family_tests!(
    execute_text_to_varchar_binds,
    query_text_to_varchar_binds,
    VARCHAR
);
text_family_tests!(
    execute_text_to_bpchar_binds,
    query_text_to_bpchar_binds,
    BPCHAR
);
text_family_tests!(execute_text_to_name_binds, query_text_to_name_binds, NAME);

async fn rejects_malformed_uuid(method: Method) {
    rejects_mismatch(
        text(MALFORMED_UUID),
        Param::Text(MALFORMED_UUID),
        UUID,
        method,
    )
    .await;
}

#[tokio::test]
async fn execute_malformed_uuid_rejects_with_data_exception() {
    rejects_malformed_uuid(Method::Execute).await;
}

#[tokio::test]
async fn query_malformed_uuid_rejects_with_data_exception() {
    rejects_malformed_uuid(Method::Query).await;
}

async fn rejects_count(values: Vec<Value>, method: Method) {
    match method {
        Method::Execute => {
            let step = Step::execute(BIND_EXECUTE, vec![Param::Text(LABEL)], 1);
            let session = Session::start(vec![step]).await;
            let db = PgWriterDb::new(&session.client, pending::<()>);
            let result = bounded(db.execute(BIND_EXECUTE, &values)).await;
            let trace = session.finish_rejected_params().await;
            assert_sanitized(
                &result.expect_err("parameter count mismatch must not succeed"),
                Some("22000"),
            );
            assert_eq!(trace.bound, 0);
            assert_eq!(trace.completed, 0);
        }
        Method::Query => {
            let step = Step::query(
                BIND_QUERY,
                vec![Param::Text(LABEL)],
                vec![Column::new("probe", TEXT)],
                vec![vec![Cell::Text(LABEL)]],
            );
            let session = Session::start(vec![step]).await;
            let db = PgWriterDb::new(&session.client, pending::<()>);
            let result = bounded(db.query(BIND_QUERY, &values)).await;
            let trace = session.finish_rejected_params().await;
            assert_sanitized(
                &result.expect_err("parameter count mismatch must not succeed"),
                Some("22000"),
            );
            assert_eq!(trace.bound, 0);
            assert_eq!(trace.completed, 0);
        }
    }
}

#[tokio::test]
async fn execute_too_few_params_rejects_with_data_exception() {
    rejects_count(vec![], Method::Execute).await;
}

#[tokio::test]
async fn query_too_few_params_rejects_with_data_exception() {
    rejects_count(vec![], Method::Query).await;
}

#[tokio::test]
async fn execute_too_many_params_rejects_with_data_exception() {
    rejects_count(vec![text(LABEL), text(LABEL)], Method::Execute).await;
}

#[tokio::test]
async fn query_too_many_params_rejects_with_data_exception() {
    rejects_count(vec![text(LABEL), text(LABEL)], Method::Query).await;
}

#[tokio::test]
async fn execute_without_sqlstate_stays_transient() {
    let session = Session::start(vec![Step::execute(
        BIND_EXECUTE,
        vec![Param::Text(LABEL)],
        1,
    )
    .disconnect()])
    .await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let direct = bounded(db.execute(BIND_EXECUTE, &[text(LABEL)])).await;
    session.finish().await;
    assert_sanitized(&direct.expect_err("disconnect must fail closed"), None);

    let session = Session::start(vec![Step::execute(
        ingest_jobs::upsert_ingest_job_sql(),
        vec![
            Param::Text(JOB),
            Param::Text(OWNER),
            Param::Text(KEY),
            Param::I64(TIMESTAMP),
        ],
        1,
    )
    .disconnect()])
    .await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let classified = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    session.finish().await;
    assert_classified(
        &classified.expect_err("disconnect must remain retryable"),
        true,
    );
}

#[tokio::test]
async fn query_without_sqlstate_stays_transient() {
    let session = Session::start(vec![Step::query(
        BIND_QUERY,
        vec![Param::Text(LABEL)],
        vec![Column::new("probe", TEXT)],
        vec![vec![Cell::Text(LABEL)]],
    )
    .disconnect()])
    .await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let direct = bounded(db.query(BIND_QUERY, &[text(LABEL)])).await;
    session.finish().await;
    assert_sanitized(&direct.expect_err("disconnect must fail closed"), None);

    let admit = Step::execute(
        ingest_jobs::upsert_ingest_job_sql(),
        vec![
            Param::Text(JOB),
            Param::Text(OWNER),
            Param::Text(KEY),
            Param::I64(TIMESTAMP),
        ],
        0,
    );
    let reselect = Step::query(
        ingest_jobs::select_ingest_job_identity_sql(),
        vec![Param::Text(JOB)],
        vec![
            Column::new("user_id", TEXT),
            Column::new("idempotency_key", TEXT),
        ],
        vec![],
    )
    .disconnect();
    let session = Session::start(vec![admit, reselect]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let classified = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    session.finish().await;
    assert_classified(
        &classified.expect_err("disconnect must remain retryable"),
        true,
    );
}

#[tokio::test]
async fn admission_execute_text_to_int8_is_permanent_write_failed() {
    let step = Step::execute(
        ingest_jobs::upsert_ingest_job_sql(),
        vec![
            Param::Text(JOB),
            Param::Text(OWNER),
            Param::Text(KEY),
            Param::I64(TIMESTAMP),
        ],
        1,
    )
    .with_param_oids(vec![INT8, TEXT, TEXT, INT8]);
    let session = Session::start(vec![step]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    let trace = session.finish_rejected_params().await;
    assert_classified(
        &result.expect_err("admission OID mismatch must not admit"),
        false,
    );
    assert_eq!(trace.bound, 0);
    assert_eq!(trace.completed, 0);
}

#[tokio::test]
async fn admission_query_text_to_int8_is_permanent_write_failed() {
    let admit = Step::execute(
        ingest_jobs::upsert_ingest_job_sql(),
        vec![
            Param::Text(JOB),
            Param::Text(OWNER),
            Param::Text(KEY),
            Param::I64(TIMESTAMP),
        ],
        0,
    );
    let reselect = Step::query(
        ingest_jobs::select_ingest_job_identity_sql(),
        vec![Param::Text(JOB)],
        vec![
            Column::new("user_id", TEXT),
            Column::new("idempotency_key", TEXT),
        ],
        vec![vec![Cell::Text(OWNER), Cell::Text(KEY)]],
    )
    .with_param_oids(vec![INT8]);
    let session = Session::start(vec![admit, reselect]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::admit_job(&db, JOB, OWNER, KEY, TIMESTAMP)).await;
    let trace = session.finish_partial().await;
    assert_classified(
        &result.expect_err("reselect OID mismatch must not admit"),
        false,
    );
    assert_eq!(trace.prepared, 2, "both statements prepare");
    assert_eq!(trace.bound, 1, "only the admission may Bind");
    assert_eq!(trace.completed, 1, "only the admission may Execute");
}
