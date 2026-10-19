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
//!   anti-vacuity half (`random.rs` must be in the found set).
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

/// One `label:line: <line>` string per non-comment line of `source`
/// containing `needle`. Comment lines (`//`, `//!`, `///`) are ignored.
fn code_lines_containing(label: &str, source: &str, needle: &str) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//") && line.contains(needle))
        .map(|(idx, line)| format!("{label}:{}: {}", idx + 1, line.trim()))
        .collect()
}

/// Scan `source` for `getrandom::` statements whose text (from the call
/// up to the terminating `;`) contains any of `banned`. Returns one
/// `label:line: <statement>` string per offending statement.
/// Occurrences on `//` comment lines are ignored.
fn getrandom_offenders(label: &str, source: &str, banned: &[&str]) -> Vec<String> {
    let mut offenders = Vec::new();
    for (idx, _) in source.match_indices("getrandom::") {
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
}

#[test]
fn scanner_ignores_comment_lines() {
    let src = "// previously: getrandom::fill(&mut b)?;\n//! getrandom::fill in docs\nlet b = crate::random::secure_random_bytes::<16>()?;";
    assert!(code_lines_containing("x.rs", src, "getrandom::").is_empty());
    assert!(getrandom_offenders("x.rs", src, EXPECT_BAN).is_empty());
}

#[test]
fn scanner_flags_expect_and_unwrap_on_getrandom() {
    for src in [
        r#"getrandom::fill(&mut b).expect("secure randomness unavailable");"#,
        "getrandom::fill(&mut b).unwrap();",
        "getrandom::fill(&mut b)\n    .unwrap_or_default();",
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
