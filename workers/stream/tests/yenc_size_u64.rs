//! Acceptance tests: single-part yEnc sizes compare in `u64`, not narrowed `usize`.
//!
//! Contract under test (`workers/stream/src/yenc.rs:253,266`):
//!
//! - the two single-part size compares (`yend_size`, `file_size` against the
//!   decoded payload length) must compare in `u64` (widening `out.len()` as
//!   the multipart path already does at `:261` with `out.len() as u64`);
//! - narrowing a declared `u64` with `as usize` truncates on 32-bit targets
//!   (`wasm32`: `usize` is 32-bit), so a declared size differing by `k * 2^32`
//!   can compare equal and accept a corrupt article.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - restoring either `yend_size as usize` or `file_size as usize` fails
//!   `single_part_size_compares_do_not_narrow_declared_u64` on any host
//!   (source-text pin via `include_str!`, comments stripped first so a
//!   comment-out cannot hide the narrowing);
//! - deleting either size check fails the `*_plus_one_is_size_mismatch`
//!   behavioural pins (they still require `SizeMismatch` for a one-byte lie);
//! - exact-size articles keep decoding (no-checksum fixtures, so a sibling
//!   checksum-precedence change cannot turn these red).
//!
//! Deliberately unpinned: the exact widening syntax (any `u64` comparison
//! passes: `out.len() as u64`, `u64::try_from`, etc.), multipart `pcrc32`
//! scope, core trailer-size parsing, and checksum precedence.
//!
//! Out of scope: `yenc_checksum_syntax.rs`, `yenc_escape_framing.rs` (untouched).
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-stream --test yenc_size_u64
//! ```

use nntune_stream::yenc::{decode, decode_part, YencError};

/// Decoder source, read as text (not imported): the RED gate pins the
/// narrowing textually so it fails on 64-bit hosts too, where a behavioural
/// `decoded_len + 2^32` decode is green today.
const YENC_RS: &str = include_str!("../src/yenc.rs");

