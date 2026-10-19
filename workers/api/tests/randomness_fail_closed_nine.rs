//! Randomness must fail closed at the nine named `getrandom` call sites
//! (tests-only).
//!
//! Contract under test: none of the nine `getrandom` paths below may turn a
//! randomness failure into a panic via `.expect(..)`. Each path must instead
//! propagate the error to its caller (e.g. `?` / `.map_err(..)?`), so the
//! request fails closed rather than the worker panicking (or, worse,
//! continuing with unfilled buffers). Sites as of source SHA `b8e8f744`:
//!
//! - `workers/api/src/auth.rs:43`, `workers/api/src/auth.rs:291`
//! - `workers/api/src/library.rs:127`
//! - `workers/api/src/handlers.rs:929`
//! - `workers/api/src/session.rs:83`
//! - `workers/api/src/vault.rs:490`, `workers/api/src/vault.rs:514`
//! - `workers/api/src/native_rest.rs:225`,
//!   `workers/api/src/native_rest.rs:231`
//!
//! All nine currently read
//! `getrandom::fill(..).expect("secure randomness unavailable")`, so this
//! suite is RED on the unfixed tree (assertion failure listing the nine
//! sites, not a compile error) and GREEN once the paired build propagates
//! the error. Line numbers above are the pre-fix locations for orientation
//! only; the assertions locate sites dynamically and never pin a line
//! number, an error type, or a replacement spelling, so any honest
//! error-propagating fix passes.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - keeping `.expect(..)` on any of the nine statements fails
//!   `nine_named_files_no_expect_on_getrandom`, which lists every
//!   offending `file:line` it finds;
//! - moving the nine fills behind a shared helper that still panics fails
//!   `crate_wide_no_expect_on_getrandom_including_ledger`: the scan covers
//!   every `.rs` file under `src/`, not just the six named files, so
//!   relocating the panic does not evade it;
//! - swapping `.expect(..)` for `.unwrap()` / `.unwrap_or(..)` /
//!   `.unwrap_or_else(..)` / `.unwrap_or_default()` (same panic, or a
//!   silent fallback buffer) fails
//!   `nine_named_files_no_unwrap_on_getrandom`;
//! - deleting the randomness calls outright instead of propagating their
//!   errors fails `crate_still_uses_getrandom`.
//!
//! The `scanner_*` self-tests pin the scan itself against synthetic
//! sources: it flags same-line and multi-line `.expect(..)`, accepts `?`
//! and `.map_err(..)?`, ignores `//` comment lines, and stops each
//! statement window at its `;` so a later statement's `.expect(..)` is
//! never charged to a `getrandom` call. The scan is spelling-insensitive:
//! it strips comments/strings and anchors on the bare `getrandom` word
//! token plus aliases bound to it, so a crate alias, an imported `fill`
//! or a comment/whitespace-separated path is flagged exactly like a raw
//! `getrandom::fill`, and a `;` inside a string cannot end a statement
//! window early.
//!
//! Boundary: a tenth site in `workers/api/src/ledger.rs` (the
//! idempotency-key mint) was once excluded from the crate-wide scan by
//! name; that exclusion is lifted — the scan now covers `ledger.rs`
//! like every other file, and `ledger_mint_shared_randomness` pins the
//! mint's routing through the shared helper. Behavioral error-path
//! tests (forcing an OS RNG failure) are out of scope: `getrandom`
//! failure cannot be induced portably in a unit test, which is why the
//! card specifies a source assertion. Tests only; no production code
//! is touched.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --manifest-path workers/api/Cargo.toml --test randomness_fail_closed_nine
//! ```

use std::path::PathBuf;

/// The six files holding the nine named call sites, relative to
/// `workers/api/src/`.
const NAMED_FILES: &[&str] = &[
    "auth.rs",
    "library.rs",
    "handlers.rs",
    "session.rs",
    "vault.rs",
    "native_rest.rs",
];

const EXPECT_BAN: &[&str] = &[".expect("];
const UNWRAP_BAN: &[&str] = &[
    ".unwrap(",
    ".unwrap_or(",
    ".unwrap_or_else(",
    ".unwrap_or_default(",
];

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_src(rel: &str) -> String {
    let path = src_dir().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every `.rs` path under `dir`, recursively, sorted for stable messages.
fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot read dir entry: {e}"));
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out.sort();
}

