//! Protected acceptance: the Builder must not edit this file.
//! Run: python3 scripts/cargo_cache.py run -- test -p mb-import-validate --test decoded_typed_fields
//! These tests catch raw-token type checks, unsafe decoded COPY output, and
//! normalization that changes nullable/text semantics or poison provenance.
use mb_import_validate::{validate_line, Table};
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const A: &str = "abcdef00-0000-4000-8000-000000000001";
const R: &str = "abcdef00-0000-4000-8000-000000000002";
const C: &str = "abcdef00-0000-4000-8000-000000000003";
const W: &str = "abcdef00-0000-4000-8000-000000000004";
const L: &str = "abcdef00-0000-4000-8000-000000000005";
const TS: &str = "2024-02-29 12:34:56+00:00";

fn row(table: Table) -> Vec<String> {
    let fields = match table {
        Table::Artist => vec![A, "Name", "Sort", "Person", "US", TS],
        Table::ReleaseGroup => vec![R, "Group Name", A, "Album", TS],
        Table::Release => vec![
            R,
            A,
            "Title",
            "Album",
            "2024",
            "https://example.test/a",
            C,
            TS,
        ],
        Table::Recording => vec![C, R, "Title", "1", "1234", TS],
        Table::Track => vec![R, C, "1", "2", TS],
        Table::Url => vec![C, "https://example.test/a", TS],
        Table::ArtistUrl => vec![A, C, "youtube", TS],
        Table::RecordingUrl => vec![C, R, "free streaming", "true", TS],
        Table::RecordingMusicVideo => vec![C, R, TS],
        Table::RecordingIsrc => vec![C, "USABC2400001", TS],
        Table::Work => vec![W, "Work Name", "Song", TS],
        Table::Label => vec![L, "Label Name", "12345", "Original Production", TS],
        Table::ArtistRecording => vec![A, C, "performer", TS],
        Table::ArtistRelease => vec![A, R, "producer", TS],
        Table::ArtistWork => vec![A, W, "composer", TS],
        Table::RecordingWork => vec![C, W, "performance", TS],
        Table::RecordingRecording => vec![C, R, "remix", TS],
        Table::ReleaseCredit => vec![R, "1", A, "Credited Name", " feat. ", TS],
        Table::ArtistAlias => vec![A, "Alias", "en", "Alias, Sort", "true", TS],
        Table::ReleaseAlias => vec![R, "Alias", "en", "Alias, Sort", "true", TS],
        Table::RecordingAlias => vec![C, "Alias", "en", "Alias, Sort", "true", TS],
        Table::ReleaseGroupAlias => vec![R, "Alias", "en", "Alias, Sort", "true", TS],
        Table::WorkAlias => vec![W, "Alias", "en", "Alias, Sort", "true", TS],
        Table::LabelAlias => vec![L, "Alias", "en", "Alias, Sort", "true", TS],
        Table::ReleaseLabel => vec![R, L, "CAT-001", TS],
    };
    fields.into_iter().map(str::to_owned).collect()
}

fn check_field(table: Table, index: usize, token: &str, expected: &str) {
    let mut input = row(table);
    input[index] = token.to_owned();
    let mut want = row(table);
    want[index] = expected.to_owned();
    want.push("37".into());
    assert_eq!(
        validate_line(table, 37, &input.join("\t")),
        Ok(want.join("\t")),
        "{table:?} field {index}, COPY token {token:?} must use its decoded typed value"
    );
}

#[test]
fn literal_normal_flow_controls() {
    for table in [
        Table::Artist,
        Table::ReleaseGroup,
        Table::Release,
        Table::Recording,
        Table::Track,
        Table::Url,
        Table::ArtistUrl,
        Table::RecordingUrl,
        Table::RecordingMusicVideo,
        Table::RecordingIsrc,
        Table::Work,
        Table::Label,
        Table::ArtistRecording,
        Table::ArtistRelease,
        Table::ArtistWork,
        Table::RecordingWork,
        Table::RecordingRecording,
        Table::ReleaseCredit,
        Table::ArtistAlias,
        Table::ReleaseAlias,
        Table::RecordingAlias,
        Table::ReleaseGroupAlias,
        Table::WorkAlias,
        Table::LabelAlias,
        Table::ReleaseLabel,
    ] {
        let fields = row(table);
        assert_eq!(
            validate_line(table, 37, &fields.join("\t")),
            Ok(format!("{}\t37", fields.join("\t")))
        );
    }
}

