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
const TS: &str = "2024-02-29 12:34:56+00:00";

fn row(table: Table) -> Vec<String> {
    let fields = match table {
        Table::Artist => vec![A, "Name", "Sort", "Person", "US", TS],
        Table::Release => vec![R, A, "Title", "Album", "2024", "https://example.test/a", TS],
        Table::Recording => vec![C, R, "Title", "1", "1234", TS],
        Table::Track => vec![R, C, "1", "2", TS],
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
        Table::Release,
        Table::Recording,
        Table::Track,
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
        (Table::Release, 0, R),
        (Table::Release, 1, A),
        (Table::Recording, 0, C),
        (Table::Recording, 1, R),
        (Table::Track, 0, R),
        (Table::Track, 1, C),
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
    ] {
        check_field(table, index, octal, want);
        check_field(table, index, hex, want);
    }
    check_field(Table::Track, 2, r"\x2b001", "1");
    check_field(Table::Track, 3, "0002", "2");
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
        Table::Release,
        Table::Recording,
        Table::Track,
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
        (Table::Release, 1),
        (Table::Release, 4),
        (Table::Recording, 3),
        (Table::Recording, 4),
    ] {
        check_field(table, index, "", r"\N");
        check_field(table, index, r"\N", r"\N");
    }
    for (table, index, reason) in [
        (Table::Artist, 0, "bad_uuid"),
        (Table::Artist, 5, "bad_timestamp"),
        (Table::Track, 2, "bad_int"),
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
