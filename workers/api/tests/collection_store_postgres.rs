//! Production collection-saves statements with only connection acquisition
//! injected. CI's disposable PostgreSQL owns the app rows and the mirror
//! probes (temp `mb_*` shapes on the same disposable cluster); no mirror
//! service is required.
use nntune_api::collection::{SaveKind, COLLECTION_PAGE_MAX};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::worker_entry::catalogue_store_for_postgres_test;
use std::cell::Cell;
use std::rc::Rc;
use tokio_postgres::{Client, Config, NoTls};

const ALBUM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_ALBUM: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const SHARED_ALBUM: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const ARTIST: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const OTHER_ARTIST: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";

/// App client seeded with the collection users/ledger + a temp mirror
/// carrying the releases/artists the probes join against. Temp tables
/// satisfy the unqualified names the production statements use.
async fn client() -> Client {
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
         CREATE TEMP TABLE users (id TEXT PRIMARY KEY, deleting_at BIGINT);
         CREATE TEMP TABLE library (user_id TEXT, job_id TEXT, release_mbid TEXT, recording_mbids TEXT[], idempotency_key TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text);
         CREATE TEMP TABLE collection_saves (user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE, kind TEXT NOT NULL CHECK (kind IN ('album','artist')), mbid TEXT NOT NULL, saved_at BIGINT NOT NULL CHECK (saved_at >= 0), PRIMARY KEY (user_id, kind, mbid));
         CREATE TEMP TABLE mb_artist (mbid UUID PRIMARY KEY, name TEXT, sort_name TEXT, type TEXT, country CHAR(2));
         CREATE TEMP TABLE mb_release (mbid UUID PRIMARY KEY, title TEXT, release_type TEXT, year SMALLINT, artist_mbid UUID REFERENCES mb_artist(mbid));
         INSERT INTO users (id) VALUES ('u1'), ('u2');
         INSERT INTO mb_artist VALUES
             ('dddddddd-dddd-4ddd-8ddd-dddddddddddd','New Order','New Order','Group','GB'),
             ('eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee','Stranger','Stranger','Person','US');
         INSERT INTO mb_release VALUES
             ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','Power, Corruption & Lies','Album',1983,'dddddddd-dddd-4ddd-8ddd-dddddddddddd'),
             ('bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb','Low-Life','Album',1985,'dddddddd-dddd-4ddd-8ddd-dddddddddddd'),
             ('cccccccc-cccc-4ccc-8ccc-cccccccccccc','Shared Release','Album',1986,'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee');
         INSERT INTO library (user_id, job_id, release_mbid) VALUES
             ('u1','job-1','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'),
             ('other','job-p','bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'),
             (NULL,'job-s','cccccccc-cccc-4ccc-8ccc-cccccccccccc');",
    )
    .await
    .unwrap();
    client
}

/// Store whose app AND mirror connections run on the seeded disposable
/// client (temp `mb_*` tables make the mirror statements execute
/// against real data).
async fn store(app: Client) -> impl Store {
    let attempts = Rc::new(Cell::new(0));
    let mirror_client = client().await;
    catalogue_store_for_postgres_test(app, true, Ok(mirror_client), attempts)
}

