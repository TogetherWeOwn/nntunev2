//! Production catalogue reads with only connection acquisition injected.
//! CI's disposable PostgreSQL owns the app rows; no mirror service is required.
use nntune_api::catalogue::{assemble_library_rows, LibraryPageRow};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::worker_entry::catalogue_store_for_postgres_test;
use std::cell::Cell;
use std::rc::Rc;
use tokio_postgres::{Client, Config, NoTls};

const RELEASE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const RECORDING: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const OTHER_RELEASE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

async fn client(populated: bool) -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client.batch_execute(
        "SET statement_timeout = '3s'; SET search_path = pg_temp;
         CREATE TEMP TABLE library (user_id TEXT, job_id TEXT, release_mbid TEXT, recording_mbids TEXT[], idempotency_key TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text)",
    ).await.unwrap();
    if populated {
        // Duplicate recordings and malformed stored MBIDs must survive library
        // degradation. The other user's row must never become membership.
        client
            .execute(
                "INSERT INTO library VALUES ('u1','job-1',$1,ARRAY[$2,$2,'stored-invalid'],'key-1'),
             ('u1','job-0',NULL,ARRAY[]::text[],'key-0'), ('other','private',$3,ARRAY[$2],'key-private')",
                &[&RELEASE, &RECORDING, &OTHER_RELEASE],
            )
            .await
            .unwrap();
    }
    client
}

async fn store(populated: bool, configured: bool) -> (impl Store, Rc<Cell<usize>>) {
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(
        client(populated).await,
        configured,
        Err(StoreError),
        attempts.clone(),
    );
    (store, attempts)
}

#[tokio::test]
async fn library_continuation_binds_every_key_and_preserves_owner_scope() {
    use nntune_api::catalogue::LibraryCursor;
    for caller in ["u1", "fresh"] {
        let app = client(false).await;
        app.execute(
            "INSERT INTO library SELECT NULL, 'same-job', $1, ARRAY[$2], 'shared-' || lpad(g::text, 3, '0')
             FROM generate_series(1,55) g",
            &[&RELEASE, &RECORDING],
        ).await.unwrap();
        app.execute(
            "INSERT INTO library VALUES ('u1','same-job',$1,ARRAY[$2],'own'),
             ('other','same-job',$1,ARRAY[$2],'other')",
            &[&RELEASE, &RECORDING],
        )
        .await
        .unwrap();
        let attempts = Rc::new(Cell::new(0));
        let s = catalogue_store_for_postgres_test(app, false, Err(StoreError), attempts.clone());
        let mut after = None;
        let mut seen = std::collections::HashSet::new();
        for turn in 0..3 {
            let mut rows = s.catalogue_library(caller, after.as_ref()).await.unwrap();
            if turn == 0 {
                assert_eq!(rows.len(), 51, "one lookahead row");
            }
            let more = rows.len() > 50;
            rows.truncate(50);
            for row in &rows {
                assert!(seen.insert(row.idempotency_key.clone()));
            }
            if !more {
                break;
            }
            after = rows.last().map(LibraryCursor::from_row);
        }
        assert_eq!(seen.len(), if caller == "u1" { 56 } else { 55 });
        assert_eq!(seen.contains("own"), caller == "u1");
        assert!(!seen.contains("other"));
        assert_eq!(attempts.get(), 0);
    }
}

#[tokio::test]
async fn shared_memberships_use_production_queries_without_private_leaks() {
    use nntune_api::catalogue::*;
    let app = client(true).await;
    app.execute(
        "INSERT INTO library VALUES (NULL, 'shared', $1, ARRAY[$2])",
        &[&OTHER_RELEASE, &RECORDING],
    )
    .await
    .unwrap();
    for caller in ["u1", "fresh-a", "fresh-b"] {
        let releases = app
            .query(catalogue_member_releases_statement(), &[&caller])
            .await
            .unwrap();
        let ids: Vec<String> = releases.iter().map(|r| r.get(0)).collect();
        assert!(ids.iter().any(|id| id == OTHER_RELEASE));
        assert_eq!(ids.iter().any(|id| id == RELEASE), caller == "u1");
        let pairs = app
            .query(catalogue_member_track_pairs_statement(), &[&caller])
            .await
            .unwrap();
        assert_eq!(pairs.len(), if caller == "u1" { 3 } else { 1 });
        for release in [RELEASE, OTHER_RELEASE] {
            let is_member = caller == "u1" || release == OTHER_RELEASE;
            assert_eq!(
                app.query(
                    catalogue_release_membership_statement(),
                    &[&caller, &release]
                )
                .await
                .unwrap()
                .len(),
                usize::from(is_member)
            );
            let recordings = app
                .query(
                    catalogue_member_release_recordings_statement(),
                    &[&caller, &release],
                )
                .await
                .unwrap();
            assert_eq!(!recordings.is_empty(), is_member);
        }
        let page = app
            .query(
                library_page_statement(),
                &[
                    &caller,
                    &50_i64,
                    &None::<bool>,
                    &None::<String>,
                    &None::<i64>,
                    &None::<String>,
                ],
            )
            .await
            .unwrap();
        assert_eq!(page.len(), if caller == "u1" { 5 } else { 1 });
        assert!(page.iter().all(|r| r.get::<_, String>(0) != "private"));
    }
}

