//! Independently owned profile-artifact chooser slice containment guards,
//! native and hermetic. These inspect source; they do not touch a database,
//! R2, grants, HTTP, sockets, clocks or the network. See
//! choose-slice-acceptance.md for the oracle and proof limits.
//!
//! Unlike the behavioral chooser suite, this file compiles and runs before
//! the seam exists: it imports nothing from the future module. The
//! chooser-purity fence activates when the seam file lands; until then that
//! one test passes with nothing to fence while the missing-seam RED
//! preflight (see the oracle doc) records the absent module.

use quote::ToTokens;
use std::path::Path;

// ---- No production caller of the new seam ----

fn has_seam_reference(stream: impl ToTokens) -> bool {
    // Conservative spelling guard, including aliases/imports; not a call-graph proof.
    // Bare `Exact`/`Fallback` are deliberately not listed: `Exact` already
    // occurs in production sources, and any production use of the match level
    // also names `MatchLevel` or the seam function below.
    stream
        .to_token_stream()
        .to_string()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| {
            matches!(
                word,
                "choose"
                    | "choose_profile_artifact"
                    | "MediaRow"
                    | "StreamProfile"
                    | "Opus64"
                    | "MatchLevel"
                    | "ChosenArtifact"
                    | "ChoiceMiss"
                    | "NextAction"
                    | "EmptyCandidateSet"
                    | "CrossProfileRefused"
                    | "NoWellFormedCandidate"
                    | "TranscodeThenRetry"
                    | "RepairCandidates"
            )
        })
}

fn inspect_src(path: &Path, seam_definition: &Path, stream_lib: &Path, checked: &mut usize) {
    for entry in std::fs::read_dir(path).expect("read production source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            inspect_src(&path, seam_definition, stream_lib, checked);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") || path == seam_definition {
            continue;
        }
        *checked += 1;
        let source = std::fs::read_to_string(&path).expect("read production Rust");
        let file = syn::parse_file(&source).expect("parse production Rust");
        for item in file.items {
            if path == stream_lib {
                if let syn::Item::Mod(module) = &item {
                    if module.ident == "choose" {
                        assert!(
                            module.content.is_none(),
                            "only an inert out-of-line export is permitted"
                        );
                        continue;
                    }
                }
            }
            assert!(
                !has_seam_reference(item),
                "chooser seam must have no production caller/import: {}",
                path.display()
            );
        }
    }
}

#[test]
fn no_production_source_enables_or_consumes_the_choose_seam() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let stream_lib = root.join("workers/stream/src/lib.rs");
    let seam_definition = root.join("workers/stream/src/choose.rs");
    let mut checked = 0;
    for group in ["workers", "crates"] {
        for package in std::fs::read_dir(root.join(group)).expect("package directory") {
            let src = package.expect("package entry").path().join("src");
            if src.is_dir() {
                inspect_src(&src, &seam_definition, &stream_lib, &mut checked);
            }
        }
    }
    assert!(checked > 0, "source scan must not pass vacuously");
}

// ---- Chooser-module purity: no DB/R2/grant/HTTP/socket/clock/async ----

/// Words that must never appear as code words in the pure chooser module.
/// Comments, doc prose, strings, chars and raw strings are stripped first,
/// so contract prose cannot trip the fence; expressions inside macros still
/// scan because only their string literals are stripped.
const CHOOSE_BANNED_IDENTS: &[&str] = &[
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "ToSocketAddrs",
    "lookup_host",
    "DnsResolver",
    "hickory",
    "trust_dns",
    "reqwest",
    "hyper",
    "http",
    "HttpRequest",
    "HttpResponse",
    "StatusCode",
    "connect_timeout",
    "fetch",
    "Fetch",
    "R2Bucket",
    "R2Object",
    "sqlx",
    "tokio_postgres",
    "postgres",
    "Postgres",
    "rusqlite",
    "hyperdrive",
    "Hyperdrive",
    "grant",
    "Grant",
    "worker",
    "wasm_bindgen",
    "wasm_bindgen_futures",
    "js_sys",
    "web_sys",
    "tokio",
    "async",
    "await",
    "SystemTime",
    "Instant",
    "File",
    "read_dir",
    "process",
    "Command",
    "env",
    "rand",
    "getrandom",
    "Rng",
    "thread_rng",
    "unsafe",
];

