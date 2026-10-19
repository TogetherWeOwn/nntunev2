//! Builder-owned supplemental pin: no Debug-format specifier on the
//! admission-ledger commit path.
//!
//! The acceptance pin in `admission_commit_sqlstate_acceptance.rs` bans the
//! literal `"{:?}"` spelling; captured (`{e:?}`) and alternate (`{:#?}`,
//! `{e:#?}`) Debug spellings slip that ban while leaking the same server
//! message/detail/hint via `tokio_postgres::Error`'s Debug render. This file
//! bans the Debug format specifier itself in the commit context (the
//! `mark_queued` body plus any one-level helper it calls, sliced from
//! production pre-`#[cfg(test)]` source so test literals cannot satisfy it).
//!
//! Detection parses `{...}` placeholders inside string literals (normal and
//! raw) rather than banning substrings: a placeholder whose spec — the text
//! after `:` — ends with `?` (allowing whitespace before `}`, which rustc
//! accepts) is a Debug render, whatever fill/align/width/precision/`#`/hex
//! params precede the `?`. Scoping to literals keeps Rust's `?` operator
//! (`bar()?\n}`) and `{{`/`}}` escapes from tripping the pin.
//!
//! Slicing helpers below mirror the acceptance file's scoping; the
//! Test-Author file itself is intentionally untouched.

fn production(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .expect("source must have a test module boundary")
        .0
}

/// Text of `PgAdmissionLedger::mark_queued` in production `lib.rs`.
fn mark_queued_body() -> &'static str {
    let prod = production(include_str!("../src/lib.rs"));
    // Anchor on the Pg impl, not the trait declaration of the same name.
    let pg_impl = prod
        .split_once("impl AdmissionLedger for PgAdmissionLedger")
        .expect("PgAdmissionLedger impl must exist")
        .1;
    pg_impl
        .split_once("async fn mark_queued")
        .expect("PgAdmissionLedger::mark_queued must exist")
        .1
        .split_once("async fn release_claim")
        .expect("release_claim must bound mark_queued")
        .0
}

