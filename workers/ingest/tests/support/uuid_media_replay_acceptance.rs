//! UUID identity is case-insensitive at the PgWriterDb boundary. These calls
//! cross the real executor, real UUID binary binds/results and ordered peer;
//! a lower-case Value-only WriterDb script would miss the case regression.

use super::*;

const UPPER_TRACK: &str = "00112233-4455-4677-8899-AABBCCDDEEFF";
const MIXED_TRACK: &str = "00112233-4455-4677-8899-aAbBcCdDeEfF";

async fn same_uuid_replay(track_id: &str, duplicate_grant: bool) {
    let publication = MediaPublication {
        track_id: track_id.to_owned(),
        ..publication()
    };
    let mut steps = vec![
        media_insert(0),
        media_reselect(),
        grant_insert(u64::from(!duplicate_grant)),
    ];
    if duplicate_grant {
        steps.push(first_grant_reselect());
    }
    let expected = steps.len();
    let session = Session::start(steps).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::publish_media(&db, &publication)).await;
    // Drain without demanding a grant first: the regression must fail on its
    // false identity refusal, not on the peer's unconsumed-tail assertion.
    let trace = session.finish_partial().await;
    assert_eq!(
        result,
        Ok(()),
        "the selected UUID bytes identify the same track for {track_id}"
    );
    assert_eq!(trace.prepared, expected);
    assert_eq!(trace.bound, expected);
    assert_eq!(
        trace.completed, expected,
        "media reselect precedes the grant"
    );
}

macro_rules! same_uuid_tests {
    ($new_grant:ident, $first_grant:ident, $track:expr) => {
        #[tokio::test]
        async fn $new_grant() {
            same_uuid_replay($track, false).await;
        }
        #[tokio::test]
        async fn $first_grant() {
            same_uuid_replay($track, true).await;
        }
    };
}

same_uuid_tests!(
    lowercase_uuid_media_replay_reaches_grant,
    lowercase_uuid_media_replay_preserves_first_provider,
    TRACK
);
same_uuid_tests!(
    uppercase_uuid_media_replay_reaches_grant,
    uppercase_uuid_media_replay_preserves_first_provider,
    UPPER_TRACK
);
same_uuid_tests!(
    mixed_case_uuid_media_replay_reaches_grant,
    mixed_case_uuid_media_replay_preserves_first_provider,
    MIXED_TRACK
);

fn selected_media(row: Vec<Cell>) -> Step {
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
        vec![row],
    )
}

fn stored_media_row() -> Vec<Cell> {
    vec![
        Cell::Text(OWNER),
        Cell::Uuid(TRACK, TRACK_BYTES),
        Cell::Text(JOB),
        Cell::Text(HASH),
        Cell::Text("opus"),
        Cell::I32(64),
    ]
}

async fn wrong_selected_field(column: usize, wrong: Cell) {
    let mut row = stored_media_row();
    row[column] = wrong;
    let session = Session::start(vec![media_insert(0), selected_media(row)]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::publish_media(&db, &publication())).await;
    let trace = session.finish().await;
    let error = result.expect_err("a genuinely different media identity/profile cannot converge");
    assert_classified(&error, false);
    assert!(!error.made_progress);
    assert_eq!(error.retry_after_secs, None);
    assert_eq!(trace.bound, 2, "refused media must not Bind any grant");
    assert_eq!(trace.completed, 2);
}

macro_rules! wrong_selected_tests {
    ($name:ident, $column:literal, $wrong:expr) => {
        #[tokio::test]
        async fn $name() {
            wrong_selected_field($column, $wrong).await;
        }
    };
}

wrong_selected_tests!(
    different_uuid_bytes_refuse_before_grant,
    1,
    Cell::Uuid(OTHER_TRACK, OTHER_TRACK_BYTES)
);
wrong_selected_tests!(
    wrong_owner_refuses_before_grant,
    0,
    Cell::Text("other-owner")
);
wrong_selected_tests!(
    wrong_source_job_refuses_before_grant,
    2,
    Cell::Text("other_job-8")
);
wrong_selected_tests!(
    wrong_content_hash_refuses_before_grant,
    3,
    Cell::Text("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
);
wrong_selected_tests!(wrong_codec_refuses_before_grant, 4, Cell::Text("mp3"));
wrong_selected_tests!(wrong_bitrate_refuses_before_grant, 5, Cell::I32(96));

#[tokio::test]
async fn uppercase_uuid_new_media_uses_the_same_binary_track_identity() {
    let publication = MediaPublication {
        track_id: UPPER_TRACK.to_owned(),
        ..publication()
    };
    let session = Session::start(vec![media_insert(1), grant_insert(1)]).await;
    let db = PgWriterDb::new(&session.client, pending::<()>);
    let result = bounded(writer_execution::publish_media(&db, &publication)).await;
    let trace = session.finish().await;
    assert_eq!(result, Ok(()));
    assert_eq!(trace.bound, 2);
}

#[tokio::test]
async fn malformed_track_uuid_is_refused_before_any_driver_io() {
    for track_id in [
        "not-a-uuid",
        "00112233-4455-4677-8899-AABBCCDDEEFG",
        "00112233-4455-4677-8899-AABBCCDDEEF",
        "00112233445546778899aabbccddeeff",
    ] {
        let publication = MediaPublication {
            track_id: track_id.to_owned(),
            ..publication()
        };
        let session = Session::start(vec![]).await;
        let db = PgWriterDb::new(&session.client, pending::<()>);
        let result = bounded(writer_execution::publish_media(&db, &publication)).await;
        let trace = session.finish().await;
        assert_classified(
            &result.expect_err("malformed track UUID cannot publish"),
            false,
        );
        assert_eq!(trace.prepared, 0);
        assert_eq!(trace.bound, 0);
        assert_eq!(trace.completed, 0);
    }
}