/// Replace comments, strings, chars and doc prose with spaces so the fence
/// scans code words only. Handles line comments, nested block comments,
/// cooked/byte strings with escapes, chars (not lifetimes) and raw strings
/// with any hash count. Stripped spans become spaces so adjacent words never
/// fuse. Dependency-free: the fenced crate has no AST visitor available.
fn push_spaces(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

// Consume a cooked ("..."/b"...") string starting at the quote at `i`.
fn consume_cooked(bytes: &[u8], i: &mut usize, out: &mut String) {
    let start = *i;
    *i += 1;
    while *i < bytes.len() {
        if bytes[*i] == b'\\' {
            *i += 2;
        } else if bytes[*i] == b'"' {
            *i += 1;
            break;
        } else {
            *i += 1;
        }
    }
    push_spaces(out, *i - start);
}

fn strip_prose(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if b == b'/' && next == Some(b'/') {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            push_spaces(&mut out, i - start);
        } else if b == b'/' && next == Some(b'*') {
            let start = i;
            let mut depth = 0usize;
            while i + 1 < bytes.len() {
                if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            push_spaces(&mut out, i - start);
        } else if b == b'"' || (b == b'b' && next == Some(b'"')) {
            if b == b'b' {
                out.push(' ');
                i += 1;
            }
            consume_cooked(bytes, &mut i, &mut out);
        } else if b == b'r' && is_raw_open(bytes, i) {
            let start = i;
            let mut j = i + 1;
            let mut hashes = 0usize;
            while bytes.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            j += 1; // Opening quote.
            i = j;
            while i < bytes.len() {
                if bytes[i] == b'"' && (0..hashes).all(|h| bytes.get(i + 1 + h) == Some(&b'#')) {
                    i += 1 + hashes;
                    break;
                }
                i += 1;
            }
            push_spaces(&mut out, i - start);
        } else if b == b'b' && next == Some(b'\'') && is_char_close(bytes, i + 1) {
            let end = char_end(bytes, i + 1);
            push_spaces(&mut out, end - i);
            i = end;
        } else if b == b'\'' && is_char_close(bytes, i) {
            let end = char_end(bytes, i);
            push_spaces(&mut out, end - i);
            i = end;
        } else {
            // Lifetimes, idents, keywords and punctuation scan as words;
            // non-ASCII bytes pass through as separators (no banned ident
            // is non-ASCII).
            out.push(b as char);
            i += 1;
        }
    }
    out
}

/// True when a raw-string opening (`r"` / `r#"...`) starts at `i`.
fn is_raw_open(bytes: &[u8], i: usize) -> bool {
    if bytes.get(i) != Some(&b'r') {
        return false;
    }
    let mut j = i + 1;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    bytes.get(j) == Some(&b'"')
}

/// True when the quote at `i` opens a char literal rather than a lifetime:
/// `'x'`, an escaped char, or a unicode escape with its closing quote.
fn is_char_close(bytes: &[u8], i: usize) -> bool {
    if bytes.get(i) != Some(&b'\'') {
        return false;
    }
    if bytes.get(i + 1) == Some(&b'\\') {
        if bytes.get(i + 2) == Some(&b'u') && bytes.get(i + 3) == Some(&b'{') {
            return bytes[i + 4..]
                .iter()
                .position(|b| *b == b'}')
                .is_some_and(|k| bytes.get(i + 4 + k + 1) == Some(&b'\''));
        }
        return bytes.get(i + 3) == Some(&b'\'');
    }
    bytes.get(i + 2) == Some(&b'\'')
}

/// End offset (exclusive) of the char literal whose quote is at `i`.
fn char_end(bytes: &[u8], i: usize) -> usize {
    if bytes.get(i + 1) == Some(&b'\\')
        && bytes.get(i + 2) == Some(&b'u')
        && bytes.get(i + 3) == Some(&b'{')
    {
        let k = bytes[i + 4..]
            .iter()
            .position(|b| *b == b'}')
            .expect("char escape checked by is_char_close");
        return i + 4 + k + 2;
    }
    if bytes.get(i + 1) == Some(&b'\\') {
        return i + 4;
    }
    i + 3
}

fn code_words(source: &str) -> Vec<String> {
    strip_prose(source)
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

fn banned_hits(source: &str, banned: &[&str]) -> Vec<String> {
    code_words(source)
        .into_iter()
        .filter(|name| banned.contains(&name.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[test]
fn choose_module_stays_pure_when_it_lands() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/choose.rs");
    if !path.is_file() {
        // Nothing to fence yet; the missing-seam RED preflight records the
        // absent module separately. This guard activates on the seam file.
        return;
    }
    let source = std::fs::read_to_string(&path).expect("read chooser module");
    assert_eq!(
        banned_hits(&source, CHOOSE_BANNED_IDENTS),
        Vec::<String>::new(),
        "pure chooser module must not touch DB/R2/grant/HTTP/socket/clock/async"
    );
}

#[test]
fn purity_fence_skips_prose_but_trips_on_code_idents() {
    let clean = r#"
        //! Pure profile-artifact choice. No grant is minted here, no R2 or
        //! HTTP fetch happens, and no database row is read: the caller
        //! supplies inert rows and the function returns an inert choice.
        use std::fmt::{Display, Formatter};
        /// Threshold docs may name the "process" and its "env" freely.
        pub fn choose(rows: &[u8]) -> bool {
            let note = "async grant fetch over http stays outside this module";
            !rows.is_empty() && !note.is_empty()
        }
    "#;
    assert_eq!(
        banned_hits(clean, CHOOSE_BANNED_IDENTS),
        Vec::<String>::new()
    );
    // The stripper hides every prose shape: nested block comments, raw and
    // byte strings, chars, lifetimes and doc attributes.
    for prose in [
        "/* grant /* nested fetch */ still comment */ fn f() {}",
        "fn f() { let _ = r#\"grant::verify\"#; }",
        "fn f() { let _ = b\"http bytes\"; }",
        "fn f(c: char) -> bool { c == 'v' }",
        "fn f<'a>(x: &'a str) -> &'a str { x }",
        "#[doc = \"R2 docs stay prose\"] fn f() {}",
        "/// sqlx docs stay prose\nfn f() {}",
    ] {
        assert_eq!(
            banned_hits(prose, CHOOSE_BANNED_IDENTS),
            Vec::<String>::new(),
            "prose must not trip the fence: {prose}"
        );
    }
    for (dirty, word) in [
        ("use sqlx::PgPool;", "sqlx"),
        ("fn f(r: &http::Request<()>) {}", "http"),
        ("fn f() { let _ = StatusCode::OK; }", "StatusCode"),
        ("async fn f() {}", "async"),
        ("fn f() { work().await; }", "await"),
        ("fn f(g: &Grant) {}", "Grant"),
        ("fn f() { let _ = grant::verify(); }", "grant"),
        ("fn f(b: &R2Bucket) {}", "R2Bucket"),
        ("fn f() { let _ = tokio::spawn(x); }", "tokio"),
        (
            "fn f() { let _ = std::time::SystemTime::now(); }",
            "SystemTime",
        ),
        ("fn f() { unsafe { core::hint::spin_loop() } }", "unsafe"),
        ("fn f() { let _ = std::env::var(\"X\"); }", "env"),
        // Comment-adjacent idents still scan: stripped spans become spaces,
        // so neither side fuses nor vanishes.
        ("fn f() { grant/**/::verify(); }", "grant"),
        ("fn f() {// http\n HttpRequest::get(); }", "HttpRequest"),
    ] {
        let hits = banned_hits(dirty, CHOOSE_BANNED_IDENTS);
        assert!(
            hits.iter().any(|hit| hit == word),
            "fence must trip on {word}: {dirty}"
        );
    }
}

// ---- Tracked acceptance files stay free of tracker IDs, UI paths and
// ---- private-IPv4 literals (the public-text lint enforces this in CI) ----

const ACCEPTANCE_FILES: &[&str] = &[
    "tests/choose_profile_acceptance.rs",
    "tests/choose_slice_guard_acceptance.rs",
    "tests/choose-slice-acceptance.md",
];

fn has_tracker_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(4).enumerate().any(|(i, w)| {
        w == b"TOG-".as_slice() && bytes.get(i + 4).is_some_and(|b| b.is_ascii_digit())
    })
}

fn has_relative_ui_path(text: &str) -> bool {
    // Mirrors the lint's /PREFIX/issues|agents|projects|approvals|runs rule.
    let mut rest = text;
    while let Some(i) = rest.find('/') {
        rest = &rest[i + 1..];
        let prefix: String = rest
            .chars()
            .take_while(|c| c.is_ascii_uppercase())
            .collect();
        if prefix.len() < 2 || prefix.len() > 6 {
            continue;
        }
        let after = &rest[prefix.len()..];
        let Some(section) = after.strip_prefix('/') else {
            continue;
        };
        for name in ["issues/", "agents/", "projects/", "approvals/", "runs/"] {
            if section.starts_with(name) {
                return true;
            }
        }
    }
    false
}

/// True when `tail` opens with `groups` dot-separated 1-3-digit groups
/// followed by a word boundary, mirroring the lint's trailing `\b`.
fn quad_tail_valid(tail: &str, groups: usize) -> bool {
    let mut rest = tail;
    for g in 0..groups {
        if g > 0 {
            rest = match rest.strip_prefix('.') {
                Some(after) => after,
                None => return false,
            };
        }
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || digits.len() > 3 {
            return false;
        }
        rest = &rest[digits.len()..];
    }
    !rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn quad_start_ok(text: &str, i: usize) -> bool {
    // Word boundary before the address, plus no fusing dot: version-like
    // "1.10.0.1" must not match at the inner "10.".
    !text[..i]
        .chars()
        .last()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn has_private_ipv4_literal(text: &str) -> bool {
    // Mirrors the lint's dotted-quad rule: full 10/8 and 192.168/16 quads,
    // full 172.16-31/12 quads, and the exact link-local metadata endpoint.
    // Family mentions (CIDR, octet tuples) never match. Detection patterns
    // are assembled from fragments so this scanner's own source stays
    // lint-clean.
    const METADATA: &str = concat!("169.254.", "169.254");
    for (i, _) in text.match_indices(METADATA) {
        if !quad_start_ok(text, i) {
            continue;
        }
        let after = text[i + METADATA.len()..].chars().next();
        if after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        return true;
    }
    const HOME_LAN: &str = concat!("192.168", ".");
    for (i, _) in text.match_indices(HOME_LAN) {
        if quad_start_ok(text, i) && quad_tail_valid(&text[i + HOME_LAN.len()..], 2) {
            return true;
        }
    }
    for (i, _) in text.match_indices("10.") {
        if quad_start_ok(text, i) && quad_tail_valid(&text[i + 3..], 3) {
            return true;
        }
    }
    for (i, _) in text.match_indices("172.") {
        if !quad_start_ok(text, i) {
            continue;
        }
        let second: String = text[i + 4..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let Ok(n) = second.parse::<u32>() else {
            continue;
        };
        if !(16..=31).contains(&n) {
            continue;
        }
        let rest = &text[i + 4 + second.len()..];
        if rest.starts_with('.') && quad_tail_valid(&rest[1..], 2) {
            return true;
        }
    }
    false
}

#[test]
fn acceptance_files_carry_no_tracker_ids_ui_paths_or_private_ipv4() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ACCEPTANCE_FILES {
        let text = std::fs::read_to_string(root.join(file)).expect("read acceptance file");
        assert!(!has_tracker_id(&text), "tracker ID leaked into {file}");
        assert!(!has_relative_ui_path(&text), "UI path leaked into {file}");
        assert!(
            !has_private_ipv4_literal(&text),
            "private IPv4 literal leaked into {file}"
        );
    }
}

#[test]
fn public_text_scanners_trip_on_known_shapes() {
    // Violation-shaped fixtures are assembled from fragments so this test's
    // own source stays lint-clean; each assembled string holds the exact
    // banned shape at runtime.
    assert!(has_tracker_id(&format!("see TOG-{} for context", 1)));
    assert!(!has_tracker_id("see the authoring card for context"));
    assert!(!has_tracker_id("TOG- alone and TOG-x are not IDs"));
    assert!(has_relative_ui_path(&format!(
        "(x/{}/issues/{}-{})",
        "AB", "AB", 12
    )));
    assert!(!has_relative_ui_path("(the authoring card)"));
    assert!(!has_relative_ui_path("a/b/c"));
    assert!(has_private_ipv4_literal(&format!("host 10.0.0.{} end", 5)));
    assert!(has_private_ipv4_literal(&format!(
        "host 192.168.1.{} end",
        1
    )));
    assert!(has_private_ipv4_literal(&format!(
        "host 172.16.0.{} end",
        9
    )));
    assert!(has_private_ipv4_literal(&format!(
        "host 172.31.255.{} end",
        1
    )));
    assert!(has_private_ipv4_literal(&format!(
        "host {}.{}.{}.{} end",
        169, 254, 169, 254
    )));
    assert!(!has_private_ipv4_literal("host 172.15.0.1 end"));
    assert!(!has_private_ipv4_literal("host 172.32.0.1 end"));
    assert!(!has_private_ipv4_literal("host 93.184.216.34 end"));
    assert!(!has_private_ipv4_literal("Ipv4Addr::new(10, 0, 0, 5)"));
    // Family mentions, CIDR ranges, versions and trailing-word runs are
    // not dotted-quad literals and must not match.
    assert!(!has_private_ipv4_literal("RFC 1918 172.16/12"));
    assert!(!has_private_ipv4_literal("RFC 1918 192.168/16"));
    assert!(!has_private_ipv4_literal("version 1.10.0.1 packaged"));
    assert!(!has_private_ipv4_literal("host 10.0.0.5x end"));
    assert!(!has_private_ipv4_literal("host 10.0.0.1234 end"));
}