#[test]
fn octal_and_hex_uuid_aliases_emit_canonical_keys() {
    for (table, index, uuid) in [
        (Table::Artist, 0, A),
        (Table::ReleaseGroup, 0, R),
        (Table::ReleaseGroup, 2, A),
        (Table::Release, 0, R),
        (Table::Release, 1, A),
        (Table::Release, 6, C),
        (Table::Recording, 0, C),
        (Table::Recording, 1, R),
        (Table::Track, 0, R),
        (Table::Track, 1, C),
        (Table::Url, 0, C),
        (Table::ArtistUrl, 0, A),
        (Table::ArtistUrl, 1, C),
        (Table::RecordingUrl, 0, C),
        (Table::RecordingUrl, 1, R),
        (Table::RecordingMusicVideo, 0, C),
        (Table::RecordingMusicVideo, 1, R),
        (Table::RecordingIsrc, 0, C),
        (Table::Work, 0, W),
        (Table::Label, 0, L),
        (Table::ArtistRecording, 0, A),
        (Table::ArtistRecording, 1, C),
        (Table::ArtistRelease, 0, A),
        (Table::ArtistRelease, 1, R),
        (Table::ArtistWork, 0, A),
        (Table::ArtistWork, 1, W),
        (Table::RecordingWork, 0, C),
        (Table::RecordingWork, 1, W),
        (Table::RecordingRecording, 0, C),
        (Table::RecordingRecording, 1, R),
        (Table::ReleaseCredit, 0, R),
        (Table::ReleaseCredit, 2, A),
        (Table::ArtistAlias, 0, A),
        (Table::ReleaseAlias, 0, R),
        (Table::RecordingAlias, 0, C),
        (Table::ReleaseGroupAlias, 0, R),
        (Table::WorkAlias, 0, W),
        (Table::LabelAlias, 0, L),
        (Table::ReleaseLabel, 0, R),
        (Table::ReleaseLabel, 1, L),
    ] {
        for first in [r"\141", r"\x61"] {
            check_field(table, index, &format!("{first}{}", &uuid[1..]), uuid);
        }
        check_field(table, index, &uuid.to_ascii_uppercase(), uuid);
    }
}

#[test]
fn octal_and_hex_integers_emit_canonical_values() {
    for (table, index, octal, hex, want) in [
        (Table::Release, 4, r"\062024", r"\x32024", "2024"),
        (Table::Recording, 3, r"\061", r"\x31", "1"),
        (Table::Recording, 4, r"\061234", r"\x31234", "1234"),
        (Table::Track, 2, r"\061", r"\x31", "1"),
        (Table::Track, 3, r"\062", r"\x32", "2"),
        (Table::Label, 2, r"\0612345", r"\x312345", "12345"),
        (Table::ReleaseCredit, 1, r"\061", r"\x31", "1"),
    ] {
        check_field(table, index, octal, want);
        check_field(table, index, hex, want);
    }
    check_field(Table::Track, 2, r"\x2b001", "1");
    check_field(Table::Track, 3, "0002", "2");
    // Label codes are INTEGER, not SMALLINT: past 32767 and signed values
    // stay valid and canonicalize like every other integer column.
    check_field(Table::Label, 2, "40000", "40000");
    check_field(Table::Label, 2, r"\x2b0040000", "40000");
    check_field(Table::Label, 2, "-0007", "-7");
    check_field(Table::ReleaseCredit, 1, "0", "0");
    check_field(Table::ReleaseCredit, 1, "0003", "3");
}

