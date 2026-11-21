use mb_import_validate::aliases::{lookup, EntityKind, ReadError, MAX_ARTIFACT_BYTES};

macro_rules! golden {
    ($entity:literal) => {
        include_bytes!(concat!(
            "../../../containers/mb-import/tests/expected/mbdump-",
            $entity,
            "_alias.tsv"
        ))
    };
}

#[test]
fn reads_all_six_golden_sidecars_as_literal_text() {
    use EntityKind::*;
    for (kind, bytes, query, count, primary_sort) in [
        (
            Artist,
            golden!("artist").as_slice(),
            "Café",
            4,
            "Jazz\tQuartet",
        ),
        (
            Label,
            golden!("label").as_slice(),
            "Étiquette",
            2,
            "Quartz\tRecords",
        ),
        (
            Recording,
            golden!("recording").as_slice(),
            "Écho",
            4,
            "Echo\tMix",
        ),
        (
            Release,
            golden!("release").as_slice(),
            "Édition",
            4,
            "Edition\tBox",
        ),
        (
            ReleaseGroup,
            golden!("release_group").as_slice(),
            "Écho",
            3,
            "Echo\tGroup",
        ),
        (Work, golden!("work").as_slice(), "Étude", 2, "Study\tOne"),
    ] {
        let hits = lookup(bytes, kind, query).unwrap();
        assert_eq!(hits.len(), count);
        assert!(hits
            .iter()
            .all(|hit| hit.entity.kind == kind && hit.credited_name == query));
        assert!(hits.iter().any(|hit| hit.locale.as_deref() == Some("en")
            && hit.sort_name == primary_sort
            && hit.primary_for_locale));
        assert!(lookup(bytes, kind, &query.to_lowercase())
            .unwrap()
            .is_empty());
    }
}

#[test]
fn preserves_distinct_keys_in_deterministic_order() {
    let bytes = golden!("artist");
    let hits = lookup(bytes, EntityKind::Artist, "Café").unwrap();
    let keys: Vec<_> = hits
        .iter()
        .map(|hit| (hit.entity.mbid.as_str(), hit.locale.as_deref()))
        .collect();
    let first = "11111111-1111-4111-8111-111111111111";
    let second = "22222222-2222-4222-8222-222222222222";
    assert_eq!(
        keys,
        [
            (first, None),
            (first, Some("en")),
            (first, Some("fr")),
            (second, Some("en"))
        ]
    );
    assert!(!hits[0].primary_for_locale);
    let reversed: String = std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .rev()
        .map(|row| format!("{row}\n"))
        .collect();
    assert_eq!(
        lookup(reversed.as_bytes(), EntityKind::Artist, "Café").unwrap(),
        hits
    );
    assert!(lookup(bytes, EntityKind::Artist, "Cafe\u{301}")
        .unwrap()
        .is_empty());

    let row = b"AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA\tA\\tB\t\\\\N\tSort\tf\t2025-02-03T04:05:06.123456Z\n";
    let hits = lookup(row, EntityKind::Release, "A\tB").unwrap();
    assert_eq!(hits[0].entity.mbid, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    assert_eq!(hits[0].locale.as_deref(), Some("\\N"));
    assert!(lookup(row, EntityKind::Release, r"A\tB")
        .unwrap()
        .is_empty());
}

#[test]
fn rejects_bad_rows_even_when_they_do_not_match() {
    let mbid = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let stamp = "2025-02-03T04:05:06Z";
    let valid = format!("{mbid}\tCafé\ten\tSort\tf\t{stamp}\n");
    for bad in [
        format!(
            "{}\tCaf\\303\\xA9\t\\145n\tOther\tt\t{stamp}\n",
            mbid.to_uppercase()
        ),
        valid.replace(mbid, "not-a-uuid"),
        valid.replace("en", r"\N"),
        valid.replace("Café", r"\000"),
        valid.replace("Sort", r"\N"),
        valid.replace("\tf\t", "\ttrue\t"),
        valid.replace(stamp, "2025-02-30T04:05:06Z"),
        valid.replace("\tSort", ""),
    ] {
        let input = format!("{valid}{bad}");
        assert!(
            matches!(
                lookup(input.as_bytes(), EntityKind::Label, "missing"),
                Err(ReadError::InvalidRow { line: 2, .. })
            ),
            "accepted {bad:?}"
        );
    }
}

#[test]
fn distinguishes_no_match_from_query_and_artifact_errors() {
    let check = |bytes, query| lookup(bytes, EntityKind::Artist, query);
    assert!(check(b"", "unknown").unwrap().is_empty());
    assert_eq!(check(b"", "\0"), Err(ReadError::InvalidQuery));
    assert_eq!(
        lookup(b"", EntityKind::Artist, &"x".repeat(MAX_ARTIFACT_BYTES + 1)),
        Err(ReadError::InvalidQuery)
    );
    assert_eq!(check(b"\xff\n", "x"), Err(ReadError::InvalidUtf8));
    assert_eq!(check(b"truncated", "x"), Err(ReadError::MissingFinalLf));
    assert_eq!(
        lookup(&vec![b'x'; MAX_ARTIFACT_BYTES + 1], EntityKind::Artist, "x"),
        Err(ReadError::TooLarge)
    );
}
