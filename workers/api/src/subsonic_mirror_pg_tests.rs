//! Disposable PostgreSQL only, invoked by the required Actions migration job.
use super::*;
use tokio_postgres::{Client, Config, NoTls};

async fn database(name: &str) -> Client {
    let mut config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty());
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.dbname(name).connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
}

#[tokio::test]
async fn staging_mirror_counts_combined_text_and_keeps_exact_occurrences() {
    const ARTIST: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const RELEASE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const OTHER: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    const RECORDING: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    // One artist, two releases, two exact-pair occurrences. UUIDs are 36
    // bytes in the receipt and all remaining non-null text is one byte.
    const TEXT_BYTES: i32 = 38 + 2 * 74 + 2 * 111;
    const CARRIED: usize = 17;
    let mut client = database("nntune_ci").await;
    client.batch_execute(
        "SET search_path = pg_temp;
         CREATE TEMP TABLE mb_artist (mbid uuid, name text, sort_name text, type text, country text);
         CREATE TEMP TABLE mb_release (mbid uuid, title text, release_type text, year smallint, artist_mbid uuid);
         CREATE TEMP TABLE mb_recording (mbid uuid, title text, duration_ms integer);
         CREATE TEMP TABLE mb_track (release_mbid uuid, recording_mbid uuid, medium_no smallint, track_no smallint);",
    ).await.unwrap();
    client
        .execute(
            "INSERT INTO mb_artist VALUES ($1::text::uuid, 'a', 'a', NULL, NULL)",
            &[&ARTIST],
        )
        .await
        .unwrap();
    client.execute("INSERT INTO mb_release VALUES ($2::text::uuid, 'c', NULL, 2023, $3::text::uuid), ($1::text::uuid, 'b', NULL, 2024, $3::text::uuid)", &[&RELEASE, &OTHER, &ARTIST]).await.unwrap();
    client
        .execute(
            "INSERT INTO mb_recording VALUES ($1::text::uuid, 'd', 123)",
            &[&RECORDING],
        )
        .await
        .unwrap();
    client.execute("INSERT INTO mb_track VALUES ($1::text::uuid, $3::text::uuid, 1, 2), ($2::text::uuid, $3::text::uuid, 1, 1), ($1::text::uuid, $3::text::uuid, 1, 1)", &[&RELEASE, &OTHER, &RECORDING]).await.unwrap();
    let releases = [RELEASE.into(), OTHER.into()];
    let pairs = [
        (RELEASE.into(), RECORDING.into()),
        (OTHER.into(), ARTIST.into()),
    ];
    let member = |remaining_text_bytes| MirrorSelection {
        releases: &releases,
        pairs: &pairs,
        remaining_text_bytes,
    };
    // No mirror tables in this fixture: rejection must precede building a probe.
    let mut latin1 = database("nntune_ci_latin1").await;
    let mut budget = ConstructionBudget::new(16 * 1024);
    let result = read_mirror(&mut latin1, &mut budget, member(TEXT_BYTES), 2).await;
    assert!(result.is_err());
    assert_eq!(budget.charged(), 0);

    budget.charge(CARRIED, 1, 1).unwrap();
    let (artists, releases, tracks) = read_mirror(&mut client, &mut budget, member(TEXT_BYTES), 2)
        .await
        .unwrap();
    assert_eq!((artists.len(), releases.len(), tracks.len()), (1, 2, 2));
    assert_eq!(
        (artists.capacity(), releases.capacity(), tracks.capacity()),
        (1, 2, 2)
    );
    assert_eq!(releases[0].year, Some(2024));
    assert_eq!(tracks[0].release_mbid, RELEASE);
    assert_eq!(tracks[1].release_mbid, RELEASE);
    assert_eq!((tracks[0].track_no, tracks[1].track_no), (Some(1), Some(2)));
    let exact = budget.charged();
    for (ceiling, bytes, cap, accepted) in [
        (exact - 1, TEXT_BYTES, 2, false),
        (exact, TEXT_BYTES, 2, true),
        (16 * 1024, TEXT_BYTES - 1, 2, false),
        (16 * 1024, TEXT_BYTES, 1, false),
    ] {
        let mut budget = ConstructionBudget::new(ceiling);
        budget.charge(CARRIED, 1, 1).unwrap();
        let result = read_mirror(&mut client, &mut budget, member(bytes), cap).await;
        assert_eq!(result.is_ok(), accepted, "{ceiling} / {bytes} / {cap}");
    }
    client
        .batch_execute("UPDATE mb_artist SET sort_name = repeat('é', 512)")
        .await
        .unwrap();
    let mut budget = ConstructionBudget::new(16 * 1024);
    let (artists, _, _) = read_mirror(&mut client, &mut budget, member(TEXT_BYTES + 1_023), 2)
        .await
        .unwrap();
    assert_eq!(artists[0].sort_name.len(), 1_024);
    client
        .batch_execute("UPDATE mb_artist SET sort_name = sort_name || 'x'")
        .await
        .unwrap();
    let mut budget = ConstructionBudget::new(16 * 1024);
    assert!(
        read_mirror(&mut client, &mut budget, member(TEXT_BYTES + 2_048), 2)
            .await
            .is_err()
    );
}