/// Strip Rust `//` line comments and `/* … */` block comments (nesting-aware),
/// preserving string/char/raw-string contents and newlines. Comments are
/// replaced with spaces so `yend_size/*c*/as usize` still reads as narrowing
/// while `// yend_size as usize` in a comment does not count.
fn strip_rust_comments(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    let mut block_depth = 0usize;
    let mut in_line = false;
    let mut in_str: Option<char> = None;
    let mut escaped = false;
    let mut in_raw: Option<usize> = None;

    while i < chars.len() {
        if in_line {
            if chars[i] == '\n' {
                in_line = false;
                out.push('\n');
            } else {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                block_depth += 1;
                out.push(' ');
                out.push(' ');
                i += 2;
                continue;
            }
            if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                block_depth -= 1;
                out.push(' ');
                out.push(' ');
                i += 2;
                continue;
            }
            if chars[i] == '\n' {
                out.push('\n');
            } else {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if let Some(hashes) = in_raw {
            if chars[i] == '"' {
                let mut ok = true;
                for k in 0..hashes {
                    if i + 1 + k >= chars.len() || chars[i + 1 + k] != '#' {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    out.push('"');
                    for _ in 0..hashes {
                        out.push('#');
                    }
                    i += 1 + hashes;
                    in_raw = None;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
            continue;
        }
        if let Some(q) = in_str {
            out.push(chars[i]);
            if escaped {
                escaped = false;
            } else if chars[i] == '\\' {
                escaped = true;
            } else if chars[i] == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        // Normal code: check for a raw-string start (`r"`, `r#*"`, `br"`, `br#*"`).
        if let Some((hashes, len)) = raw_start(&chars, i) {
            for k in 0..len {
                out.push(chars[i + k]);
            }
            i += len;
            in_raw = Some(hashes);
            continue;
        }
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            in_line = true;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            block_depth = 1;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        if chars[i] == '"' {
            in_str = Some('"');
            out.push('"');
            i += 1;
            continue;
        }
        if chars[i] == '\'' {
            // Lifetime (`'a`, `'static`) vs char literal (`'x'`, `'\n'`):
            // only enter string mode when a closing `'` lands within a few chars.
            let mut is_char = false;
            for k in 1..=4 {
                if i + k >= chars.len() {
                    break;
                }
                if chars[i + k] == '\'' {
                    is_char = true;
                    break;
                }
                if matches!(chars[i + k], '\n' | ' ' | ';' | ',' | '(' | ')') {
                    break;
                }
            }
            if is_char {
                in_str = Some('\'');
            }
            out.push('\'');
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// If `chars[i..]` starts a raw string, return `(hash_count, prefix_len)`
/// including the opening `"` (e.g. `r"` -> `(0, 2)`, `br##"` -> `(2, 5)`).
/// Returns `None` for a bare `r`/`br` that is part of a longer identifier.
fn raw_start(chars: &[char], i: usize) -> Option<(usize, usize)> {
    let prev_is_ident = i > 0 && (chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
    if prev_is_ident {
        return None;
    }
    // `br` prefix.
    if chars[i] == 'b'
        && i + 1 < chars.len()
        && chars[i + 1] == 'r'
        && !(i + 2 < chars.len() && (chars[i + 2].is_ascii_alphanumeric() || chars[i + 2] == '_'))
    {
        let mut j = i + 2;
        let mut hashes = 0usize;
        while j < chars.len() && chars[j] == '#' {
            hashes += 1;
            j += 1;
        }
        if j < chars.len() && chars[j] == '"' {
            return Some((hashes, j - i + 1));
        }
        return None;
    }
    // `r` prefix.
    if chars[i] == 'r' {
        // `r` followed by alphanum/`_` is an identifier, not a raw string.
        if i + 1 < chars.len() && (chars[i + 1].is_ascii_alphanumeric() || chars[i + 1] == '_') {
            // Except `#` and `"` which start a raw string.
            if chars[i + 1] != '#' && chars[i + 1] != '"' {
                return None;
            }
        }
        let mut j = i + 1;
        let mut hashes = 0usize;
        while j < chars.len() && chars[j] == '#' {
            hashes += 1;
            j += 1;
        }
        if j < chars.len() && chars[j] == '"' {
            return Some((hashes, j - i + 1));
        }
    }
    None
}

/// True when `stripped` contains `name as usize` with any whitespace between
/// the tokens (word-boundary aware, so `my_yend_size` does not match).
fn narrows_to_usize(stripped: &str, name: &str) -> bool {
    let bytes = stripped.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = stripped[from..].find(name) {
        let abs = from + rel;
        let before_ok =
            abs == 0 || !(bytes[abs - 1].is_ascii_alphanumeric() || bytes[abs - 1] == b'_');
        let after_name = abs + name.len();
        let after_ok = after_name >= bytes.len()
            || !(bytes[after_name].is_ascii_alphanumeric() || bytes[after_name] == b'_');
        if before_ok && after_ok {
            let mut k = after_name;
            while k < bytes.len() && matches!(bytes[k], b' ' | b'\t' | b'\n' | b'\r') {
                k += 1;
            }
            if stripped[k..].starts_with("as") {
                let after_as = k + 2;
                let as_after_ok = after_as >= bytes.len()
                    || !(bytes[after_as].is_ascii_alphanumeric() || bytes[after_as] == b'_');
                if as_after_ok {
                    let mut j = after_as;
                    while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
                        j += 1;
                    }
                    if stripped[j..].starts_with("usize") {
                        let after_usize = j + "usize".len();
                        let usize_after_ok = after_usize >= bytes.len()
                            || !(bytes[after_usize].is_ascii_alphanumeric()
                                || bytes[after_usize] == b'_');
                        if usize_after_ok {
                            return true;
                        }
                    }
                }
            }
        }
        from = abs + name.len();
    }
    false
}

/// Minimal single-part article with no checksum tags (checksum precedence
/// stays out of scope): body `XYZ` decodes to exactly 3 bytes.
fn article(ybegin_size: u64, yend_size: u64) -> Vec<u8> {
    format!("=ybegin line=128 size={ybegin_size} name=a.bin\r\nXYZ\r\n=yend size={yend_size}\r\n")
        .into_bytes()
}

// ---------------------------------------------------------------------------
// Source-text RED gate: fails on the pre-fix source on any host.
// ---------------------------------------------------------------------------

#[test]
fn single_part_size_compares_do_not_narrow_declared_u64() {
    let stripped = strip_rust_comments(YENC_RS);
    for name in ["yend_size", "file_size"] {
        assert!(
            !narrows_to_usize(&stripped, name),
            "workers/stream/src/yenc.rs still narrows `{name}` with `as usize`; \
             compare yEnc sizes in `u64` (widen `out.len()`, as the multipart \
             path already does with `out.len() as u64`)"
        );
    }
    // The checks must still exist (a deletion would otherwise pass the
    // negative pin and fail behaviourally below, but call it out here too).
    assert!(
        stripped.contains("SizeMismatch"),
        "yenc.rs must still report SizeMismatch for single-part size lies"
    );
    assert!(
        stripped.contains("yend_size") && stripped.contains("file_size"),
        "yenc.rs must still track both single-part declared sizes"
    );
}

// ---------------------------------------------------------------------------
// Behavioural controls: green before and after the fix (tiny sizes, no wrap).
// ---------------------------------------------------------------------------

#[test]
fn exact_size_single_part_decodes() {
    let text = article(3, 3);
    assert_eq!(decode(&text).expect("exact size must decode").len(), 3);
    let got = decode_part(&text).expect("exact size must decode");
    assert_eq!(got.data.len(), 3);
    assert_eq!((got.file_size, got.offset, got.multipart), (3, 0, false));
}

#[test]
fn yend_size_plus_one_is_size_mismatch() {
    // `=ybegin` truthful, `=yend` lies by one: the first single-part compare fires.
    let text = article(3, 4);
    assert!(matches!(
        decode_part(&text),
        Err(YencError::SizeMismatch {
            expected: 4,
            actual: 3
        })
    ));
    assert!(matches!(decode(&text), Err(YencError::SizeMismatch { .. })));
}

#[test]
fn file_size_plus_one_is_size_mismatch() {
    // `=yend` truthful, `=ybegin` lies by one: the second single-part compare fires.
    let text = article(4, 3);
    assert!(matches!(
        decode_part(&text),
        Err(YencError::SizeMismatch {
            expected: 4,
            actual: 3
        })
    ));
    assert!(matches!(decode(&text), Err(YencError::SizeMismatch { .. })));
}

// ---------------------------------------------------------------------------
// Optional 32-bit behavioural pins: not the CI gate (host CI is 64-bit).
// A declared size of `3 + 2^32` truncates to `3` under `as usize` on 32-bit.
// ---------------------------------------------------------------------------

#[cfg(target_pointer_width = "32")]
#[test]
fn wrap_around_yend_size_is_mismatch_on_32bit() {
    const WRAP: u64 = (1u64 << 32) + 3;
    let text = article(3, WRAP);
    assert!(
        matches!(
            decode_part(&text),
            Err(YencError::SizeMismatch {
                expected: WRAP,
                actual: 3
            })
        ),
        "yend_size differing by 2^32 must not compare equal on 32-bit"
    );
}

#[cfg(target_pointer_width = "32")]
#[test]
fn wrap_around_file_size_is_mismatch_on_32bit() {
    const WRAP: u64 = (1u64 << 32) + 3;
    let text = article(WRAP, 3);
    assert!(
        matches!(
            decode_part(&text),
            Err(YencError::SizeMismatch {
                expected: WRAP,
                actual: 3
            })
        ),
        "file_size differing by 2^32 must not compare equal on 32-bit"
    );
}
