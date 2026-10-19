//! Acceptance tests: admission-ledger commit failures log SQLSTATE only.
//!
//! Contract: when the `mark_queued` commit (`PgAdmissionLedger` in
//! `src/lib.rs`) fails with a `tokio_postgres::Error`, the log line carries
//! the SQLSTATE code (`error.as_db_error().map(|db| db.code().code())`,
//! `"none"` for non-DB errors) and NEVER the error's Debug/Display rendering
//! or its message/detail/hint — those can embed server detail (constraint
//! names, key values, connection context). This mirrors the sibling
//! precedent in `src/queue_consumer.rs` (library upsert).
//!
//! The commit path is wasm/Hyperdrive-only, so it cannot execute on the
//! host; the pin is therefore source-shaped, scoped to the `mark_queued`
//! body (plus any one-level helper it calls), sliced from production
//! (pre-`#[cfg(test)]`) source so test literals cannot satisfy it.

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
/// defined in production `lib.rs`/`library.rs` (so an extracted
/// `pg_sqlstate(..)` helper satisfies the pin exactly like inline code).
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
fn sibling_precedent_still_logs_sqlstate_only() {
    // Positive control / anchor: the pattern the commit path must mirror.
    // If this fails, the precedent moved and this file needs re-anchoring,
    // not silent weakening.
    let consumer = production(include_str!("../src/queue_consumer.rs"));
    assert!(
        consumer.contains("error.as_db_error().map(|db| db.code().code())"),
        "queue_consumer.rs must keep the SQLSTATE extraction precedent"
    );
    assert!(
        consumer.contains("sqlstate={}") && consumer.contains("unwrap_or(\"none\")"),
        "queue_consumer.rs must keep logging sqlstate= with a \"none\" fallback"
    );
}

#[test]
fn commit_failure_is_still_detail_free_transport_error() {
    // Positive control (green before and after): the caller-facing error
    // stays the stable, detail-free string; only the LOG changes.
    let body = mark_queued_body();
    assert!(
        body.contains("MeterWriteError::Transport(\"admission ledger unavailable\".to_string())"),
        "commit failure must keep mapping to the detail-free transport error"
    );
    assert!(
        body.contains("admission_mark_queued_sql()"),
        "slice anchor: mark_queued must run the commit statement"
    );
}

#[test]
fn commit_failure_log_carries_the_sqlstate_code() {
    let context = commit_context();
    assert!(
        context.contains("as_db_error()"),
        "commit failure path must extract the DbError via as_db_error() \
         (queue_consumer.rs precedent), got:\n{context}"
    );
    assert!(
        context.contains(".code().code()"),
        "commit failure path must read the SQLSTATE via .code().code(), got:\n{context}"
    );
    assert!(
        context.contains("sqlstate="),
        "commit failure log line must carry `sqlstate=` like the sibling \
         precedent, got:\n{context}"
    );
    assert!(
        context.contains("unwrap_or(\"none\")"),
        "non-DB errors (no SQLSTATE) must log the \"none\" placeholder, \
         never panic or fall back to the raw error, got:\n{context}"
    );
}

#[test]
fn commit_failure_log_never_renders_the_raw_error() {
    let context = commit_context();
    assert!(
        !context.contains("{:?}"),
        "commit failure path must not Debug-format anything: the Debug of \
         tokio_postgres::Error embeds the server message/detail, got:\n{context}"
    );
    for leak in [".message()", ".detail()", ".hint()"] {
        assert!(
            !context.contains(leak),
            "commit failure path must not read DbError {leak} — SQLSTATE \
             code only, got:\n{context}"
        );
    }
    // Display leaks the same server message as Debug: no console_log! in
    // the commit context may interpolate the raw error binding itself.
    for (i, _) in context.match_indices("console_log!") {
        let stmt = context[i..].split(");").next().unwrap_or(&context[i..]);
        for raw in [
            "{e}",
            "{error}",
            ", e)",
            ", error)",
            "e.to_string()",
            "error.to_string()",
        ] {
            assert!(
                !stmt.contains(raw),
                "commit log must not render the raw error via `{raw}` — \
                 SQLSTATE code only, got:\n{stmt}"
            );
        }
    }
    // The current (pre-fix) leaking line, pinned by name so a rename of the
    // binding cannot slip the same rendering past the generic checks above.
    assert!(
        !production(include_str!("../src/lib.rs")).contains("admission ledger commit failed: {:?}"),
        "the `admission ledger commit failed: {{:?}}` log must be replaced \
         by the SQLSTATE-only form"
    );
}

#[test]
fn commit_failure_still_logs_at_the_commit_site() {
    // The fix must keep a log at the failure site (dropping the log line
    // entirely would satisfy the leak bans but lose operability).
    let context = commit_context();
    let logs: Vec<&str> = context
        .match_indices("console_log!")
        .map(|(i, _)| &context[i..])
        .collect();
    assert!(
        logs.iter().any(|invocation| {
            let stmt = invocation.split(");").next().unwrap_or(invocation);
            stmt.contains("admission ledger commit failed") && stmt.contains("sqlstate={}")
        }),
        "mark_queued must keep an `admission ledger commit failed` \
         console_log! carrying sqlstate=, got:\n{context}"
    );
}
