use mb_import_validate::{
    aliases, plan_chunks, reason, relationships, release_credits, release_labels, validate_line,
    works, Table,
};

const A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const TS: &str = "2024-02-29T12:34:56.000001Z";

#[test]
fn copy_rows_require_a_positive_bigint_line_number() {
    let row = format!("{A}\tURL\t{TS}");
    let max = i64::MAX as u64;
    for line in [0, max + 1, u64::MAX] {
        assert_eq!(
            validate_line(Table::Url, line, &row),
            Err(("bad_line_number", line))
        );
    }
    assert_eq!(
        validate_line(Table::Url, max, &row),
        Ok(format!("{row}\t{max}"))
    );
}

#[test]
fn copy_rows_reject_excess_columns_and_keep_escaped_delimiters() {
    let name = r"a\011b\x0ac\134N";
    let row = format!("{A}\t{name}\tSort\t\t\\x09U\t{TS}");
    assert_eq!(
        validate_line(Table::Artist, 1, &row),
        Ok(format!("{A}\t{name}\tSort\t\\N\t\\tU\t{TS}\t1"))
    );
    assert_eq!(
        validate_line(Table::Artist, 1, &format!("{row}{}", "\t".repeat(4096))),
        Err((reason::COLUMN_COUNT, 1))
    );
    for bad in ["raw\0nul", r"\x00", r"\xc3\x28", "raw\rcr", "trailing\\"] {
        assert_eq!(
            validate_line(Table::Artist, 2, &row.replace(name, bad)),
            Err((reason::BAD_ESCAPE, 2))
        );
    }
}

#[test]
fn video_pair_refuses_same_mbid_under_distinct_spellings() {
    let escaped_a = format!(r"\101{}", &A.to_ascii_uppercase()[1..]);
    assert_eq!(
        validate_line(
            Table::RecordingMusicVideo,
            3,
            &format!("{A}\t{escaped_a}\t{TS}")
        ),
        Err(("bad_video_pair", 3))
    );
    assert_eq!(
        validate_line(
            Table::RecordingMusicVideo,
            3,
            &format!("{escaped_a}\t{B}\t{TS}")
        ),
        Ok(format!("{A}\t{B}\t{TS}\t3"))
    );
}

#[test]
#[should_panic(expected = "chunk plan exceeds 16 MiB of ranges")]
fn chunk_plan_refuses_an_oversized_range_count() {
    plan_chunks(u64::MAX - 1_048_577, u64::MAX, 1);
}

#[test]
fn chunk_plan_accepts_a_small_plan_near_u64_max() {
    let max = u64::MAX;
    assert_eq!(
        plan_chunks(max - 4, max, 3),
        vec![(max - 3, max - 1), (max, max)]
    );
}

#[test]
fn alias_locale_distinguishes_empty_and_literal_backslash_n_from_null_marker() {
    let aliases = |locale| format!("{A}\tAlias\t{locale}\tSort\tf\t{TS}\n");
    let lookup =
        |rows: &str| aliases::lookup(rows.as_bytes(), aliases::EntityKind::Artist, "Alias");
    assert_eq!(lookup(&aliases("")).unwrap()[0].locale, None);
    assert_eq!(
        lookup(&aliases(r"\\N")).unwrap()[0].locale.as_deref(),
        Some(r"\N")
    );
    assert_eq!(
        lookup(&aliases(r"\N")),
        Err(aliases::ReadError::InvalidRow {
            line: 1,
            reason: reason::NULL_IN_NOT_NULL
        })
    );
}

#[test]
fn credit_position_requires_canonical_decoded_decimal_on_unmatched_rows() {
    let matching = format!("{A}\t\\061\t{B}\tName\t\t{TS}\n");
    let credits = release_credits::lookup(matching.as_bytes(), A).unwrap();
    assert_eq!(credits[0].position, 1);
    for position in ["+1", "01", "-0", r"\0531"] {
        let rows = format!("{matching}{B}\t{position}\t{A}\tOther\t\t{TS}\n");
        assert_eq!(
            release_credits::lookup(rows.as_bytes(), A),
            Err(release_credits::ReadError::InvalidRow {
                line: 2,
                reason: "invalid_credit_position"
            })
        );
    }
}

#[test]
fn curated_readers_reject_zero_fraction_after_a_valid_row() {
    let bad = "2024-02-29T12:34:56.000000Z";
    let alias = format!("{A}\tAlias\t\tSort\tf\t{TS}\n{B}\tOther\t\tSort\tf\t{bad}\n");
    let edges = format!("{A}\t{B}\tcredit\t{TS}\n{B}\t{A}\tcredit\t{bad}\n");
    let credits = format!("{A}\t0\t{B}\tName\t\t{TS}\n{B}\t0\t{A}\tOther\t\t{bad}\n");
    let labels = format!("{A}\tLabel\t0\tKind\t{TS}\n{B}\tOther\t1\tKind\t{bad}\n");
    let works = format!("{A}\tWork\tKind\t{TS}\n{B}\tOther\tKind\t{bad}\n");
    let why = reason::BAD_TIMESTAMP;
    assert_eq!(
        aliases::lookup(alias.as_bytes(), aliases::EntityKind::Artist, "Alias"),
        Err(aliases::ReadError::InvalidRow {
            line: 2,
            reason: why
        })
    );
    assert_eq!(
        relationships::lookup(relationships::Projection::ArtistWork, edges.as_bytes(), A),
        Err(relationships::ReadError::InvalidRow {
            line: 2,
            reason: why
        })
    );
    assert_eq!(
        release_credits::lookup(credits.as_bytes(), A),
        Err(release_credits::ReadError::InvalidRow {
            line: 2,
            reason: why
        })
    );
    assert_eq!(
        release_labels::lookup(labels.as_bytes(), b"", A),
        Err(release_labels::ReadError::InvalidRow(
            release_labels::Artifact::Labels,
            2,
            why
        ))
    );
    assert_eq!(
        release_labels::lookup(
            format!("{B}\tLabel\t0\tKind\t{TS}\n").as_bytes(),
            format!("{A}\t{B}\tFirst\t{TS}\n{B}\t{B}\tSecond\t{bad}\n").as_bytes(),
            A
        ),
        Err(release_labels::ReadError::InvalidRow(
            release_labels::Artifact::ReleaseLabels,
            2,
            why
        ))
    );
    assert_eq!(
        works::lookup(works.as_bytes(), A),
        Err(works::ReadError::InvalidRow {
            line: 2,
            reason: why
        })
    );
}
