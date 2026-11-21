use mb_import_validate::release_credits::{lookup, ReadError, ReleaseCredit, MAX_ARTIFACT_BYTES};

const ARTIFACT: &[u8] =
    include_bytes!("../../../containers/mb-import/tests/expected/mbdump-release_credit.tsv");
const RELEASE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const OTHER: &str = "55555555-5555-4555-8555-555555555555";
const ABSENT: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

#[test]
fn reads_ordered_credits_from_curated_artifact() {
    let text = std::str::from_utf8(ARTIFACT).unwrap();
    let reversed: String = text.split_inclusive('\n').rev().collect();
    let expected = vec![
        ReleaseCredit {
            position: 0,
            artist_mbid: "11111111-1111-4111-8111-111111111111".into(),
            credited_name: "First Artist".into(),
            join_phrase: " &".into(),
        },
        ReleaseCredit {
            position: 1,
            artist_mbid: "22222222-2222-4222-8222-222222222222".into(),
            credited_name: "Second Artist".into(),
            join_phrase: " with".into(),
        },
    ];
    assert_eq!(lookup(ARTIFACT, RELEASE), Ok(expected));
    assert_eq!(
        lookup(reversed.as_bytes(), &RELEASE.to_ascii_uppercase()),
        lookup(ARTIFACT, RELEASE)
    );
    assert_eq!(lookup(ARTIFACT, ABSENT), Ok(vec![]));
}

#[test]
fn decodes_utf8_escapes_without_losing_empty_fields() {
    let bytes = std::str::from_utf8(ARTIFACT)
        .unwrap()
        .replacen(
            "First Artist\t &",
            concat!(r"Björk\303\251\t\\N", "\t", r" \x26"),
            1,
        )
        .replacen("Second Artist\t with", "\t", 1)
        .replacen("2025-02-03T04:05:06Z", r"\062025-02-03T04:05:06.123456Z", 1);
    let credits = lookup(bytes.as_bytes(), OTHER).unwrap();
    assert_eq!(credits.len(), 2);
    assert_eq!(
        (&*credits[0].credited_name, &*credits[0].join_phrase),
        ("Björké\t\\N", " &")
    );
    assert_eq!(
        (&*credits[1].credited_name, &*credits[1].join_phrase),
        ("", "")
    );
}

#[test]
fn refuses_invalid_artifacts_even_for_no_match() {
    let text = std::str::from_utf8(ARTIFACT).unwrap();
    for (token, expected) in [
        ("-1", "invalid_credit_position"),
        ("2147483648", "invalid_credit_position"),
        ("not-a-position", "invalid_credit_position"),
        ("", "invalid_credit_position"),
        (r"\060", "duplicate_credit_position"),
    ] {
        let changed = text.replacen("\t1\t", &format!("\t{token}\t"), 1);
        assert_eq!(
            lookup(changed.as_bytes(), ABSENT),
            Err(ReadError::InvalidRow {
                line: 2,
                reason: expected
            })
        );
    }
    for timestamp in ["2025-+2-03T04:05:06Z", "2025-02-03T04:05:+6Z"] {
        let changed = text.replacen("2025-02-03T04:05:06Z", timestamp, 1);
        assert_eq!(
            lookup(changed.as_bytes(), ABSENT),
            Err(ReadError::InvalidRow {
                line: 1,
                reason: "bad_timestamp"
            })
        );
    }
    assert_eq!(
        lookup(&ARTIFACT[..ARTIFACT.len() - 1], RELEASE),
        Err(ReadError::MissingFinalLf)
    );
    assert_eq!(
        lookup(&vec![0; MAX_ARTIFACT_BYTES + 1], RELEASE),
        Err(ReadError::TooLarge)
    );
}