#[test]
fn octal_and_hex_booleans_emit_canonical_values() {
    for table in [
        Table::ArtistAlias,
        Table::ReleaseAlias,
        Table::RecordingAlias,
        Table::ReleaseGroupAlias,
        Table::WorkAlias,
        Table::LabelAlias,
    ] {
        for (token, want) in [
            (r"\x74rue", "true"),
            (r"\164rue", "true"),
            ("false", "false"),
            (r"\x66alse", "false"),
        ] {
            check_field(table, 4, token, want);
        }
    }
    check_field(Table::RecordingUrl, 3, r"\x66alse", "false");
}

#[test]
fn octal_and_hex_countries_use_decoded_character_count() {
    check_field(Table::Artist, 4, r"\125S", "US");
    check_field(Table::Artist, 4, r"\x55S", "US");
    check_field(Table::Artist, 4, r"\303\251A", "éA");
    check_field(Table::Artist, 4, r"\xc3\xa9A", "éA");
}

#[test]
fn timestamps_validate_decoded_digits_and_spaces() {
    for table in [
        Table::Artist,
        Table::ReleaseGroup,
        Table::Release,
        Table::Recording,
        Table::Track,
        Table::Url,
        Table::ArtistUrl,
        Table::RecordingUrl,
        Table::RecordingMusicVideo,
        Table::RecordingIsrc,
        Table::Work,
        Table::Label,
        Table::ArtistRecording,
        Table::ArtistRelease,
        Table::ArtistWork,
        Table::RecordingWork,
        Table::RecordingRecording,
        Table::ReleaseCredit,
        Table::ArtistAlias,
        Table::ReleaseAlias,
        Table::RecordingAlias,
        Table::ReleaseGroupAlias,
        Table::WorkAlias,
        Table::LabelAlias,
        Table::ReleaseLabel,
    ] {
        let index = row(table).len() - 1;
        check_field(table, index, r"\062024-02-29\04012:34:56+00:00", TS);
        check_field(table, index, r"\x32024-02-29\x2012:34:56+00:00", TS);
    }
}

#[test]
fn semantic_failures_keep_the_literal_reason_taxonomy() {
    // Compare literal and encoded inputs to explicit, stable public reasons.
    for (table, index, literal, encoded, reason) in [
        (Table::Artist, 0, "not-a-uuid", r"\x6eot-a-uuid", "bad_uuid"),
        (Table::Release, 4, "32768", r"\x332768", "bad_int"),
        (Table::Recording, 3, "-32769", r"\05532769", "bad_int"),
        (Table::Recording, 4, "-1", r"\0551", "negative_duration"),
        (
            Table::Recording,
            4,
            "2147483648",
            r"\x32147483648",
            "bad_int",
        ),
        (Table::Track, 2, "-1", r"\x2d1", "negative_position"),
        (Table::Track, 3, "32768", r"\0632768", "bad_int"),
        (Table::Artist, 4, "USA", r"\x55SA", "bad_country"),
        (Table::Artist, 4, "U", r"\125", "bad_country"),
        (
            Table::RecordingIsrc,
            1,
            "USABC24000",
            r"\x55SABC24000",
            "bad_isrc",
        ),
        (
            Table::RecordingIsrc,
            1,
            "12ABC2400001",
            r"\x312ABC2400001",
            "bad_isrc",
        ),
        (Table::RecordingUrl, 3, "yes", r"\x79es", "bad_bool"),
        (Table::RecordingUrl, 3, "TRUE", r"\x54RUE", "bad_bool"),
        (Table::ArtistAlias, 4, "t", r"\x74", "bad_bool"),
        (Table::LabelAlias, 4, "1", r"\061", "bad_bool"),
        (Table::WorkAlias, 4, "True", r"\x54rue", "bad_bool"),
        (Table::Work, 0, "not-a-uuid", r"\x6eot-a-uuid", "bad_uuid"),
        (
            Table::ReleaseLabel,
            1,
            "not-a-uuid",
            r"\156ot-a-uuid",
            "bad_uuid",
        ),
        (Table::Label, 2, "2147483648", r"\x32147483648", "bad_int"),
        (Table::Label, 2, "12a", r"\x312a", "bad_int"),
        (Table::ReleaseCredit, 1, "-1", r"\x2d1", "negative_position"),
        (Table::ReleaseCredit, 1, "32768", r"\0632768", "bad_int"),
        (
            Table::ArtistAlias,
            5,
            "2023-02-29",
            r"\x32023-02-29",
            "bad_timestamp",
        ),
        (
            Table::Artist,
            5,
            "2023-02-29",
            r"\x32023-02-29",
            "bad_timestamp",
        ),
        (
            Table::Artist,
            5,
            "2024-01-01 25:00:00",
            r"2024-01-01\04025:00:00",
            "bad_timestamp",
        ),
    ] {
        for token in [literal, encoded] {
            let mut fields = row(table);
            fields[index] = token.into();
            assert_eq!(
                validate_line(table, 81, &fields.join("\t")),
                Err((reason, 81)),
                "{table:?} field {index}: {token:?}"
            );
        }
    }
}

