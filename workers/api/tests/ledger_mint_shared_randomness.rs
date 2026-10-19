//! The ledger idempotency-key mint must route through the shared
//! randomness helper (tests-only).
//!
//! Contract under test — the paired build implements exactly this shape
//! in `workers/api/src/ledger.rs` and must not edit this file:
//!
//! ```ignore
//! pub fn new_idempotency_key() -> Result<String, LedgerError> {
//!     let bytes = crate::random::secure_random_bytes::<16>()
//!         .map_err(|_| LedgerError::RandomnessUnavailable)?;
//!     Ok(crate::b64::encode(&bytes))
//! }
//! ```
//!
//! That is the last direct `getrandom::fill` outside the helper: the nine
//! other request-serving sites already call
//! `crate::random::secure_random_bytes`, whose single audited
//! `getrandom::fill` lives in `workers/api/src/random.rs`. Routing the
//! mint through the helper changes no success bytes (same backend, same
//! 16 entropy bytes, same `crate::b64::encode`) and no failure shape
//! (`LedgerError::RandomnessUnavailable`, HTTP 503): the helper's
//! `CryptoError` is mapped onto the ledger variant at the call site.
//!
//! This suite is RED on the base tree (assertion failures naming
//! `ledger.rs` and its direct fill, not compile errors) and GREEN once
//! the mint routes through the helper. It pins:
//!
//! - the sole `getrandom::` code file under `src/` is `random.rs`
//!   (`sole_getrandom_code_file_in_src_is_random_rs`);
//! - `ledger.rs` contains no direct `getrandom::` at all
//!   (`ledger_rs_has_no_direct_getrandom`);
//! - the mint body calls `crate::random::secure_random_bytes::<16>()`,
//!   maps its error to `LedgerError::RandomnessUnavailable`, and keeps
//!   the single `Ok(crate::b64::encode(&bytes))` success path, with no
//!   panic/fallback token (`ledger_mint_routes_through_shared_helper`);
//! - the crate-wide no-expect/no-unwrap scan over `getrandom::`
//!   statements covers every `src/` file with no exclusion —
//!   `ledger.rs` included, and asserted present in the scanned set so
//!   the coverage cannot silently vacate
//!   (`crate_wide_no_expect_or_unwrap_on_getrandom_including_ledger`).
//!
//! Sensitivity (each defeats at least one pin):
//!
//! - keeping the direct `getrandom::fill` in `ledger.rs` fails the
//!   sole-site, ledger-direct and helper-routing pins;
//! - calling the helper with any other size (`::<32>()`), or via a
//!   renamed local wrapper in `ledger.rs`, fails the routing pin (and a
//!   wrapper hiding a fill fails the sole-site pin);
//! - dropping the `map_err(..)?` (e.g. `.expect(..)`, `.unwrap()`,
//!   `.unwrap_or_default()`, `let _ =`, encoding a zero buffer) fails the
//!   routing pin's exact propagation shape and its panic/fallback ban;
//! - relocating a direct fill into any other `src/` file fails the
//!   sole-site pin, which scans the whole tree rather than a file list;
//! - deleting the helper's own fill fails the sole-site pin's
//!   anti-vacuity half (`random.rs` must be in the found set);
//! - respelling a direct fill as a crate alias (`use getrandom as gr;`),
//!   an imported `fill`, a comment/whitespace-separated path
//!   (`getrandom /* entropy */ ::fill`) or a leading-`::` path still
//!   fails: the scanners strip comments/strings and anchor on the bare
//!   `getrandom` word token (plus aliases bound to it), never on the raw
//!   `getrandom::` text, and statement windows are located on the
//!   stripped text so a `;` inside a string cannot hide a banned
//!   combinator.
//!
//! Occurrences on `//` comment lines never count, so prose about the
//! old code cannot false-positive; the `scanner_*` self-tests pin that
//! behaviour against synthetic sources. Behavioural RNG-failure
//! injection is out of scope (not portable); the success-path behaviour
//! (22-char base64url, freshness, 503 variant shape) stays pinned by
//! `idempotency_randomness_fail_closed`.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/api/Cargo.toml \
//!     --test ledger_mint_shared_randomness
//! ```

use std::collections::BTreeSet;
use std::path::PathBuf;

const LEDGER_SRC: &str = include_str!("../src/ledger.rs");

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

