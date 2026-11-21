use mb_import_validate::works::{lookup, ReadError, WorkMbid, MAX_ARTIFACT_BYTES};

const GOLDEN: &[u8] =
    include_bytes!("../../../containers/mb-import/tests/expected/mbdump-work.tsv");
const TYPED: &str = "80808080-8080-4808-8808-808080808080";
const UNTYPED: &str = "10101010-1010-4010-8010-101010101010";
const OTHER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

#[test]
fn reads_typed_and_untyped_works_from_etl_golden() {
    for (id, title, work_type) in [
        (TYPED, "A\tSong\\N", Some("Song\tSuite")),
        (UNTYPED, "Café\nLive", None),
    ] {
        let work = lookup(GOLDEN, id).unwrap().unwrap();
        let mbid: WorkMbid = work.mbid;
        assert_eq!(mbid.as_str(), id);
        assert_eq!(work.title, title);
        assert_eq!(work.work_type.as_deref(), work_type);
    }
    assert_eq!(lookup(GOLDEN, OTHER), Ok(None));
    assert_eq!(lookup(b"", OTHER), Ok(None));
}

#[test]
fn canonicalizes_mbid_and_keeps_escaped_null_as_text() {
    let row = b"\\x41AAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA\tCaf\\303\\xA9\t\\\\N\t2025-02-03T04:05:06.123456Z\n";
    let work = lookup(row, &OTHER.to_ascii_uppercase()).unwrap().unwrap();
    assert_eq!(work.mbid.as_str(), OTHER);
    assert_eq!(work.title, "Café");
    assert_eq!(work.work_type.as_deref(), Some(r"\N"));
}

#[test]
fn rejects_invalid_trailing_rows_after_hit_or_miss() {
    let valid = format!("{OTHER}\tTitle\tType\t2025-02-03T04:05:06Z\n");
    for (bad, reason) in [
        (valid.replace(OTHER, "not-a-uuid"), "bad_uuid"),
        (valid.replace("Title", r"\N"), "null_in_not_null"),
        (valid.replace("Title", r"\q"), "bad_escape"),
        (valid.replace("Type", r"\000"), "bad_escape"),
        (valid.replace("Type", r"\xff"), "bad_escape"),
        (valid.replace("06Z", "60Z"), "bad_timestamp"),
        (valid.replace("02-03", "02-30"), "bad_timestamp"),
        (valid.replace("Z\n", "Z\t\n"), "column_count"),
        (valid.replace("\tType", ""), "column_count"),
        (
            valid.replace(OTHER, r"\141AAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"),
            "duplicate_work_mbid",
        ),
    ] {
        let bytes = format!("{valid}{bad}");
        for query in [OTHER, TYPED] {
            assert_eq!(
                lookup(bytes.as_bytes(), query),
                Err(ReadError::InvalidRow { line: 2, reason }),
                "{bad:?}"
            );
        }
    }
}

#[test]
fn rejects_invalid_query_and_artifact_envelope() {
    assert_eq!(lookup(GOLDEN, "bad"), Err(ReadError::InvalidWorkMbid));
    assert_eq!(lookup(b"\xff\n", TYPED), Err(ReadError::InvalidUtf8));
    assert_eq!(
        lookup(&GOLDEN[..GOLDEN.len() - 1], TYPED),
        Err(ReadError::MissingFinalLf)
    );
    assert_eq!(
        lookup(&vec![b'x'; MAX_ARTIFACT_BYTES + 1], TYPED),
        Err(ReadError::TooLarge)
    );
}
