//! Supplemental privacy guard for structured admission commit and upsert events.
//!
//! Inspect emit invocations, not just the retired console macros: JSON fields
//! can leak raw errors via Display, Debug, to_string(), or direct serialization.
//! Commit context includes one-level production helpers so extracting the
//! formatting into a helper cannot hide it. Synthetic negative fixtures cover
//! captured/positional formatting and prove these predicates are non-vacuous.
//!
//! Debug detection parses placeholders in normal/raw string literals. Specs
//! ending in `?` cover alternate, width, precision and hex forms. Literal
//! scoping excludes Rust's `?` operator, comments and escaped `{{`/`}}` braces.

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

/// `mark_queued` plus any one-level helpers it calls in production
/// `lib.rs`/`library.rs`. The whole method is covered to catch added raw logs.
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

fn assert_no_debug_format(context: &str) {
    for body in string_literal_bodies(context) {
        assert!(
            debug_placeholder(body).is_none(),
            "failure path must not Debug-format server details: {body}"
        );
    }
}

#[test]
fn failure_contexts_use_no_debug_format_specifier() {
    assert_no_debug_format(&commit_context());
    assert_no_debug_format(&upsert_context());
}

/// Complete emit/legacy-console invocations, including nested format! args.
/// Count parentheses outside literals; `);` inside a string or nested call
/// must not truncate a statement before a leaking positional argument.
fn log_statements(context: &str) -> Vec<&str> {
    let literals = string_literal_bodies(context);
    let mut out = Vec::new();
    for name in [
        "emit(",
        "console_log!(",
        "console_error!(",
        "format!(",
        "format_args!(",
    ] {
        for (start, _) in context.match_indices(name) {
            let open = start + name.len() - 1;
            let mut depth = 0usize;
            for (offset, ch) in context[open..].char_indices() {
                let i = open + offset;
                let in_literal = literals.iter().any(|body| {
                    let body_start = body.as_ptr() as usize - context.as_ptr() as usize;
                    (body_start..body_start + body.len()).contains(&i)
                });
                if in_literal {
                    continue;
                }
                if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    depth -= 1;
                    if depth == 0 {
                        out.push(&context[start..=i]);
                        break;
                    }
                }
            }
        }
    }
    out
}

fn upsert_context() -> String {
    let consumer = production(include_str!("../src/queue_consumer.rs"));
    let edge_impl = consumer
        .split_once("impl MatchWriteRuntime for EdgeMatchWrite<'_>")
        .expect("edge upsert impl must exist")
        .1;
    fn_body(edge_impl, "upsert").expect("edge upsert body must exist")
}