#[test]
fn two_byte_escape_spelling_is_not_a_two_character_country() {
    for token in [r"\t", r"\n", r"\r", r"\b", r"\f", r"\v", r"\\"] {
        let mut fields = row(Table::Artist);
        fields[4] = token.into();
        assert_eq!(
            validate_line(Table::Artist, 19, &fields.join("\t")),
            Err(("bad_country", 19)),
            "{token:?} decodes to one character"
        );
    }
}

#[test]
fn typed_control_characters_are_copy_safe_not_new_columns_or_rows() {
    for (token, safe) in [(r"\x09A", r"\tA"), (r"\012A", r"\nA"), (r"\015A", r"\rA")] {
        check_field(Table::Artist, 4, token, safe);
    }
    // The decoded country is the two characters backslash + N, NOT SQL NULL.
    check_field(Table::Artist, 4, r"\134N", r"\\N");
    check_field(Table::Artist, 4, r"\x5cN", r"\\N");
}

#[test]
fn text_escapes_nullable_markers_and_empty_rules_are_preserved() {
    for token in [r"tab\tline\nslash\\", r"\\N", r"\x55S", ""] {
        check_field(Table::Artist, 1, token, token);
    }
    for (table, index) in [
        (Table::Artist, 3),
        (Table::Artist, 4),
        (Table::ReleaseGroup, 2),
        (Table::ReleaseGroup, 3),
        (Table::Release, 1),
        (Table::Release, 3),
        (Table::Release, 4),
        (Table::Release, 5),
        (Table::Release, 6),
        (Table::Recording, 1),
        (Table::Recording, 3),
        (Table::Recording, 4),
        (Table::Work, 2),
        (Table::Label, 2),
        (Table::Label, 3),
    ] {
        check_field(table, index, "", r"\N");
        check_field(table, index, r"\N", r"\N");
    }
    // NOT NULL text columns keep the empty string (the ETL rewrites NULL
    // alias locales to ''), but a bare `\N` would load as NULL: poison.
    for (table, index) in [
        (Table::Work, 1),
        (Table::Label, 1),
        (Table::ArtistRecording, 2),
        (Table::ArtistRelease, 2),
        (Table::ArtistWork, 2),
        (Table::RecordingWork, 2),
        (Table::RecordingRecording, 2),
        (Table::ReleaseCredit, 3),
        (Table::ReleaseCredit, 4),
        (Table::ArtistAlias, 1),
        (Table::ArtistAlias, 2),
        (Table::ArtistAlias, 3),
        (Table::ReleaseAlias, 2),
        (Table::RecordingAlias, 2),
        (Table::ReleaseGroupAlias, 2),
        (Table::WorkAlias, 2),
        (Table::LabelAlias, 2),
        (Table::ReleaseLabel, 2),
    ] {
        check_field(table, index, "", "");
        let mut fields = row(table);
        fields[index] = r"\N".into();
        assert_eq!(
            validate_line(table, 9, &fields.join("\t")),
            Err(("null_in_not_null", 9)),
            "{table:?} field {index}"
        );
    }
    for token in [r"tab\tline\nslash\\", r"\\N", r"\x55S"] {
        check_field(Table::ReleaseCredit, 4, token, token);
        check_field(Table::ReleaseLabel, 2, token, token);
    }
    for (table, index, reason) in [
        (Table::Artist, 0, "bad_uuid"),
        (Table::Artist, 5, "bad_timestamp"),
        (Table::Track, 2, "bad_int"),
        (Table::Work, 0, "bad_uuid"),
        (Table::ReleaseLabel, 1, "bad_uuid"),
        (Table::ReleaseCredit, 1, "bad_int"),
        (Table::ArtistAlias, 4, "bad_bool"),
        (Table::LabelAlias, 5, "bad_timestamp"),
    ] {
        let mut fields = row(table);
        fields[index] = String::new();
        assert_eq!(
            validate_line(table, 9, &fields.join("\t")),
            Err((reason, 9))
        );
        fields[index] = r"\N".into();
        assert_eq!(
            validate_line(table, 9, &fields.join("\t")),
            Err(("null_in_not_null", 9))
        );
    }
}