#[tokio::test]
async fn absent_mirror_library_preserves_app_rows_and_occurrences() {
    let (store, attempts) = store(true, false).await;
    let rows = store.catalogue_library("u1", None).await.unwrap();
    let page: Vec<_> = [
        Some(RECORDING),
        Some(RECORDING),
        Some("stored-invalid"),
        None,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, recording)| LibraryPageRow {
        idempotency_key: if recording.is_some() {
            "key-1"
        } else {
            "key-0"
        }
        .into(),
        job_id: if recording.is_some() {
            "job-1"
        } else {
            "job-0"
        }
        .into(),
        release_mbid: recording.map(|_| RELEASE.into()),
        recording_mbids: if recording.is_some() {
            vec![RECORDING.into(), RECORDING.into(), "stored-invalid".into()]
        } else {
            vec![]
        },
        recording_mbid: recording.map(str::to_owned),
        recording_ord: if recording.is_some() { i as i64 + 1 } else { 0 },
    })
    .collect();
    assert_eq!(rows, assemble_library_rows(&page, &[], &[], &[]));
    assert_ne!(
        nntune_api::catalogue::library_entry(&rows[0]).occurrence_key,
        nntune_api::catalogue::library_entry(&rows[1]).occurrence_key
    );
    assert_eq!(attempts.get(), 0, "absence is not a failed connection");
}

#[tokio::test]
async fn empty_app_membership_never_connects_in_either_mode() {
    for configured in [false, true] {
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_library("u1", None).await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_artists("u1").await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_releases("u1").await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_tracks("u1").await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
    }
}

#[tokio::test]
async fn empty_effective_release_recordings_need_no_mirror() {
    for configured in [false, true] {
        for recordings in [vec![], vec!["stored-invalid".to_owned()]] {
            let app = client(true).await;
            app.execute(
                "UPDATE library SET recording_mbids = $1 WHERE user_id = 'u1'",
                &[&recordings],
            )
            .await
            .unwrap();
            let attempts = Rc::new(Cell::new(0));
            let s = catalogue_store_for_postgres_test(
                app,
                configured,
                Err(StoreError),
                attempts.clone(),
            );
            assert!(s
                .catalogue_release_tracks("u1", RELEASE)
                .await
                .unwrap()
                .is_empty());
            assert_eq!(attempts.get(), 0);
        }
    }
}

#[tokio::test]
async fn nonempty_entities_fail_without_dialing_an_absent_binding() {
    for configured in [false, true] {
        let expected_attempts = usize::from(configured);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_artists("u1").await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_releases("u1").await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_tracks("u1").await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_release("u1", RELEASE).await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_release_tracks("u1", RELEASE).await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
    }
}

#[tokio::test]
async fn configured_library_connect_and_statement_failures_never_degrade() {
    let (s, attempts) = store(true, true).await;
    assert!(s.catalogue_library("u1", None).await.is_err());
    assert_eq!(attempts.get(), 1);
    // A connected mirror without its required tables must propagate the real
    // statement failure, not serve the same null-enriched rows as absence.
    let attempts = Rc::new(Cell::new(0));
    let s = catalogue_store_for_postgres_test(
        client(true).await,
        true,
        Ok(client(false).await),
        attempts.clone(),
    );
    assert!(s.catalogue_library("u1", None).await.is_err());
    assert_eq!(attempts.get(), 1);
}

#[tokio::test]
async fn release_membership_hides_nonmembers_before_mirror_state() {
    for configured in [false, true] {
        for mbid in ["malformed", OTHER_RELEASE] {
            let (s, attempts) = store(true, configured).await;
            assert!(s.catalogue_release("u1", mbid).await.unwrap().is_none());
            assert_eq!(attempts.get(), 0);
            let (s, attempts) = store(true, configured).await;
            assert!(s
                .catalogue_release_tracks("u1", mbid)
                .await
                .unwrap()
                .is_empty());
            assert_eq!(attempts.get(), 0);
        }
    }
}
