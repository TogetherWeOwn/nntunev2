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
//! Note: a bare `":?"` substring ban misses the alternate spellings (`:#?`
//! puts `#` between `:` and `?`), so both `":?"` and `"#?"` are banned.
//! Format params (fill/align/width/precision/hex, e.g. `{e:x?}`, `{e:10?}`)
//! can also sit between `:` and `?`, so `"?}"` is banned as well — the Debug
//! `?` is always the final char of a format spec, covering every spelling.
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

#[test]
fn commit_context_uses_no_debug_format_specifier() {
    let context = commit_context();
    for spec in [":?", "#?", "?}"] {
        assert!(
            !context.contains(spec),
            "commit failure path must not use the Debug format specifier \
             (`{{:?}}`, `{{e:?}}`, `{{:#?}}`, `{{e:#?}}`, plus param variants \
             like `{{e:x?}}`/`{{e:10?}}`, all render the server \
             message/detail via tokio_postgres::Error Debug) — found `{spec}` \
             in:\n{context}"
        );
    }
}
