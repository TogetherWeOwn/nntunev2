//! Generated-byte preparation for mixed-archive consumers. These exercise
//! the existing portable route's cache publication boundary only; they do
//! not qualify the Worker cold path, an archive adapter, or provider durability.

use nntune_stream::cache::ContentCache;
use nntune_stream::delivered::VecDeliveredSink;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::StreamPool;
use nntune_stream::play_route::{
    serve_play, MemoryManifestStore, PlayRouteOutcome, PlayRouteRequest, PlayRouteServe,
};
use nntune_stream::pool::Pool;

const SECRET: &[u8] = b"synthetic-cache-publication-secret";
const NOW: u64 = 1_700_000_000;
const LEN: usize = 48 * 1024;

fn generated_bytes(seed: usize) -> Vec<u8> {
    (0..LEN).map(|i| ((i * 31 + seed) % 251) as u8).collect()
}

fn begin(
    cache: &mut ContentCache,
    pool: &mut StreamPool,
    meter: &mut MemoryMeter,
    track: &str,
    play: &str,
    range: Option<&str>,
) -> PlayRouteServe {
    let request = PlayRouteRequest {
        track_id: track.into(),
        token: Some(grant::issue_v2(
            "listener",
            track,
            play,
            1,
            NOW + 60,
            SECRET,
        )),
        range: range.map(str::to_owned),
        trace_id: None,
    };
    // Zero pool capacity: an accidental miss cannot dial a real or fake provider.
    match serve_play(
        pool,
        cache,
        meter,
        SECRET,
        &MemoryManifestStore::default(),
        "192.0.2.1:563".parse().unwrap(),
        NOW,
        &request,
    ) {
        PlayRouteOutcome::Serve(response) => response,
        PlayRouteOutcome::Refuse(refusal) => panic!("cache serve refused: {refusal:?}"),
    }
}

#[test]
fn republishing_a_track_does_not_splice_an_inflight_range() {
    let old = generated_bytes(7);
    let new = generated_bytes(19);
    let mut cache = ContentCache::with_max_bytes(2 * LEN);
    let mut pool = Pool::new(0);
    let mut meter = MemoryMeter::default();
    let mut sink = VecDeliveredSink::default();
    cache.put_track_at("track-a", old.clone(), NOW);

    let mut prior = begin(
        &mut cache,
        &mut pool,
        &mut meter,
        "track-a",
        "play-old",
        Some("bytes=100-40000"),
    );
    assert_eq!(prior.status, 206);
    assert_eq!(prior.body_len, 39_901);
    let mut served = prior.play.next().expect("first range chunk");
    assert!(served.len() < prior.body_len as usize);

    cache.put_track_at("track-a", new.clone(), NOW);
    served.extend(prior.play.by_ref().flatten());
    assert_eq!(served, old[100..40_001]);
    prior
        .play
        .finish(&mut pool, &mut cache, &mut meter, &mut sink)
        .unwrap();

    let mut current = begin(
        &mut cache,
        &mut pool,
        &mut meter,
        "track-a",
        "play-new",
        Some("bytes=100-40000"),
    );
    assert_eq!(current.status, 206);
    assert_eq!(
        current.play.by_ref().flatten().collect::<Vec<_>>(),
        new[100..40_001]
    );
    current
        .play
        .finish(&mut pool, &mut cache, &mut meter, &mut sink)
        .unwrap();
    assert_eq!(cache.get_track("track-a").unwrap().as_ref(), new);
    assert_eq!(cache.total_bytes(), LEN);
    assert_eq!(sink.0.len(), 2);
    for (interval, play) in sink.0.iter().zip(["play-old", "play-new"]) {
        assert_eq!(interval.play_id, play);
        assert_eq!((interval.start, interval.end), (100, 40_001));
    }
}

#[test]
fn republishing_one_shared_blob_pointer_preserves_the_other_track() {
    let original = generated_bytes(7);
    let replacement = generated_bytes(19);
    let mut cache = ContentCache::with_max_bytes(2 * LEN);
    let mut pool = Pool::new(0);
    let mut meter = MemoryMeter::default();
    let mut sink = VecDeliveredSink::default();
    cache.put_track_at("track-a", original.clone(), NOW);
    cache.put_track_at("track-b", original.clone(), NOW);
    assert_eq!(cache.total_bytes(), LEN);
    cache.put_track_at("track-a", replacement.clone(), NOW);
    assert_eq!(cache.total_bytes(), 2 * LEN);

    for (track, expected) in [("track-a", replacement), ("track-b", original)] {
        let mut response = begin(
            &mut cache,
            &mut pool,
            &mut meter,
            track,
            track,
            Some("bytes=-4097"),
        );
        assert_eq!(response.status, 206);
        assert_eq!(response.body_len, 4097);
        assert_eq!(
            response.play.by_ref().flatten().collect::<Vec<_>>(),
            expected[LEN - 4097..]
        );
        response
            .play
            .finish(&mut pool, &mut cache, &mut meter, &mut sink)
            .unwrap();
    }
    assert_eq!(sink.0.len(), 2);
    for (interval, track) in sink.0.iter().zip(["track-a", "track-b"]) {
        assert_eq!(interval.track_id, track);
        assert_eq!(
            (interval.start, interval.end),
            ((LEN - 4097) as u64, LEN as u64)
        );
    }
}