#[tokio::test]
async fn save_replays_first_instant_and_remove_is_idempotent() {
    let s = store(client().await).await;
    assert_eq!(
        s.collection_save("u1", SaveKind::Album, ALBUM, 1_700_000_000)
            .await
            .unwrap(),
        Some(1_700_000_000)
    );
    // Replay lands nothing new and returns the ORIGINAL instant.
    assert_eq!(
        s.collection_save("u1", SaveKind::Album, ALBUM, 1_800_000_000)
            .await
            .unwrap(),
        Some(1_700_000_000)
    );
    assert_eq!(
        s.collection_save_state("u1", SaveKind::Album, ALBUM)
            .await
            .unwrap(),
        Some(1_700_000_000)
    );
    // Other users and other kinds never see the row.
    assert_eq!(
        s.collection_save_state("u2", SaveKind::Album, ALBUM)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        s.collection_save_state("u1", SaveKind::Artist, ALBUM)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        s.collection_remove("u1", SaveKind::Album, ALBUM)
            .await
            .unwrap(),
        1
    );
    // Removing an unsaved MBID is the same success.
    assert_eq!(
        s.collection_remove("u1", SaveKind::Album, ALBUM)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        s.collection_save_state("u1", SaveKind::Album, ALBUM)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn page_is_owner_scoped_bounded_and_ordered() {
    let app = client().await;
    // Seed 60 saves for u1 (past the page bound) plus a foreign row that
    // must never surface.
    let mut sql =
        String::from("INSERT INTO collection_saves (user_id, kind, mbid, saved_at) VALUES ");
    for i in 0..60 {
        sql.push_str(&format!(
            "('u1','album','00000000-0000-4000-8000-{i:012x}',{i}),"
        ));
    }
    sql.push_str(&format!("('u2','album','{ALBUM}',9000)"));
    app.batch_execute(&sql).await.unwrap();
    let s = store(app).await;
    let page = s.collection_save_page("u1", SaveKind::Album).await.unwrap();
    assert_eq!(page.len(), COLLECTION_PAGE_MAX);
    // newest first; same-instant ties break on mbid ascending.
    assert!(page
        .windows(2)
        .all(|w| (w[0].saved_at, std::cmp::Reverse(&w[0].mbid))
            >= (w[1].saved_at, std::cmp::Reverse(&w[1].mbid))));
    assert!(page.iter().all(|row| row.mbid != ALBUM));
    let foreign = s.collection_save_page("u2", SaveKind::Album).await.unwrap();
    assert_eq!(foreign.len(), 1);
}

#[tokio::test]
async fn deleting_account_fences_writes_but_reads_stay() {
    let app = client().await;
    app.batch_execute(&format!(
        "INSERT INTO collection_saves (user_id, kind, mbid, saved_at) VALUES ('u1','album','{ALBUM}',42); \
         UPDATE users SET deleting_at = 1700000000 WHERE id = 'u1'"
    ))
    .await
    .unwrap();
    let s = store(app).await;
    assert_eq!(
        s.collection_save("u1", SaveKind::Album, OTHER_ALBUM, 42)
            .await
            .unwrap(),
        None,
        "fenced save lands nothing"
    );
    assert_eq!(
        s.collection_remove("u1", SaveKind::Album, ALBUM)
            .await
            .unwrap(),
        0,
        "fenced removal affects no row (the handler splits with account_deleting)"
    );
    // Reads are unaffected by the marker.
    assert_eq!(
        s.collection_save_state("u1", SaveKind::Album, ALBUM)
            .await
            .unwrap(),
        Some(42)
    );
    assert_eq!(
        s.collection_save_page("u1", SaveKind::Album)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn account_deletion_cascades_save_rows() {
    let app = client().await;
    app.batch_execute(&format!(
        "INSERT INTO collection_saves (user_id, kind, mbid, saved_at) VALUES \
         ('u1','album','{ALBUM}',1), ('u1','artist','{ARTIST}',2), ('u2','album','{ALBUM}',3); \
         DELETE FROM users WHERE id = 'u1'"
    ))
    .await
    .unwrap();
    let rows = app
        .query(
            "SELECT mbid FROM collection_saves WHERE user_id = 'u1'",
            &[],
        )
        .await
        .unwrap();
    assert!(rows.is_empty(), "CASCADE removed every preference row");
    let remaining = app
        .query("SELECT mbid FROM collection_saves", &[])
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1, "other accounts' rows survive");
}

#[tokio::test]
async fn artist_membership_requires_mirror_row_and_member_release() {
    let s = store(client().await).await;
    // ARTIST owns u1's member release AND u2's non-member release.
    assert!(s.collection_artist_member("u1", ARTIST).await.unwrap());
    assert!(s.collection_artist_member("u2", ARTIST).await.unwrap());
    // OTHER_ARTIST owns only the shared release: member for every caller.
    assert!(s
        .collection_artist_member("u1", OTHER_ARTIST)
        .await
        .unwrap());
    assert!(s
        .collection_artist_member("fresh", OTHER_ARTIST)
        .await
        .unwrap());
    // An artist with no release at all is not member-reachable.
    assert!(!s
        .collection_artist_member("u1", "99999999-9999-4999-8999-999999999999")
        .await
        .unwrap());
    // Malformed targets answer false before any mirror work.
    assert!(!s.collection_artist_member("u1", "junk").await.unwrap());
}

#[tokio::test]
async fn mirror_enrichment_reads_return_only_matching_rows() {
    let s = store(client().await).await;
    let releases = s
        .collection_releases(&[ALBUM.to_string(), OTHER_ALBUM.to_string()])
        .await
        .unwrap();
    assert_eq!(releases.len(), 2);
    assert!(releases
        .iter()
        .all(|row| row.artist_name.as_deref() == Some("New Order")));
    // Unknown MBIDs degrade silently to absent enrichment.
    let partial = s
        .collection_releases(&["99999999-9999-4999-8999-999999999999".to_string()])
        .await
        .unwrap();
    assert!(partial.is_empty());
    let artists = s.collection_artists(&[ARTIST.to_string()]).await.unwrap();
    assert_eq!(artists.len(), 1);
    assert_eq!(artists[0].name, "New Order");
}

#[tokio::test]
async fn collection_writes_fail_closed_on_statement_error() {
    // Drop the table so every statement fails like a production outage.
    let app = client().await;
    app.batch_execute("DROP TABLE collection_saves")
        .await
        .unwrap();
    let s = catalogue_store_for_postgres_test(app, true, Err(StoreError), Rc::new(Cell::new(0)));
    assert!(s
        .collection_save("u1", SaveKind::Album, ALBUM, 1)
        .await
        .is_err());
    assert!(s
        .collection_remove("u1", SaveKind::Album, ALBUM)
        .await
        .is_err());
    assert!(s
        .collection_save_state("u1", SaveKind::Album, ALBUM)
        .await
        .is_err());
    assert!(s.collection_save_page("u1", SaveKind::Album).await.is_err());
}