// --- Spelling-insensitive scan helpers ---------------------------------------
//
// The scanners below never match raw source text. They first blank comments
// and string/char/raw-string contents with `strip_code_for_scan` (mirrored
// from `strip_code_for_propagation_pin` in `src/random.rs`; tests cannot
// reach that `#[cfg(test)]` helper, so keep the copies in sync), then anchor
// on the bare `getrandom` word token. That makes the scans insensitive to
// fmt-clean rewrites a raw `getrandom::` needle misses: a crate alias
// (`use getrandom as gr; gr::fill(..)`), whitespace or a block comment
// around the path separator (`getrandom /* entropy */ ::fill(..)`), and a
// leading `::`. Statement windows are located on the stripped text, so a
// `;` inside a string no longer terminates a statement early. Prose on
// comment lines and inside strings stays invisible, as before.

/// Identifier-continue for literal suffixes and raw identifiers
/// (conservative XID-Continue: non-ASCII in code outside strings and
/// comments only occurs in identifiers, and `is_alphanumeric` misses
/// XID-Continue chars like U+203F and U+0301 that rustc eats as suffix).
fn ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric() || !c.is_ascii()
}

/// Identifier-start for raw identifiers (`r#ident`, possibly multi-`#`).
fn ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic() || !c.is_ascii()
}

/// True when the `r` at `chars[i]` starts a raw-string prefix at a token
/// boundary (mirrors `rustc_lexer`): the maximal identifier-char run
/// ending at this `r` must be exactly `r`, `br` or `cr` — a digit-led run
/// like `1r` is excluded by the exact-run rule — and the char before that
/// run must not be a quote, so a literal suffix (`"a"r`, `'a'r`) never
/// counts as a raw prefix. The run walk uses [`ident_continue`] so
/// XID-Continue suffixes (`"a"e<U+203F>r`) are read as one suffix, not
/// as a bare `r`. Hash-closed raw suffixes (`r#"a"#r`) and raw
/// identifiers (`r#r`) are handled by forward token consumption in the
/// stripper, not by this backward check.
fn is_raw_prefix_at(chars: &[char], i: usize) -> bool {
    let mut run_start = i;
    while run_start > 0 {
        let prev = chars[run_start - 1];
        if ident_continue(prev) {
            run_start -= 1;
        } else {
            break;
        }
    }
    let run: String = chars[run_start..=i].iter().collect();
    if run != "r" && run != "br" && run != "cr" {
        return false;
    }
    if run_start > 0 {
        let before = chars[run_start - 1];
        if before == '"' || before == '\'' {
            return false;
        }
    }
    true
}