/// `statement` with every `admission_commit_sqlstate(&e)` / `(&error)` call
/// replaced by a space. That call is the only sanctioned use of the raw
/// error binding inside a commit-site log; any bare `e`/`error` left is a
/// Display render of the server message.
fn scrub_sqlstate_calls(statement: &str) -> String {
    const HELPER: &str = "admission_commit_sqlstate";
    let mut out = String::with_capacity(statement.len());
    let mut rest = statement;
    while let Some(i) = rest.find(HELPER) {
        out.push_str(&rest[..i]);
        let after = &rest[i + HELPER.len()..];
        match sqlstate_call_len(after).filter(|_| !prev_is_ident(rest.as_bytes(), i)) {
            Some(len) => {
                out.push(' ');
                rest = &after[len..];
            }
            None => {
                // Not the sanctioned call shape (e.g. the helper's own
                // definition); keep the name so the token scan still sees
                // whatever follows it.
                out.push_str(HELPER);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Length of `(&e)` / `(&error)` (whitespace-tolerant) at the start of `s`,
/// if that is what follows the helper name.
fn sqlstate_call_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if b.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if b.get(i) != Some(&b'&') {
        return None;
    }
    i += 1;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    if &s[start..i] != "e" && &s[start..i] != "error" {
        return None;
    }
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if b.get(i) != Some(&b')') {
        return None;
    }
    Some(i + 1)
}

/// Bare `e` / `error` identifier tokens in `statement`, if any. Matching on
/// whole tokens (not substrings) keeps `MeterWriteError`, `as_db_error`,
/// `sqlstate` and message words from tripping the pin, while catching
/// positional Display args (`, e` on its own line), mid-argument uses, and
/// captured Display placeholders (`{e}`, `{e:}`, `{error}`) inside the
/// format string.
fn raw_error_tokens(statement: &str) -> Vec<&str> {
    statement
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|tok| *tok == "e" || *tok == "error")
        .collect()
}

fn assert_no_raw_error(context: &str) {
    assert_no_debug_format(context);
    let compact: String = context.chars().filter(|c| !c.is_whitespace()).collect();
    for leak in [".message()", ".detail()", ".hint()"] {
        assert!(
            !compact.contains(leak),
            "server detail accessor {leak}: {context}"
        );
    }
    for raw in ["e.to_string()", "error.to_string()"] {
        assert!(
            !compact
                .match_indices(raw)
                .any(|(i, _)| !prev_is_ident(compact.as_bytes(), i)),
            "helper must not Display-format the raw error: {context}"
        );
    }
    let statements = log_statements(context);
    assert!(
        !statements.is_empty(),
        "privacy guard must inspect a real log invocation"
    );
    for statement in statements {
        let scrubbed = scrub_sqlstate_calls(statement);
        assert!(
            raw_error_tokens(&scrubbed).is_empty(),
            "log must not capture, format or serialize the raw error: {statement}"
        );
    }
}

#[test]
fn failure_logs_never_interpolate_the_raw_error() {
    for (context, event) in [
        (commit_context(), "ingest.admission.commit_failed"),
        (upsert_context(), "ingest.library.upsert_failed"),
    ] {
        assert!(
            log_statements(&context)
                .iter()
                .any(|stmt| stmt.starts_with("emit(") && stmt.contains(event)),
            "privacy guard must inspect the structured {event} invocation"
        );
        assert_no_raw_error(&context);
    }
}

#[test]
fn structured_emit_guards_reject_captured_and_positional_error_mutants() {
    for event in [
        "ingest.admission.commit_failed",
        "ingest.library.upsert_failed",
    ] {
        for value in [
            "format!(\"{e:?}\")",
            "format!(\"{error:#?}\")",
            "format!(\"{:?}\", e)",
            "format!(\"{:#?}\", error)",
            "format!(\"{error:x?}\")",
            "format!(\"{e:10? }\")",
            "format!(r#\"{error:#?}\"#)",
            "format!(\"{e}\")",
            "format!(\"{error:}\")",
            "format!(\"{}\",\n e\n)",
            "format!(\"{0}\", error)",
            "format!(r#\"{e}\"#)",
            "format!(\"benign ); {}\", e)",
            "e.to_string()",
            "error.to_string()",
            "e",
            "error",
            "error.as_db_error().unwrap().message()",
            "error.as_db_error().unwrap().detail()",
            "error.as_db_error().unwrap().hint()",
        ] {
            let mutant = format!(
                "emit(Level::Error, trace_id, {event:?}, serde_json::json!({{\"build_sha\": build_sha, \"sqlstate\": {value}}}));"
            );
            assert!(
                std::panic::catch_unwind(|| assert_no_raw_error(&mutant)).is_err(),
                "privacy guard accepted leaked error: {mutant}"
            );
        }
    }
}

#[test]
fn structured_emit_guards_accept_only_sanitized_error_uses() {
    let commit = r#"emit(Level::Error, trace_id, "ingest.admission.commit_failed",
        serde_json::json!({"build_sha": build_sha, "sqlstate": admission_commit_sqlstate(&e)}));"#;
    let upsert = r#"crate::emit(nntune_logging::Level::Error, trace_id, "ingest.library.upsert_failed",
        serde_json::json!({"build_sha": build_sha, "sqlstate": sqlstate.unwrap_or("none")}));"#;
    assert_eq!(log_statements(commit).len(), 1);
    assert_eq!(log_statements(upsert).len(), 1);
    assert_no_raw_error(commit);
    assert_no_raw_error(upsert);
    assert_no_debug_format(r#"bar()?; let literal = "{{error:?}}"; let raw = r"{{e:#?}}";"#);
    assert!(std::panic::catch_unwind(|| assert_no_raw_error("no log invocation")).is_err());
}

#[test]
fn structured_emit_guard_cannot_scrub_a_second_raw_error_argument() {
    let mutant = r#"emit(Level::Error, trace_id, "ingest.admission.commit_failed",
        serde_json::json!({"build_sha": build_sha,
            "sqlstate": admission_commit_sqlstate(&e), "extra": format!("{e}")}));"#;
    assert!(std::panic::catch_unwind(|| assert_no_raw_error(mutant)).is_err());
    let disguised_helper = mutant
        .replace(
            "admission_commit_sqlstate",
            "unsafe_admission_commit_sqlstate",
        )
        .replace(", \"extra\": format!(\"{e}\")", "");
    assert!(std::panic::catch_unwind(|| assert_no_raw_error(&disguised_helper)).is_err());
    for rendering in [
        "format!(\"{error:#?}\")",
        "format!(\"{error}\")",
        "format!(\"{}\", error)",
        "error.to_string()",
    ] {
        let helper_leak = format!(
            r#"emit(Level::Error, trace_id, "ingest.admission.commit_failed",
            serde_json::json!({{"sqlstate": admission_commit_sqlstate(&e)}}));
            fn admission_commit_sqlstate(error: &Error) -> String {{ {rendering} }}"#
        );
        assert!(std::panic::catch_unwind(|| assert_no_raw_error(&helper_leak)).is_err());
    }
}

#[test]
fn legacy_console_leaks_are_still_rejected() {
    for mutant in [
        r#"console_log!("{}", e);"#,
        r#"console_error!("{error}");"#,
        r#"console_log!("{e:#?}");"#,
    ] {
        assert!(std::panic::catch_unwind(|| assert_no_raw_error(mutant)).is_err());
    }
}
