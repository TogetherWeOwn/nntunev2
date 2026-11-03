//! CC0 fixture album (Holizna "Busted Guitar (JAZZ)") against the MB
//! match pipeline and the manifest's dedupe keys (tests-only).
//!
//! Contract under test:
//!
//! - `pick_release_mbid` (`workers/ingest/src/matching.rs`) resolves the
//!   query `Holizna - Busted Guitar (JAZZ)` to the manifest release MBID
//!   among decoys, and to `None` when only wrong-artist hits exist.
//! - `parse_mb_recordings_detailed` / `parse_mb_recordings_response`
//!   return the manifest recordings in track order whatever the wire
//!   order, with MBIDs exactly the manifest `recording_mbid`s.
//! - The manifest is self-consistent: MB length vs. decoded duration,
//!   and the dedupe keys (FLAC STREAMINFO MD5, sha256, recording and
//!   track MBIDs) are well-formed and pairwise distinct.
//! - Real bytes (skipped without R2 read access): track 1 matches the
//!   manifest bytes, sha256, STREAMINFO format and MD5. The mini
//!   STREAMINFO reader lives in this test only; it verifies the
//!   manifest, it is not production code.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml \
//!     --test cc0_fixture_album
//! ```

use nntune_ingest::matching::{
    parse_mb_recordings_detailed, parse_mb_recordings_response, pick_release_mbid, MbSearchHit,
};
use nntune_test_fixtures::{cc0_track, Fixture};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MANIFEST: &str =
    include_str!("../../../fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593/manifest.json");

const QUERY: &str = "Holizna - Busted Guitar (JAZZ)";
const DECOY_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const DECOY_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const DECOY_ARTIST: &str = "cccccccc-3333-4333-8333-333333333333";

fn manifest() -> Value {
    serde_json::from_str(MANIFEST).expect("manifest parses")
}

fn tracks(m: &Value) -> Vec<Value> {
    let mut t = m["tracks"].as_array().expect("tracks array").clone();
    t.sort_by_key(|x| x["position"].as_u64().unwrap());
    t
}

fn s(v: &Value) -> String {
    v.as_str().expect("string").to_string()
}

fn hit(
    mbid: &str,
    title: &str,
    score: f32,
    artist: Option<&str>,
    artist_mbid: Option<&str>,
) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: title.to_string(),
        score,
        artist_mbid: artist_mbid.map(str::to_string),
        artist_name: artist.map(str::to_string),
    }
}

#[test]
fn pick_release_mbid_finds_the_manifest_release_among_decoys() {
    let m = manifest();
    let r = &m["release"];
    let correct = hit(
        r["mbid"].as_str().unwrap(),
        r["title"].as_str().unwrap(),
        0.7,
        r["artist"].as_str(),
        r["artist_mbid"].as_str(),
    );
    // Same title, different artist, higher score.
    let wrong_artist = hit(
        DECOY_A,
        r["title"].as_str().unwrap(),
        0.95,
        Some("Someone Else"),
        Some(DECOY_ARTIST),
    );
    // Same artist, different title.
    let wrong_title = hit(
        DECOY_B,
        "Busted Guitar (BLUES)",
        0.9,
        r["artist"].as_str(),
        r["artist_mbid"].as_str(),
    );
    let hits = vec![wrong_artist.clone(), wrong_title.clone(), correct];
    assert_eq!(pick_release_mbid(QUERY, &hits), Some(s(&r["mbid"])));

    let only_wrong = vec![wrong_artist, wrong_title];
    assert_eq!(pick_release_mbid(QUERY, &only_wrong), None);
}

#[test]
fn recordings_parse_in_track_order_from_a_shuffled_body() {
    let m = manifest();
    let ordered = tracks(&m);
    let expected: Vec<String> = ordered.iter().map(|t| s(&t["recording_mbid"])).collect();

    let mut shuffled: Vec<&Value> = ordered.iter().collect();
    shuffled.reverse();
    shuffled.swap(0, 2);
    let recordings: Vec<Value> = shuffled
        .iter()
        .map(|t| {
            json!({
                "mbid": t["recording_mbid"],
                "title": t["title"],
                "track_no": t["position"],
                "medium_no": 1,
            })
        })
        .collect();
    let body = json!({ "recordings": recordings }).to_string();
    assert_ne!(
        shuffled
            .iter()
            .map(|t| s(&t["recording_mbid"]))
            .collect::<Vec<_>>(),
        expected,
        "wire order must differ from track order"
    );

    let detailed = parse_mb_recordings_detailed(200, &body).expect("200 parses");
    assert_eq!(
        detailed.iter().map(|r| r.mbid.clone()).collect::<Vec<_>>(),
        expected
    );
    for (rec, t) in detailed.iter().zip(&ordered) {
        assert_eq!(rec.title, s(&t["title"]));
        assert_eq!(rec.track_no, Some(t["position"].as_i64().unwrap() as i16));
        assert_eq!(rec.medium_no, Some(1));
    }

    assert_eq!(parse_mb_recordings_response(200, &body), Ok(expected));
}