fn strip_code_for_scan(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        // Raw string `r#"..."#` / `r"..."` (and `br`/`cr` forms): `r` +
        // hashes + `"` only at a token boundary, so a literal suffix `r`
        // never blanks a following plain string with raw rules.
        if c == 'r' && (next == Some('"') || next == Some('#')) && is_raw_prefix_at(&chars, i) {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while chars.get(j).copied() == Some('#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j).copied() == Some('"') {
                for ch in chars.iter().take(j + 1).skip(i) {
                    out.push(*ch);
                }
                i = j + 1;
                // Blank the raw-string body; preserve newlines.
                loop {
                    if i >= chars.len() {
                        break;
                    }
                    if chars[i] == '"'
                        && (0..hashes).all(|h| chars.get(i + 1 + h).copied() == Some('#'))
                    {
                        out.push('"');
                        for h in 0..hashes {
                            out.push(chars[i + 1 + h]);
                        }
                        i += 1 + hashes;
                        break;
                    }
                    if chars[i] == '\n' {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                    i += 1;
                }
                // Consume a following literal suffix (`r#"a"#r`) as part of
                // this literal, so its trailing `r` never re-enters the
                // raw branch with `#` before it.
                while i < chars.len() && ident_continue(chars[i]) {
                    out.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            // Raw identifier `r#ident` (possibly multi-`#`): consume `r` +
            // hashes + identifier as one token, so the identifier's
            // trailing `r` (as in `r#r`) never re-enters the raw branch.
            // Only bare `r` starts a raw identifier; `br`/`cr` + `#` is an
            // identifier plus `#` and correctly falls through.
            if hashes >= 1 && chars.get(j).is_some_and(|&d| ident_start(d)) {
                let mut run_start = i;
                while run_start > 0 && ident_continue(chars[run_start - 1]) {
                    run_start -= 1;
                }
                let run: String = chars[run_start..=i].iter().collect();
                if run == "r" {
                    while i < j {
                        out.push(chars[i]);
                        i += 1;
                    }
                    while i < chars.len() && ident_continue(chars[i]) {
                        out.push(chars[i]);
                        i += 1;
                    }
                    continue;
                }
            }
        }
        if c == '/' && next == Some('/') {
            out.push(' ');
            out.push(' ');
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            out.push(' ');
            out.push(' ');
            i += 2;
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                let d = chars[i];
                let dn = chars.get(i + 1).copied();
                if d == '/' && dn == Some('*') {
                    depth += 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if d == '*' && dn == Some('/') {
                    depth -= 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if d == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            continue;
        }
        if c == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() {
                let d = chars[i];
                if d == '\\' {
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if d == '"' {
                    out.push('"');
                    i += 1;
                    break;
                }
                if d == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            // Consume a following literal suffix (`"a"e<U+203F>r`) as part
            // of this literal, so its trailing `r` never re-enters the raw
            // branch.
            while i < chars.len() && ident_continue(chars[i]) {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if c == '\'' {
            // Char literal on one line: blank its body so a `?` inside
            // cannot satisfy the scan. Non-escape bodies are exactly one
            // character (quotes exactly 2 apart); a 2-3 char body is
            // never a valid char, only a lifetime/label quote reaching
            // a later quote (e.g. `break 'a "'...'"` spans exactly 4).
            // Escape-led bodies (`'\..'`) are accepted at any width
            // because lifetimes never contain a backslash. Lifetimes
            // and labels fall through as plain code.
            let mut j = i + 1;
            let mut closed = false;
            while j < chars.len() && chars[j] != '\n' {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == '\'' {
                    closed = true;
                    break;
                }
                j += 1;
            }
            let escape_led = chars.get(i + 1).copied() == Some('\\');
            if closed && (j.saturating_sub(i) == 2 || escape_led) {
                // Blank via the escape-aware lookahead endpoint `j`:
                // rescanning for the first `'` would stop at an escaped
                // quote (`'\''`) and reinterpret the real closer as a new
                // char start, leaking a following string as code.
                out.push('\'');
                i += 1;
                while i < j {
                    if chars[i] == '\n' {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                    i += 1;
                }
                out.push('\'');
                i += 1;
                // Consume a following literal suffix as part of this char
                // literal, mirroring the string branches.
                while i < chars.len() && ident_continue(chars[i]) {
                    out.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Byte offsets of every whole-word occurrence of `word` in `haystack`:
/// the neighbouring characters (if any) must not continue an identifier,
/// so `my_getrandom_x` never matches but `::getrandom`, `r#getrandom` and
/// a bare `getrandom` in a `use` all do.
fn word_indices(haystack: &str, word: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = haystack.char_indices().collect();
    let want: Vec<char> = word.chars().collect();
    let mut out = Vec::new();
    if want.is_empty() || chars.len() < want.len() {
        return out;
    }
    for i in 0..=chars.len() - want.len() {
        if (0..want.len()).all(|k| chars[i + k].1 == want[k]) {
            let before_ok = i == 0 || !ident_continue(chars[i - 1].1);
            let after = i + want.len();
            let after_ok = after >= chars.len() || !ident_continue(chars[after].1);
            if before_ok && after_ok {
                out.push(chars[i].0);
            }
        }
    }
    out
}

/// Whole-word `getrandom` tokens in the stripped code of `source`.
fn getrandom_word_count(source: &str) -> usize {
    word_indices(&strip_code_for_scan(source), "getrandom").len()
}

/// Statement anchors for the offender scan: `getrandom` itself plus every
/// alias a `use`/`extern crate` binds to it in this source (`use getrandom
/// as gr;`, `use getrandom::fill as f;`, a bare `use getrandom::fill;`).
fn getrandom_anchors(stripped: &str) -> Vec<String> {
    let mut anchors = vec!["getrandom".to_string()];
    for idx in word_indices(stripped, "getrandom") {
        // Only an import binds an alias: the statement prefix before this
        // occurrence (back to the previous `;`, `{` or `}`) must carry
        // `use` or `extern`, so an unrelated `as` cast in a call statement
        // never mints an anchor.
        let stmt_start = stripped[..idx].rfind([';', '{', '}']).map_or(0, |p| p + 1);
        let prefix = &stripped[stmt_start..idx];
        if word_indices(prefix, "use").is_empty() && word_indices(prefix, "extern").is_empty() {
            continue;
        }
        let rest = &stripped[idx + "getrandom".len()..];
        let seg_end = rest.find([';', ',', '}']).unwrap_or(rest.len());
        let seg = &rest[..seg_end];
        if let Some(as_at) = word_indices(seg, "as").into_iter().next() {
            let alias: String = seg[as_at + 2..]
                .chars()
                .skip_while(|c| !ident_start(*c))
                .take_while(|c| ident_continue(*c))
                .collect();
            if !alias.is_empty() && !anchors.contains(&alias) {
                anchors.push(alias);
            }
        } else {
            // `use getrandom::fill;` imports the function under its own name.
            let flat: String = seg.chars().filter(|c| !c.is_whitespace()).collect();
            if flat == "::fill" && !anchors.iter().any(|a| a == "fill") {
                anchors.push("fill".to_string());
            }
        }
    }
    anchors
}

/// Scan `source` for `getrandom` statements whose text (from the anchor
/// word up to the terminating `;`) contains any of `banned`. Anchors are
/// the bare `getrandom` word plus any alias bound to it, located on the
/// stripped text (comments and string contents blanked), so a crate
/// alias or a comment/whitespace-separated path cannot evade the scan
/// and a `;` inside a string cannot end the window early. Returns one
/// `label:line: <statement>` string per offending statement, in source
/// order, deduplicated by statement terminator. Prose about the old
/// code cannot false-positive.
fn getrandom_offenders(label: &str, source: &str, banned: &[&str]) -> Vec<String> {
    let stripped = strip_code_for_scan(source);
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    for anchor in getrandom_anchors(&stripped) {
        for idx in word_indices(&stripped, &anchor) {
            let end = stripped[idx..]
                .find(';')
                .map_or(stripped.len(), |semi| idx + semi + 1);
            let statement = &stripped[idx..end];
            if banned.iter().any(|b| statement.contains(b)) {
                let line = stripped[..idx].matches('\n').count() + 1;
                let flat: String = statement.split_whitespace().collect::<Vec<_>>().join(" ");
                found.push((idx, end, format!("{label}:{line}: {flat}")));
            }
        }
    }
    found.sort_by_key(|(idx, _, _)| *idx);
    let mut seen_ends = std::collections::BTreeSet::new();
    found
        .into_iter()
        .filter(|(_, end, _)| seen_ends.insert(*end))
        .map(|(_, _, entry)| entry)
        .collect()
}

fn named_file_offenders(banned: &[&str]) -> Vec<String> {
    NAMED_FILES
        .iter()
        .flat_map(|rel| getrandom_offenders(rel, &read_src(rel), banned))
        .collect()
}

#[test]
fn nine_named_files_no_expect_on_getrandom() {
    let offenders = named_file_offenders(EXPECT_BAN);
    assert!(
        offenders.is_empty(),
        "randomness failure must fail closed (propagate the error), not \
         panic via .expect(..); offending getrandom statements:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn nine_named_files_no_unwrap_on_getrandom() {
    let offenders = named_file_offenders(UNWRAP_BAN);
    assert!(
        offenders.is_empty(),
        "randomness failure must propagate, not panic or fall back via \
         unwrap*; offending getrandom statements:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn crate_wide_no_expect_on_getrandom_including_ledger() {
    // Catches the nine fills being moved behind a shared helper that
    // still panics: the whole crate is scanned, ledger.rs included —
    // no file is excluded by name.
    let mut files = Vec::new();
    collect_rs_files(&src_dir(), &mut files);
    assert!(
        files
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == "ledger.rs")),
        "crate-wide scan must cover ledger.rs (no exclusion)"
    );
    let mut offenders = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let label = path
            .strip_prefix(src_dir())
            .unwrap_or(&path)
            .display()
            .to_string();
        offenders.extend(getrandom_offenders(&label, &source, EXPECT_BAN));
    }
    assert!(
        offenders.is_empty(),
        "no getrandom statement anywhere in the crate — ledger.rs \
         included, no exclusions — may .expect(..); offending \
         statements:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn crate_still_uses_getrandom() {
    // Anti-vacuity: removing the randomness calls instead of propagating
    // their errors must not pass.
    let mut files = Vec::new();
    collect_rs_files(&src_dir(), &mut files);
    let uses = files.iter().any(|path| {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        getrandom_word_count(&source) > 0
    });
    assert!(uses, "crate must still draw randomness via getrandom");
}

// --- Scanner self-tests: pin the scan against synthetic sources so the
// --- suite's GREEN shape is proven without touching production code. ---

#[test]
fn scanner_flags_same_line_expect() {
    let src = r#"getrandom::fill(&mut b).expect("secure randomness unavailable");"#;
    let offenders = getrandom_offenders("x.rs", src, EXPECT_BAN);
    assert_eq!(offenders.len(), 1);
    assert!(offenders[0].starts_with("x.rs:1:"), "{offenders:?}");
    // Spelling-insensitive families a raw `getrandom::` needle misses:
    // a crate alias, an imported fill, a block comment or whitespace
    // around the path separator, and a `;` inside a string before the
    // banned combinator.
    for src in [
        "use getrandom as gr;\ngr::fill(&mut b).expect(\"probe\");",
        "use getrandom::fill as f;\nf(&mut b).expect(\"probe\");",
        "use getrandom::fill;\nfill(&mut b).expect(\"probe\");",
        "getrandom /* entropy */ ::fill(&mut b).expect(\"probe\");",
        "getrandom :: fill (&mut b).expect(\"probe\");",
        "::getrandom::fill(&mut b).expect(\"probe\");",
        "getrandom::fill(&mut b).map_err(|_| (\";\", E)).expect(\"probe\");",
    ] {
        assert_eq!(
            getrandom_offenders("x.rs", src, EXPECT_BAN).len(),
            1,
            "must flag: {src}"
        );
    }
}

#[test]
fn scanner_flags_multiline_expect() {
    let src = "getrandom::fill(&mut b)\n    .expect(\"secure randomness unavailable\");";
    assert_eq!(getrandom_offenders("x.rs", src, EXPECT_BAN).len(), 1);
}

#[test]
fn scanner_accepts_question_mark_propagation() {
    let src = "getrandom::fill(&mut b)?;";
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
    assert!(getrandom_offenders("x.rs", src, UNWRAP_BAN).is_empty());
}

#[test]
fn scanner_accepts_map_err_propagation() {
    let src = "getrandom::fill(&mut b).map_err(|_| Error::RandomUnavailable)?;";
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
    assert!(getrandom_offenders("x.rs", src, UNWRAP_BAN).is_empty());
}

#[test]
fn scanner_flags_unwrap_variants() {
    for src in [
        "getrandom::fill(&mut b).unwrap();",
        "getrandom::fill(&mut b).unwrap_or_default();",
        "getrandom::fill(&mut b).unwrap_or_else(|_| ());",
    ] {
        assert_eq!(
            getrandom_offenders("x.rs", src, UNWRAP_BAN).len(),
            1,
            "must flag: {src}"
        );
    }
}

#[test]
fn scanner_ignores_comment_lines() {
    let src = "// previously: getrandom::fill(&mut b).expect(\"gone\");\ngetrandom::fill(&mut b)?;";
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
    // Block-comment and string-carried spellings are prose, not code.
    for src in [
        "/* getrandom::fill(&mut b).expect(\"gone\"); */\ngetrandom::fill(&mut b)?;",
        "let s = \"getrandom::fill(&mut b).expect(\\\"gone\\\")\";",
        "let s = \"use getrandom as gr; gr::fill(&mut b).expect(\\\"gone\\\")\";",
    ] {
        assert!(
            getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty(),
            "must ignore non-code: {src}"
        );
    }
}

#[test]
fn scanner_stops_at_statement_semicolon() {
    // An .expect(..) in the NEXT statement is not this call's panic path.
    let src = "getrandom::fill(&mut b)?;\nlet s = other().expect(\"unrelated\");";
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
}
