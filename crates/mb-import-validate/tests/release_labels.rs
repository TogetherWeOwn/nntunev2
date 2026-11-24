use mb_import_validate::reason;
use mb_import_validate::release_labels::{lookup, Artifact, Label, ReadError, MAX_ARTIFACT_BYTES};
use std::sync::Arc;

const LABELS: &[u8] =
    include_bytes!("../../../containers/mb-import/tests/expected/mbdump-label.tsv");
const EDGES: &[u8] =
    include_bytes!("../../../containers/mb-import/tests/expected/mbdump-release_label.tsv");
const FIRST: &str = "55555555-5555-4555-8555-555555555555";
const SECOND: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const QUARTZ: &str = "71717171-7171-4717-8717-717171717171";
const UNTYPED: &str = "62626262-6262-4626-8626-626262626262";
const MISSING: &str = "71717171-7171-4717-8717-717171717172";
const TS: &str = "2025-02-03T04:05:06Z";

#[test]
fn joins_shared_etl_goldens_in_decoded_order() {
    let boundary = format!("{}\t", "é".repeat(254));
    let results = lookup(LABELS, EDGES, FIRST).unwrap();
    assert_eq!(
        results
            .iter()
            .map(|m| m.catalogue_number.as_str())
            .collect::<Vec<_>>(),
        ["Café\tDisc\\N", "", " QZ-1 ", " qz-1 ", r"\N", &boundary]
    );
    assert_eq!(
        *results[0].label,
        Label {
            mbid: UNTYPED.into(),
            name: "Untyped Label".into(),
            code: None,
            type_name: None,
        }
    );
    assert_eq!(
        *results[1].label,
        Label {
            mbid: QUARTZ.into(),
            name: "Quartz\tRecords".into(),
            code: Some(12345),
            type_name: Some(boundary),
        }
    );
    assert!(results[2..]
        .iter()
        .all(|edge| Arc::ptr_eq(&results[1].label, &edge.label)));

    let reversed = |bytes: &[u8]| {
        let rows: Vec<_> = bytes.split_inclusive(|&b| b == b'\n').collect();
        rows.into_iter()
            .rev()
            .flatten()
            .copied()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lookup(&reversed(LABELS), &reversed(EDGES), FIRST).unwrap(),
        results
    );
    let second = lookup(LABELS, EDGES, &SECOND.to_uppercase()).unwrap();
    assert_eq!(second.as_slice(), &results[2..3]);
    assert!(lookup(LABELS, EDGES, MISSING).unwrap().is_empty());
    assert!(lookup(LABELS, b"", FIRST).unwrap().is_empty());
}

#[test]
fn decodes_edge_identity_and_keeps_empty_type_distinct_from_null() {
    let labels = format!("{QUARTZ}\tLabel\t-2147483648\t\t{TS}\n");
    let edges = format!("{SECOND}\t\\x371717171-7171-4717-8717-717171717171\t\t{TS}\n");
    let result = lookup(labels.as_bytes(), edges.as_bytes(), SECOND).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].label.mbid, QUARTZ);
    assert_eq!(result[0].label.code, Some(i32::MIN));
    assert_eq!(result[0].label.type_name.as_deref(), Some(""));
    assert_eq!(result[0].catalogue_number, "");
}

#[test]
fn refuses_invalid_label_rows_even_without_edges() {
    for (mbid, name, code, kind, ts, why) in [
        (
            r"\x371717171-7171-4717-8717-717171717171",
            "Duplicate",
            "0",
            "",
            TS,
            "duplicate_label_mbid",
        ),
        ("bad", "Label", "0", "", TS, reason::BAD_UUID),
        (MISSING, r"\N", "0", "", TS, reason::NULL_IN_NOT_NULL),
        (MISSING, "Label", "", "", TS, reason::BAD_INT),
        (MISSING, "Label", "2147483648", "", TS, reason::BAD_INT),
        (MISSING, "Label", "0", r"\377", TS, reason::BAD_ESCAPE),
        (
            MISSING,
            "Label",
            "0",
            "",
            "2025-02-30T04:05:06Z",
            reason::BAD_TIMESTAMP,
        ),
    ] {
        let bytes = [
            LABELS,
            format!("{mbid}\t{name}\t{code}\t{kind}\t{ts}\n").as_bytes(),
        ]
        .concat();
        assert_eq!(
            lookup(&bytes, b"", MISSING),
            Err(ReadError::InvalidRow(Artifact::Labels, 3, why))
        );
    }
}

#[test]
fn refuses_invalid_edges_after_a_hit() {
    let too_long = "é".repeat(256);
    let upper_second = SECOND.to_uppercase();
    for (release, label, number, why) in [
        (SECOND, MISSING, "OTHER", "missing_label_mbid"),
        (
            upper_second.as_str(),
            QUARTZ,
            r" QZ-\061 ",
            "duplicate_release_label_key",
        ),
        (SECOND, QUARTZ, r"\N", reason::NULL_IN_NOT_NULL),
        (SECOND, QUARTZ, r"\q", reason::BAD_ESCAPE),
        (SECOND, QUARTZ, too_long.as_str(), "text_too_long"),
    ] {
        let bytes = [
            EDGES,
            format!("{release}\t{label}\t{number}\t{TS}\n").as_bytes(),
        ]
        .concat();
        assert_eq!(
            lookup(LABELS, &bytes, FIRST),
            Err(ReadError::InvalidRow(Artifact::ReleaseLabels, 8, why))
        );
    }
    assert_eq!(
        lookup(LABELS, &[EDGES, b"truncated\n"].concat(), FIRST),
        Err(ReadError::InvalidRow(
            Artifact::ReleaseLabels,
            8,
            reason::COLUMN_COUNT
        ))
    );
}

#[test]
fn rejects_invalid_query_and_artifact_envelopes() {
    assert_eq!(
        lookup(LABELS, EDGES, "not-an-mbid"),
        Err(ReadError::InvalidReleaseMbid)
    );
    let oversized = vec![0; MAX_ARTIFACT_BYTES + 1];
    for (bytes, error) in [
        (
            oversized.as_slice(),
            ReadError::TooLarge as fn(Artifact) -> ReadError,
        ),
        (b"\xff\n", ReadError::InvalidUtf8),
        (b"unterminated", ReadError::MissingFinalLf),
    ] {
        assert_eq!(lookup(bytes, EDGES, FIRST), Err(error(Artifact::Labels)));
        assert_eq!(
            lookup(LABELS, bytes, FIRST),
            Err(error(Artifact::ReleaseLabels))
        );
    }
}