fn read_path(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
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

fn rel_label(path: &std::path::Path) -> String {
    path.strip_prefix(src_dir())
        .unwrap_or(path)
        .display()
        .to_string()
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

/// One `label:line: <line>` string per code line of `source` containing
/// `needle`. Matching runs on the stripped text (comments and string
/// contents blanked, newlines preserved), so `//` comment lines never
/// count. For the `getrandom::` needle the bare `getrandom` word token
/// also counts, catching alias imports and comment/whitespace-separated
/// path spellings the raw needle misses.
fn code_lines_containing(label: &str, source: &str, needle: &str) -> Vec<String> {
    let stripped = strip_code_for_scan(source);
    stripped
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            line.contains(needle)
                || (needle == "getrandom::" && !word_indices(line, "getrandom").is_empty())
        })
        .map(|(idx, line)| format!("{label}:{}: {}", idx + 1, line.trim()))
        .collect()
}

/// Scan `source` for `getrandom` statements whose text (from the anchor
/// word up to the terminating `;`) contains any of `banned`. Anchors are
/// the bare `getrandom` word plus any alias bound to it, located on the
/// stripped text, so comment-separated paths are seen and a `;` inside a
/// string cannot end the window early. Returns one
/// `label:line: <statement>` string per offending statement, in source
/// order, deduplicated by statement terminator.
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
    let mut seen_ends = BTreeSet::new();
    found
        .into_iter()
        .filter(|(_, end, _)| seen_ends.insert(*end))
        .map(|(_, _, entry)| entry)
        .collect()
}

fn normalized(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The whitespace-normalized body of `new_idempotency_key`, from its
/// signature to its closing brace at column zero.
fn mint_body() -> String {
    let start = LEDGER_SRC
        .find("pub fn new_idempotency_key")
        .expect("ledger.rs must define pub fn new_idempotency_key");
    let rest = &LEDGER_SRC[start..];
    let end = rest
        .find("\n}\n")
        .expect("new_idempotency_key must end with a closing brace at column zero");
    normalized(&rest[..end])
}

// --- RED on the base tree: the shared-helper routing contract ----------------

#[test]
fn sole_getrandom_code_file_in_src_is_random_rs() {
    let mut files = Vec::new();
    collect_rs_files(&src_dir(), &mut files);
    assert!(!files.is_empty(), "src/ scan must find .rs files");
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let label = rel_label(path);
        let lines = code_lines_containing(&label, &read_path(path), "getrandom::");
        if !lines.is_empty() {
            found.insert(label);
            hits.extend(lines);
        }
    }
    assert_eq!(
        found,
        BTreeSet::from(["random.rs".to_string()]),
        "the sole getrandom:: code file under src/ must be random.rs (the \
         shared helper); every other site routes through \
         crate::random::secure_random_bytes. Hits:\n{}",
        hits.join("\n")
    );
}

#[test]
fn ledger_rs_has_no_direct_getrandom() {
    let hits = code_lines_containing("ledger.rs", LEDGER_SRC, "getrandom::");
    assert!(
        hits.is_empty(),
        "ledger.rs must contain no direct getrandom:: use; the mint routes \
         through crate::random::secure_random_bytes::<16>(). Hits:\n{}",
        hits.join("\n")
    );
}

#[test]
fn ledger_mint_routes_through_shared_helper() {
    let body = mint_body();
    assert!(
        body.contains(
            "crate::random::secure_random_bytes::<16>() \
             .map_err(|_| LedgerError::RandomnessUnavailable)?;"
        ),
        "entropy must come from the shared helper at exactly 16 bytes and \
         its failure must propagate as LedgerError::RandomnessUnavailable \
         via map_err(...)?, body was: {body}"
    );
    assert_eq!(
        body.matches("secure_random_bytes").count(),
        1,
        "the mint must draw entropy exactly once, body was: {body}"
    );
    assert!(
        body.contains("Ok(crate::b64::encode(&bytes))"),
        "success path must stay byte-identical: Ok(crate::b64::encode(&bytes)), \
         body was: {body}"
    );
    assert_eq!(
        body.matches("Ok(").count(),
        1,
        "exactly one Ok( in the mint body: any fallback Ok( means a key was \
         minted without entropy, body was: {body}"
    );
    assert_eq!(
        getrandom_word_count(&body),
        0,
        "mint body must contain no getrandom token under any spelling \
         (alias import, comment-separated path, leading `::`), body was: {body}"
    );
    for banned in [
        "getrandom::",
        "[0u8;",
        ".expect(",
        ".unwrap(",
        ".unwrap_or",
        "panic!",
        "todo!",
        "unimplemented!",
        "OnceLock",
        "static ",
    ] {
        assert!(
            !body.contains(banned),
            "mint body must never bypass the helper, panic, or fall back: \
             found `{banned}` in: {body}"
        );
    }
}

// --- Coverage pin: no file is excluded from the crate-wide scan -------------

