use mb_import_validate::relationships::{
    lookup, Projection::*, ReadError, Relationship, TargetMbid, MAX_ARTIFACT_BYTES,
};

const SONG: &[u8] =
    include_bytes!("../../../containers/mb-import/tests/expected/mbdump-recording_recording.tsv");
const VIDEO: &[u8] =
    include_bytes!("../../../containers/mb-import/tests/expected/mbdump-recording_music_video.tsv");
const R1: &str = "88888888-8888-4888-8888-888888888888";
const R2: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const ARTIST: &str = "11111111-1111-4111-8111-111111111111";
const RELEASE: &str = "55555555-5555-4555-8555-555555555555";
const WORK: &str = "80808080-8080-4808-8808-808080808080";
const TS: &str = "2025-02-03T04:05:06Z";

fn edge(target: TargetMbid, link_type: &str) -> Relationship {
    Relationship {
        target,
        link_type: link_type.into(),
    }
}

#[test]
fn reads_directed_song_golden_but_refuses_video_projection() {
    for (source, target) in [(R1.to_owned(), R2), (R2.to_ascii_uppercase(), R1)] {
        assert_eq!(
            lookup(RecordingRecording, SONG, &source),
            Ok(vec![edge(TargetMbid::Recording(target.into()), "cover")])
        );
    }
    let first_row = SONG.split_inclusive(|&b| b == b'\n').next().unwrap();
    assert_eq!(lookup(RecordingRecording, first_row, R2), Ok(vec![]));
    assert_eq!(
        lookup(RecordingRecording, VIDEO, R1),
        Err(ReadError::InvalidRow {
            line: 1,
            reason: "column_count"
        })
    );
}

#[test]
fn reads_typed_targets_and_orders_decoded_links() {
    // These goldens are also checked byte-for-byte by the ETL tests.
    const ARTIST_RECORDING: &[u8] =
        include_bytes!("../../../containers/mb-import/tests/expected/mbdump-artist_recording.tsv");
    const ARTIST_RELEASE: &[u8] =
        include_bytes!("../../../containers/mb-import/tests/expected/mbdump-artist_release.tsv");
    const ARTIST_WORK: &[u8] =
        include_bytes!("../../../containers/mb-import/tests/expected/mbdump-artist_work.tsv");
    const RECORDING_WORK: &[u8] =
        include_bytes!("../../../containers/mb-import/tests/expected/mbdump-recording_work.tsv");
    let other = "10101010-1010-4010-8010-101010101010";
    let other_release = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    assert_eq!(
        lookup(ArtistRecording, ARTIST_RECORDING, ARTIST),
        Ok(vec![
            edge(TargetMbid::Recording(R1.into()), "arrangement"),
            edge(TargetMbid::Recording(R1.into()), "credit"),
            edge(TargetMbid::Recording(R2.into()), "credit"),
        ])
    );
    assert_eq!(
        lookup(ArtistRelease, ARTIST_RELEASE, ARTIST),
        Ok(vec![
            edge(TargetMbid::Release(RELEASE.into()), "arrangement"),
            edge(TargetMbid::Release(RELEASE.into()), "credit"),
            edge(TargetMbid::Release(other_release.into()), "credit"),
        ])
    );
    assert_eq!(
        lookup(ArtistWork, ARTIST_WORK, ARTIST),
        Ok(vec![
            edge(TargetMbid::Work(other.into()), "credit"),
            edge(TargetMbid::Work(WORK.into()), "arrangement"),
            edge(TargetMbid::Work(WORK.into()), "credit"),
        ])
    );
    assert_eq!(
        lookup(RecordingWork, RECORDING_WORK, R1),
        Ok(vec![
            edge(TargetMbid::Work(other.into()), "performance"),
            edge(TargetMbid::Work(WORK.into()), "medley"),
            edge(TargetMbid::Work(WORK.into()), "performance"),
        ])
    );

    let rows = format!(
        "{ARTIST}\t{WORK}\t\\143redit\t{TS}\n{ARTIST}\t{WORK}\tB\\303\\251\\t\\\\N\\034\t{TS}\n{ARTIST}\t{other}\tcredit\t{TS}\n"
    );
    let expected = Ok(vec![
        edge(TargetMbid::Work(other.into()), "credit"),
        edge(TargetMbid::Work(WORK.into()), "Bé\t\\N\u{1c}"),
        edge(TargetMbid::Work(WORK.into()), "credit"),
    ]);
    assert_eq!(lookup(ArtistWork, rows.as_bytes(), ARTIST), expected);
    let reversed: String = rows.split_inclusive('\n').rev().collect();
    assert_eq!(lookup(ArtistWork, reversed.as_bytes(), ARTIST), expected);
}

#[test]
fn validates_unmatched_rows_and_artifact_bounds() {
    let text = std::str::from_utf8(SONG).unwrap();
    let duplicate = format!("{text}\\070{}\t{R2}\t\\x63over\t{TS}\n", &R1[1..]);
    assert_eq!(
        lookup(RecordingRecording, duplicate.as_bytes(), ARTIST),
        Err(ReadError::InvalidRow {
            line: 3,
            reason: "duplicate_relationship"
        })
    );
    for (from, to, reason) in [
        (R1, "bad-key", "bad_uuid"),
        (
            "DDDDDDDD-DDDD-4DDD-8DDD-DDDDDDDDDDDD",
            r"\N",
            "null_in_not_null",
        ),
        ("cover", r"\q", "bad_escape"),
        ("cover", r"\034", "invalid_link_type"),
        ("cover", r"\035\036\037\t", "invalid_link_type"),
        (TS, "2025-02-03T04:05:60Z", "bad_timestamp"),
    ] {
        let invalid = text.replacen(from, to, 1);
        assert_eq!(
            lookup(RecordingRecording, invalid.as_bytes(), ARTIST),
            Err(ReadError::InvalidRow { line: 1, reason })
        );
    }
    for (bytes, source, error) in [
        (SONG, "bad-key", ReadError::InvalidSourceMbid),
        (&SONG[..SONG.len() - 1], R1, ReadError::MissingFinalLf),
        (&[255, b'\n'], R1, ReadError::InvalidUtf8),
        (&vec![0; MAX_ARTIFACT_BYTES + 1], R1, ReadError::TooLarge),
    ] {
        assert_eq!(lookup(RecordingRecording, bytes, source), Err(error));
    }
    assert_eq!(lookup(RecordingRecording, b"", R1), Ok(vec![]));
}