/// Brace-matched body of `fn <name>(...)` in `source`, if defined there.
fn fn_body(source: &str, name: &str) -> Option<String> {
    let start = source.find(&format!("fn {name}("))?;
    let open = source[start..].find('{')? + start;
    let mut depth = 0usize;
    for (i, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(source[open..=open + i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// `mark_queued` plus the body of any one-level helper it calls that is
/// defined in production `lib.rs`/`library.rs` (mirrors the acceptance
/// file's commit context exactly).
fn commit_context() -> String {
    let body = mark_queued_body();
    let mut context = body.to_string();
    let sources = [
        production(include_str!("../src/lib.rs")),
        production(include_str!("../src/library.rs")),
    ];
    // Candidate helper names: identifiers immediately followed by `(`.
    let mut names: Vec<&str> = Vec::new();
    for (i, _) in body.match_indices('(') {
        let name: String = body[..i]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if !name.is_empty() {
            // Leak is fine in a test; names come from 'static source.
            names.push(Box::leak(name.into_boxed_str()));
        }
    }
    for name in names {
        for source in sources {
            if let Some(helper) = fn_body(source, name) {
                context.push_str("\n// helper: ");
                context.push_str(name);
                context.push('\n');
                context.push_str(&helper);
            }
        }
    }
    context
}

/// Bodies of the normal (`"..."`) and raw (`r"..."`, `r#"..."#`) string
/// literals in `context`. Comments, char literals and byte strings are
/// skipped: none of them can carry a `format!`-family spec (format macros
/// require a string-literal format string, so every Debug spec lives in
/// one of the returned bodies).
fn string_literal_bodies(context: &str) -> Vec<&str> {
    let b = context.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // Line comment.
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment (nestable).
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1usize;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // Raw string `r"..."` / `r#"..."#` (byte form `br"..."` is skipped:
        // byte strings are never format input).
        if let Some((hashes, body_start, is_byte)) = raw_string_open(b, i) {
            match raw_string_end(b, body_start, hashes) {
                Some((body_end, next)) => {
                    if !is_byte {
                        out.push(&context[body_start..body_end]);
                    }
                    i = next;
                }
                None => break, // Unterminated; production compiles, so unreachable.
            }
            continue;
        }
        // Byte string `b"..."` (not formattable): skip without collecting.
        if b[i] == b'b' && b.get(i + 1) == Some(&b'"') && !prev_is_ident(b, i) {
            i = cooked_end(b, i + 2).1;
            continue;
        }
        // Normal string: collect the body, honouring `\` escapes.
        if b[i] == b'"' {
            let (body_end, next) = cooked_end(b, i + 1);
            out.push(&context[i + 1..body_end]);
            i = next;
            continue;
        }
        // Char literal (`'x'`, `'\n'`, `'\''`): skip so a `'"'` inside one
        // cannot open a string. Lifetimes (`'a`, `'static`) never match the
        // closing-quote shape, so they fall through harmlessly.
        if b[i] == b'\'' {
            if let Some(next) = char_end(b, i) {
                i = next;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i.checked_sub(1)
        .is_some_and(|p| b[p].is_ascii_alphanumeric() || b[p] == b'_')
}

/// If `b[i..]` opens a raw string, returns `(hashes, body_start, is_byte)`.
fn raw_string_open(b: &[u8], i: usize) -> Option<(usize, usize, bool)> {
    let (mut j, is_byte) = match b[i] {
        b'r' => (i + 1, false),
        b'b' if b.get(i + 1) == Some(&b'r') => (i + 2, true),
        _ => return None,
    };
    if prev_is_ident(b, i) {
        return None; // Identifier ending in r/br, not a string prefix.
    }
    let mut hashes = 0;
    while b.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    if b.get(j) == Some(&b'"') {
        Some((hashes, j + 1, is_byte))
    } else {
        None
    }
}

/// End of the raw-string body starting at `start`: `(body_end, next_i)`.
fn raw_string_end(b: &[u8], start: usize, hashes: usize) -> Option<(usize, usize)> {
    let mut j = start;
    while j < b.len() {
        if b[j] == b'"' {
            let mut k = j + 1;
            let mut n = 0;
            while n < hashes && b.get(k) == Some(&b'#') {
                n += 1;
                k += 1;
            }
            if n == hashes {
                return Some((j, k));
            }
        }
        j += 1;
    }
    None
}

/// `(body_end, next_i)` for the cooked string/byte-string body at `start`.
fn cooked_end(b: &[u8], start: usize) -> (usize, usize) {
    let mut j = start;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == b'"' {
            return (j, j + 1);
        }
        j += 1;
    }
    (b.len(), b.len())
}

/// If `b[i..]` is a char literal, returns the index just past it.
fn char_end(b: &[u8], i: usize) -> Option<usize> {
    let c1 = *b.get(i + 1)?;
    if c1 == b'\\' {
        // Escape: skip to the closing quote (covers `\n`, `\'`, `\\`, `\u{..}`).
        let mut j = i + 2;
        while j < b.len() && b[j] != b'\'' {
            j += 1;
        }
        return if j < b.len() { Some(j + 1) } else { None };
    }
    if b.get(i + 2) == Some(&b'\'') {
        return Some(i + 3);
    }
    None
}

/// The first Debug placeholder in a string-literal `body`, if any: `{...}`
/// whose spec (text after `:`) ends with `?`, allowing trailing whitespace
/// before `}`. The `?` trait selector is always last in a spec, so every
/// fill/align/width/precision/`#`/hex spelling is covered in one check.
fn debug_placeholder(body: &str) -> Option<&str> {
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if b.get(i + 1) == Some(&b'{') {
                i += 2; // Escaped `{{`.
                continue;
            }
            if let Some(len) = b[i + 1..].iter().position(|c| *c == b'}') {
                let placeholder = &body[i..i + 1 + len + 1];
                let inner = placeholder[1..placeholder.len() - 1].trim_end();
                if inner.contains(':') && inner.ends_with('?') {
                    return Some(placeholder);
                }
                i += 1 + len + 1;
                continue;
            }
            break; // Unclosed; production compiles, so unreachable.
        }
        i += 1;
    }
    None
}

#[test]
fn commit_context_uses_no_debug_format_specifier() {
    let context = commit_context();
    let mut offenders = Vec::new();
    for body in string_literal_bodies(&context) {
        if let Some(placeholder) = debug_placeholder(body) {
            offenders.push(format!("`{placeholder}` in {body:?}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "commit failure path must not use the Debug format specifier \
         (`{{:?}}`, `{{e:?}}`, `{{:#?}}`, `{{e:#?}}`, param variants like \
         `{{e:x?}}`/`{{e:10?}}`, and whitespace-padded forms like \
         `{{e:x? }}` — all render the server message/detail via \
         tokio_postgres::Error Debug) — found:\n{}\nin:\n{context}",
        offenders.join("\n")
    );
}
