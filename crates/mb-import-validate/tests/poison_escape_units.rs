//! Poison-escape unit pins for `escape_poison_field`.
//!
//! Tests-only pin of current behavior (refill-19 slice of TOG-30191);
//! implementation stays untouched. Backslash is escaped first in a single
//! left-to-right pass, so a lone backslash doubles to `\\` and a following
//! TAB becomes `\t` without re-escaping the introduced backslash. NUL
//! becomes the COPY-safe literal `\\x00` (double backslash + x00, no raw
//! 0x00 byte) so the poison row loads through `COPY (FORMAT text)` as the
//! visible text `\x00`.

use mb_import_validate::escape_poison_field;

#[test]
fn nul_maps_to_literal_x00_no_raw_nul() {
    let out = escape_poison_field("bad\0row");
    assert_eq!(out, "bad\\\\x00row");
    assert!(
        !out.contains('\0'),
        "escaped output must not carry a raw NUL byte"
    );
    assert!(
        !out.as_bytes().contains(&0),
        "escaped bytes must not contain 0x00"
    );
}

#[test]
fn lone_backslash_doubles_before_tab_escape() {
    // Lone backslash doubles.
    assert_eq!(escape_poison_field("\\"), "\\\\");
    // Lone TAB becomes backslash-t.
    assert_eq!(escape_poison_field("\t"), "\\t");
    // Backslash + TAB: backslash-first ordering. The output is the doubled
    // backslash followed by the tab escape: `\\` + `\t` (four bytes:
    // backslash, backslash, backslash, t). Tab-first ordering would
    // re-escape the introduced backslash and yield five bytes instead.
    let out = escape_poison_field("\\\t");
    assert_eq!(out.as_bytes(), &[b'\\', b'\\', b'\\', b't']);
    assert_eq!(out, format!("{}{}", "\\\\", "\\t"));
}

#[test]
fn cr_lf_map_to_r_n_escapes() {
    assert_eq!(escape_poison_field("\r"), "\\r");
    assert_eq!(escape_poison_field("\n"), "\\n");
    let out = escape_poison_field("a\rb\nc");
    assert_eq!(out, "a\\rb\\nc");
    assert!(
        !out.contains('\r') && !out.contains('\n'),
        "escaped output must not carry raw CR/LF bytes, got {out:?}"
    );
}

#[test]
fn plain_text_unchanged() {
    assert_eq!(escape_poison_field("hello"), "hello");
    assert_eq!(escape_poison_field("The Band"), "The Band");
    assert_eq!(escape_poison_field(""), "");
}
