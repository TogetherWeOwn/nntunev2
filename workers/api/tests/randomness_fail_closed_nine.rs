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
//! never charged to a `getrandom` call.
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

/// Scan `source` for `getrandom::` statements whose text (from the call
/// up to the terminating `;`) contains any of `banned`. Returns one
/// `label:line: <statement>` string per offending statement.
///
/// Occurrences on `//` comment lines are ignored, so prose about the old
/// code cannot false-positive.
fn getrandom_offenders(label: &str, source: &str, banned: &[&str]) -> Vec<String> {
    let mut offenders = Vec::new();
    for (idx, _) in source.match_indices("getrandom::") {
        // Skip comment lines: text from the preceding newline to the
        // occurrence, trimmed, starts with `//`.
        let line_start = source[..idx].rfind('\n').map_or(0, |nl| nl + 1);
        if source[line_start..idx].trim_start().starts_with("//") {
            continue;
        }
        let end = source[idx..]
            .find(';')
            .map_or(source.len(), |semi| idx + semi + 1);
        let statement = &source[idx..end];
        if banned.iter().any(|b| statement.contains(b)) {
            let line = source[..idx].matches('\n').count() + 1;
            let flat: String = statement.split_whitespace().collect::<Vec<_>>().join(" ");
            offenders.push(format!("{label}:{line}: {flat}"));
        }
    }
    offenders
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
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
            .contains("getrandom::")
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
}

#[test]
fn scanner_stops_at_statement_semicolon() {
    // An .expect(..) in the NEXT statement is not this call's panic path.
    let src = "getrandom::fill(&mut b)?;\nlet s = other().expect(\"unrelated\");";
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
}