#[test]
fn manifest_mb_length_matches_decoded_duration() {
    for t in tracks(&manifest()) {
        let mb = t["mb_length_ms"].as_i64().unwrap();
        let dur = (t["duration_s"].as_f64().unwrap() * 1000.0).round() as i64;
        assert!(
            (mb - dur).abs() <= 23,
            "track {}: mb_length_ms {mb} vs duration {dur}",
            t["position"]
        );
    }
}

fn all_distinct(values: &[String]) -> bool {
    let set: std::collections::HashSet<&String> = values.iter().collect();
    set.len() == values.len()
}

fn is_lower_hex(v: &str, len: usize) -> bool {
    v.len() == len
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Canonical MusicBrainz UUID shape: lowercase hex with hyphens at 8/13/18/23.
fn is_mbid(v: &str) -> bool {
    v.len() == 36
        && v.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

#[test]
fn manifest_dedupe_keys_are_well_formed_and_distinct() {
    let m = manifest();
    let ts = tracks(&m);
    assert_eq!(ts.len(), 4);
    let positions: Vec<u64> = ts.iter().map(|t| t["position"].as_u64().unwrap()).collect();
    assert_eq!(positions, vec![1, 2, 3, 4]);

    let col = |k: &str| -> Vec<String> { ts.iter().map(|t| s(&t[k])).collect() };
    let md5s = col("flac_streaminfo_md5");
    let shas = col("sha256");
    assert!(md5s.iter().all(|v| is_lower_hex(v, 32)), "{md5s:?}");
    assert!(shas.iter().all(|v| is_lower_hex(v, 64)), "{shas:?}");
    assert!(all_distinct(&md5s));
    assert!(all_distinct(&shas));
    for key in ["recording_mbid", "track_mbid"] {
        let values = col(key);
        assert!(values.iter().all(|v| is_mbid(v)), "{key}: {values:?}");
        assert!(all_distinct(&values), "{key}");
    }

    for t in &ts {
        assert_eq!(t["sample_rate"], 48000);
        assert_eq!(t["bits_per_sample"], 24);
        assert_eq!(t["channels"], 2);
    }
}

#[test]
fn real_track_one_bytes_match_the_manifest() {
    let path = match cc0_track(1).expect("fetcher runs") {
        Fixture::Ready(p) => p,
        Fixture::Skipped(why) => {
            eprintln!("skipping real-bytes check: {why}");
            return;
        }
    };
    let m = manifest();
    let t = &tracks(&m)[0];
    let bytes = std::fs::read(&path).expect("read fixture");
    assert_eq!(bytes.len() as u64, t["bytes"].as_u64().unwrap());
    assert_eq!(hex::encode(Sha256::digest(&bytes)), s(&t["sha256"]));

    assert_eq!(&bytes[0..4], b"fLaC");
    // STREAMINFO body starts at 8 (after "fLaC" + 4-byte block header);
    // min/max block size (4) and min/max frame size (6) precede the
    // packed 64-bit word at 18.
    let v = u64::from_be_bytes(bytes[18..26].try_into().unwrap());
    assert_eq!(v >> 44, 48000);
    assert_eq!(((v >> 41) & 0x7) + 1, 2);
    assert_eq!(((v >> 36) & 0x1f) + 1, 24);
    let total_samples = v & ((1u64 << 36) - 1);
    assert_eq!(hex::encode(&bytes[26..42]), s(&t["flac_streaminfo_md5"]));

    // 23 ms nominal MB-vs-decoded gap plus duration_s millisecond rounding.
    let ms = (total_samples * 1000 / 48000) as i64;
    let mb = t["mb_length_ms"].as_i64().unwrap();
    assert!((ms - mb).abs() <= 30, "stream {ms} ms vs mb {mb} ms");
}
