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
                "INSERT INTO library VALUES ('u1','job-1',$1,ARRAY[$2,$2,'stored-invalid']),
             ('u1','job-0',NULL,ARRAY[]::text[]), ('other','private',$3,ARRAY[$2])",
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
            .query(library_page_statement(), &[&caller, &50_i64])
            .await
            .unwrap();
        assert_eq!(page.len(), if caller == "u1" { 5 } else { 1 });
        assert!(page.iter().all(|r| r.get::<_, String>(0) != "private"));
    }
}

#[tokio::test]
async fn absent_mirror_library_preserves_app_rows_and_occurrences() {
    let (store, attempts) = store(true, false).await;
    let rows = store.catalogue_library("u1").await.unwrap();
    let page: Vec<_> = [
        Some(RECORDING),
        Some(RECORDING),
        Some("stored-invalid"),
        None,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, recording)| LibraryPageRow {
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
        assert!(s.catalogue_library("u1").await.unwrap().is_empty());
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
    assert!(s.catalogue_library("u1").await.is_err());
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
    assert!(s.catalogue_library("u1").await.is_err());
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

#[tokio::test]
async fn artist_detail_hides_nonmembers_before_mirror_state() {
    for configured in [false, true] {
        let (s, attempts) = store(true, configured).await;
        // Malformed ids never reach SQL; an empty effective membership
        // never dials the mirror (the other user's private row is not
        // membership).
        assert!(s
            .catalogue_artist("u1", "malformed")
            .await
            .unwrap()
            .is_none());
        assert!(s
            .catalogue_artist("fresh", RELEASE)
            .await
            .unwrap()
            .is_none());
        assert_eq!(attempts.get(), 0);
    }
}

const ARTIST_A: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const ARTIST_B: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
const ARTIST_C: &str = "ffffffff-ffff-4fff-8fff-ffffffffffff";
const ARTIST_D: &str = "99999999-9999-4999-8999-999999999999";
const UNOWNED_RELEASE: &str = "11111111-1111-4111-8111-111111111111";
const SHARED_RELEASE: &str = "22222222-2222-4222-8222-222222222222";
const GROUP: &str = "33333333-3333-4333-8333-333333333333";

/// Mirror stand-in on its own connection: temp tables with the production
/// column types, so the real artist SQL and row decoders run unmodified.
async fn mirror() -> Client {
    let mirror = client(false).await;
    mirror
        .batch_execute(
            "CREATE TEMP TABLE mb_artist (mbid UUID PRIMARY KEY, name TEXT NOT NULL,
               sort_name TEXT NOT NULL, type TEXT, country CHAR(2));
             CREATE TEMP TABLE mb_release (mbid UUID PRIMARY KEY, title TEXT NOT NULL,
               release_type TEXT, year SMALLINT, artist_mbid UUID, release_group_mbid UUID);",
        )
        .await
        .unwrap();
    for (artist, name) in [
        (ARTIST_A, "A"),
        (ARTIST_B, "B"),
        (ARTIST_C, "C"),
        (ARTIST_D, "D"),
    ] {
        mirror
            .execute(
                "INSERT INTO mb_artist VALUES ($1::text::uuid, $2, $2, 'Group', 'GB')",
                &[&artist, &name],
            )
            .await
            .unwrap();
    }
    for (release, artist, year, group) in [
        (RELEASE, ARTIST_A, Some(2001_i16), Some(GROUP)),
        (UNOWNED_RELEASE, ARTIST_A, Some(1990_i16), None),
        (OTHER_RELEASE, ARTIST_B, Some(1999_i16), None),
        (SHARED_RELEASE, ARTIST_C, None, None),
    ] {
        mirror
            .execute(
                "INSERT INTO mb_release VALUES ($1::text::uuid, 'T', 'Album', $3,
                   $2::text::uuid, $4::text::uuid)",
                &[&release, &artist, &year, &group],
            )
            .await
            .unwrap();
    }
    // 51 member releases for ARTIST_D: the store must read bound + 1.
    mirror
        .execute(
            "INSERT INTO mb_release SELECT ('00000000-0000-4000-8000-' ||
               lpad(to_hex(g), 12, '0'))::uuid, 'T', 'Album', 2000, $1::text::uuid, NULL
             FROM generate_series(1, 51) g",
            &[&ARTIST_D],
        )
        .await
        .unwrap();
    mirror
}

async fn artist_store() -> impl Store {
    let app = client(true).await;
    app.batch_execute(
        "INSERT INTO library SELECT 'u1', 'bulk-' || g,
           '00000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'), ARRAY[]::text[]
           FROM generate_series(1, 51) g",
    )
    .await
    .unwrap();
    app.execute(
        "INSERT INTO library VALUES (NULL, 'shared', $1, ARRAY[]::text[])",
        &[&SHARED_RELEASE],
    )
    .await
    .unwrap();
    catalogue_store_for_postgres_test(app, true, Ok(mirror().await), Rc::new(Cell::new(0)))
}

#[tokio::test]
async fn artist_detail_runs_production_sql_with_membership_and_bound() {
    use nntune_api::catalogue::{assemble_artist_detail, CATALOGUE_ARTIST_RELEASES_MAX};
    // Own member: only the member release, with the group id; the artist's
    // unowned release never appears.
    let row = artist_store()
        .await
        .catalogue_artist("u1", &ARTIST_A.to_uppercase())
        .await
        .unwrap()
        .expect("member artist");
    assert_eq!(row.artist.mbid, ARTIST_A);
    assert_eq!(row.artist.artist_type.as_deref(), Some("Group"));
    assert_eq!(row.artist.country.as_deref(), Some("GB"));
    let ids: Vec<_> = row.releases.iter().map(|r| r.mbid.as_str()).collect();
    assert_eq!(ids, [RELEASE]);
    assert_eq!(row.releases[0].release_group_mbid.as_deref(), Some(GROUP));
    // Another user's private release never reveals its artist.
    assert!(artist_store()
        .await
        .catalogue_artist("u1", ARTIST_B)
        .await
        .unwrap()
        .is_none());
    // Shared (NULL-owner) rows reveal their artist to a caller with no
    // ledger of their own, who still cannot see u1's private artist.
    let shared = artist_store()
        .await
        .catalogue_artist("fresh", ARTIST_C)
        .await
        .unwrap()
        .expect("shared artist");
    assert_eq!(shared.releases.len(), 1);
    assert_eq!(shared.releases[0].year, None);
    assert!(artist_store()
        .await
        .catalogue_artist("fresh", ARTIST_A)
        .await
        .unwrap()
        .is_none());
    // Unknown artist with a non-empty membership is the same None.
    assert!(artist_store()
        .await
        .catalogue_artist("u1", "44444444-4444-4444-8444-444444444444")
        .await
        .unwrap()
        .is_none());
    // Bound + 1 rows read, response bounded and flagged truncated.
    let big = artist_store()
        .await
        .catalogue_artist("u1", ARTIST_D)
        .await
        .unwrap()
        .expect("artist with many releases");
    assert_eq!(big.releases.len(), CATALOGUE_ARTIST_RELEASES_MAX + 1);
    let detail = assemble_artist_detail(&big);
    assert_eq!(detail.releases.len(), CATALOGUE_ARTIST_RELEASES_MAX);
    assert!(detail.truncated);
}