#[test]
fn crate_wide_no_expect_or_unwrap_on_getrandom_including_ledger() {
    let mut files = Vec::new();
    collect_rs_files(&src_dir(), &mut files);
    let scanned: BTreeSet<String> = files.iter().map(|p| rel_label(p)).collect();
    assert!(
        scanned.contains("ledger.rs"),
        "crate-wide scan must cover ledger.rs (no exclusion); scanned: {scanned:?}"
    );
    assert!(
        scanned.contains("random.rs"),
        "crate-wide scan must cover random.rs; scanned: {scanned:?}"
    );
    let banned: Vec<&str> = EXPECT_BAN.iter().chain(UNWRAP_BAN).copied().collect();
    let mut offenders = Vec::new();
    for path in &files {
        offenders.extend(getrandom_offenders(
            &rel_label(path),
            &read_path(path),
            &banned,
        ));
    }
    assert!(
        offenders.is_empty(),
        "no getrandom statement anywhere in the crate — ledger.rs included, \
         no exclusions — may .expect(..) or unwrap*; offending statements:\n{}",
        offenders.join("\n")
    );
}

// --- Scanner self-tests: pin the scan against synthetic sources --------------

#[test]
fn scanner_flags_direct_getrandom_line() {
    let src = "    getrandom::fill(&mut bytes).map_err(|_| E)?;";
    let hits = code_lines_containing("x.rs", src, "getrandom::");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].starts_with("x.rs:1:"), "{hits:?}");
    // Spelling-insensitive families a raw `getrandom::` needle misses:
    // a crate alias, a block comment around the path separator, leading
    // `::`, and plain whitespace around `::`.
    for src in [
        "    use getrandom as gr;\n    gr::fill(&mut b).expect(\"probe\");",
        "    getrandom /* entropy */ ::fill(&mut b).expect(\"probe\");",
        "    ::getrandom::fill(&mut b).expect(\"probe\");",
        "    getrandom :: fill (&mut b).expect(\"probe\");",
        "    use getrandom::fill;\n    fill(&mut b).expect(\"probe\");",
    ] {
        assert!(
            !code_lines_containing("x.rs", src, "getrandom::").is_empty(),
            "must flag a getrandom line in: {src}"
        );
    }
}

#[test]
fn scanner_ignores_comment_lines() {
    let src = "// previously: getrandom::fill(&mut b)?;\n//! getrandom::fill in docs\nlet b = crate::random::secure_random_bytes::<16>()?;";
    assert!(code_lines_containing("x.rs", src, "getrandom::").is_empty());
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
    // Block-comment and string-carried spellings are prose, not code.
    for src in [
        "/* getrandom::fill(&mut b).expect(\"gone\"); */\nlet b = 1;",
        "let s = \"getrandom::fill(&mut b).expect(\\\"gone\\\")\";",
        "let s = \"use getrandom as gr; gr::fill(&mut b).expect(\\\"gone\\\")\";",
    ] {
        assert!(
            code_lines_containing("x.rs", src, "getrandom::").is_empty(),
            "must ignore non-code: {src}"
        );
        assert!(
            getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty(),
            "must ignore non-code: {src}"
        );
    }
}

#[test]
fn scanner_flags_expect_and_unwrap_on_getrandom() {
    for src in [
        r#"getrandom::fill(&mut b).expect("secure randomness unavailable");"#,
        "getrandom::fill(&mut b).unwrap();",
        "getrandom::fill(&mut b)\n    .unwrap_or_default();",
        // Alias and comment-separated mutants (fmt-clean in a real file).
        "use getrandom as gr;\ngr::fill(&mut b).expect(\"probe\");",
        "getrandom /* entropy */ ::fill(&mut b).expect(\"probe\");",
        "use getrandom::fill as f;\nf(&mut b).expect(\"probe\");",
        "use getrandom::fill;\nfill(&mut b).expect(\"probe\");",
        // A `;` inside a string must not end the statement window early.
        "getrandom::fill(&mut b).map_err(|_| (\";\", E)).expect(\"probe\");",
    ] {
        let banned: Vec<&str> = EXPECT_BAN.iter().chain(UNWRAP_BAN).copied().collect();
        assert_eq!(
            getrandom_offenders("x.rs", src, &banned).len(),
            1,
            "must flag: {src}"
        );
    }
}

#[test]
fn scanner_accepts_map_err_propagation() {
    let src = "getrandom::fill(&mut b).map_err(|_| E::RandomnessUnavailable)?;";
    let banned: Vec<&str> = EXPECT_BAN.iter().chain(UNWRAP_BAN).copied().collect();
    assert!(getrandom_offenders("x.rs", src, &banned).is_empty());
}

#[test]
fn scanner_stops_at_statement_semicolon() {
    // An .expect(..) in the NEXT statement is not this call's panic path.
    let src = "getrandom::fill(&mut b)?;\nlet s = other().expect(\"unrelated\");";
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
}
