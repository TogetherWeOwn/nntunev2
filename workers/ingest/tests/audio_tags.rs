//! Bounded tag-reader component; no network, art staging or catalog authority.
use nntune_ingest::{
    audio_manifest::AudioHints,
    audio_tags::{read_flac_hints, TagReadError, MAX_TAG_PREFIX_BYTES},
};

fn block(kind: u8, last: bool, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind | if last { 128 } else { 0 }];
    out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    out
}

fn streaminfo() -> Vec<u8> {
    let mut info = vec![0; 34];
    let samples = (44_100_u64 << 44) | (88_200_u64);
    info[10..18].copy_from_slice(&samples.to_be_bytes());
    block(0, false, &info)
}

fn comments(fields: &[&str]) -> Vec<u8> {
    let mut body = vec![0; 4]; // empty vendor
    body.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    for field in fields {
        body.extend_from_slice(&(field.len() as u32).to_le_bytes());
        body.extend_from_slice(field.as_bytes());
    }
    block(4, true, &body)
}

fn flac(fields: &[&str]) -> Vec<u8> {
    let mut out = b"fLaC".to_vec();
    out.extend(streaminfo());
    out.extend(comments(fields));
    out
}

#[test]
fn extracts_untrusted_flac_hints_without_turning_claims_into_matches() {
    let bytes = flac(&[
        "title= A title ",
        "ALBUM=An album",
        "ARTIST=Track artist",
        "ALBUMARTIST=Album artist",
        "DISCNUMBER=2/3",
        "TRACKNUMBER=7/12",
        "MUSICBRAINZ_ARTISTID=11111111-1111-1111-1111-111111111111",
        "MUSICBRAINZ_ALBUMARTISTID=22222222-2222-2222-2222-222222222222",
        "MUSICBRAINZ_ALBUMID=33333333-3333-3333-3333-333333333333",
        "MUSICBRAINZ_TRACKID=44444444-4444-4444-4444-444444444444",
        "FINGERPRINT=untrusted arbitrary text",
    ]);
    let hints = read_flac_hints(&bytes).unwrap();
    assert_eq!(hints.title.as_deref(), Some("A title"));
    assert_eq!(hints.album.as_deref(), Some("An album"));
    assert_eq!(hints.track_artist.as_deref(), Some("Track artist"));
    assert_eq!(hints.album_artist.as_deref(), Some("Album artist"));
    assert_eq!(hints.disc_no, Some(2));
    assert_eq!(hints.track_no, Some(7));
    assert_eq!(hints.duration_ms, Some(2000));
    assert!(hints.artist_mbid.is_some());
    assert!(hints.album_artist_mbid.is_some());
    assert!(hints.release_mbid.is_some());
    assert!(hints.recording_mbid.is_some());
    assert!(
        hints.fingerprint.is_none(),
        "a comment is not a computed fingerprint"
    );
}

#[test]
fn duplicate_claims_are_unknown_not_first_or_last_wins() {
    let hints = read_flac_hints(&flac(&[
        "TITLE=first",
        "title=second",
        "ARTIST=one",
        "ARTIST=two",
        "DISCNUMBER=1",
        "discnumber=bad",
        "TRACKNUMBER=2",
        "TRACKNUMBER=2",
    ]))
    .unwrap();
    assert!(hints.title.is_none());
    assert!(hints.track_artist.is_none());
    assert_eq!(hints.disc_no, Some(0), "invalid disc presence is retained");
    assert!(hints.track_no.is_none());
    assert_eq!(hints.duration_ms, Some(2000));
}

#[test]
fn invalid_claims_cannot_escape_into_the_manifest() {
    let hints = read_flac_hints(&flac(&[
        "TITLE=control\ntext",
        "ALBUM=   ",
        "TRACKNUMBER=-1",
        "DISCNUMBER=0",
        "MUSICBRAINZ_ALBUMID=not-a-mbid",
        "MUSICBRAINZ_TRACKID=https://example.invalid",
    ]))
    .unwrap();
    assert!(hints.title.is_none());
    assert!(hints.album.is_none());
    assert!(hints.track_no.is_none());
    assert_eq!(hints.disc_no, Some(0), "invalid disc presence is retained");
    assert!(hints.release_mbid.is_none());
    assert!(hints.recording_mbid.is_none());
    for raw in ["1/0", "1/", "1/2/3", "2147483648", "1x"] {
        let hints = read_flac_hints(&flac(&[&format!("TRACKNUMBER={raw}")])).unwrap();
        assert!(hints.track_no.is_none(), "{raw}");
    }
    let long = format!("TITLE={}", "x".repeat(4097));
    assert!(read_flac_hints(&flac(&[&long])).unwrap().title.is_none());
}

#[test]
fn every_truncated_metadata_prefix_is_rejected_without_partial_hints() {
    let bytes = flac(&["TITLE=complete only"]);
    for len in 0..bytes.len() {
        assert!(read_flac_hints(&bytes[..len]).is_err(), "prefix {len}");
    }
    assert_eq!(
        read_flac_hints(&bytes).unwrap().title.as_deref(),
        Some("complete only")
    );
}

#[test]
fn malformed_lengths_utf8_and_duplicate_comment_blocks_are_rejected() {
    let mut bytes = flac(&["TITLE=test"]);
    let start = 4 + 4 + 34 + 4;
    bytes[start..start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        read_flac_hints(&bytes).err().unwrap(),
        TagReadError::Malformed
    );
    let mut bytes = flac(&["TITLE=test"]);
    *bytes.last_mut().unwrap() = 255;
    assert_eq!(
        read_flac_hints(&bytes).err().unwrap(),
        TagReadError::Malformed
    );
    let mut bytes = flac(&["TITLE=one"]);
    bytes[4 + 4 + 34] &= 127;
    bytes.extend(comments(&["TITLE=two"]));
    assert_eq!(
        read_flac_hints(&bytes).err().unwrap(),
        TagReadError::Malformed
    );
    let mut bytes = flac(&[]);
    bytes.extend_from_slice(b"audio frames are not metadata");
    assert_eq!(read_flac_hints(&bytes).unwrap().duration_ms, Some(2000));
}

#[test]
fn enforces_prefix_budget_and_does_not_treat_other_formats_as_flac() {
    assert_eq!(
        read_flac_hints(&vec![0; MAX_TAG_PREFIX_BYTES + 1])
            .err()
            .unwrap(),
        TagReadError::TooLarge
    );
    assert_eq!(
        read_flac_hints(b"RIFFnot-flac").err().unwrap(),
        TagReadError::Unsupported
    );
    let mut bytes = b"fLaC".to_vec();
    bytes.extend(block(4, true, &[0; 8]));
    assert_eq!(
        read_flac_hints(&bytes).err().unwrap(),
        TagReadError::Malformed
    );
}

#[test]
fn picture_blocks_are_not_claimed_as_validated_or_staged_art() {
    let mut bytes = b"fLaC".to_vec();
    bytes.extend(streaminfo());
    bytes.extend(block(6, false, b"not a validated picture"));
    bytes.extend(comments(&[]));
    let hints = read_flac_hints(&bytes).unwrap();
    let expected = AudioHints {
        duration_ms: Some(2000),
        ..AudioHints::default()
    };
    assert_eq!(
        serde_json::to_value(hints).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}