#[test]
fn malformed_unsupported_nul_and_non_utf8_escapes_stay_refused() {
    for token in [
        r"\x",
        r"\q",
        r"\8",
        r"\400",
        r"\000",
        r"\x00",
        r"\377",
        r"\xc3\x28",
        "trailing\\",
        "raw\rcr",
        "raw\0nul",
    ] {
        for index in [1, 4] {
            let mut fields = row(Table::Artist);
            fields[index] = token.into();
            assert_eq!(
                validate_line(Table::Artist, 7, &fields.join("\t")),
                Err(("bad_escape", 7)),
                "field {index}: {token:?}"
            );
        }
    }
}

#[test]
fn offline_importer_normalization_keys_fk_dlq_and_resume() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../containers/mb-import/tests/test_decoded_typed_fields.py");
    let out = Command::new("python3")
        .arg(script)
        .arg("-v")
        .env("MB_VALIDATE_BIN", env!("CARGO_BIN_EXE_mb-validate"))
        .output()
        .expect("python3 is required by the importer itself");
    assert!(
        out.status.success(),
        "offline importer acceptance failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

static NEXT: AtomicU64 = AtomicU64::new(0);

#[test]
fn binary_keeps_poison_raw_bytes_offsets_and_crlf_while_normalizing_valid_rows() {
    let root = std::env::var_os("PAPERCLIP_RUN_SCRATCH_DIR")
        .or_else(|| std::env::var_os("PAPERCLIP_SCRATCH_DIR"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = root.join(format!(
        "decoded-copy-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let mut good = row(Table::Artist);
    good[4] = r"\x55S".into();
    let mut bad = row(Table::Artist);
    bad[4] = r"\t".into();
    let raw_bad = bad.join("\t");
    let mut invalid = row(Table::Artist).join("\t").into_bytes();
    invalid.splice(A.len() + 1..A.len() + 5, [0xff]);
    let mut bytes = format!("{}\r\n{raw_bad}\r\n", good.join("\t")).into_bytes();
    bytes.extend_from_slice(&invalid);
    bytes.push(b'\n');
    fs::write(dir.join("in.tsv"), bytes).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mb-validate"))
        .args(["--table", "artist", "--in"])
        .arg(dir.join("in.tsv"))
        .arg("--valid-out")
        .arg(dir.join("valid.tsv"))
        .arg("--poison-out")
        .arg(dir.join("poison.tsv"))
        .args(["--line-offset", "100"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let valid = fs::read_to_string(dir.join("valid.tsv")).unwrap();
    let poison = fs::read_to_string(dir.join("poison.tsv")).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "valid=1 poison=2\n");
    assert_eq!(valid, format!("{}\t101\n", row(Table::Artist).join("\t")));
    // Independent COPY escaping of the ORIGINAL token, not normalized country.
    let escaped_bad = raw_bad.replace('\\', "\\\\").replace('\t', "\\t");
    let escaped_invalid = String::from_utf8_lossy(&invalid)
        .replace('\\', "\\\\")
        .replace('\t', "\\t");
    assert_eq!(
        poison,
        format!("bad_country\t102\t{escaped_bad}\ninvalid_encoding\t103\t{escaped_invalid}\n")
    );
}
