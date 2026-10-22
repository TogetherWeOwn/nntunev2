//! Acceptance tests: admission commit and sibling upsert failures emit SQLSTATE only.
//!
//! These Hyperdrive paths cannot execute on the host. Source predicates inspect
//! the actual execute-error closures (not unrelated events elsewhere), plus
//! one-level SQLSTATE helpers. Synthetic mutants exercise the same predicates:
//! dropping the log/category or its trace/build context must fail the guard.
//! The paired logging-standard acceptance file remains independently protected.

fn production(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .expect("source must have a test module boundary")
        .0
}

fn mark_queued_body() -> &'static str {
    let prod = production(include_str!("../src/lib.rs"));
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

/// Match a call/body delimiter without counting comments or string contents.
fn balanced_end(source: &str, open: usize, left: u8, right: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            i += source[i..].find('\n').unwrap_or(bytes.len() - i);
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let mut comments = 1;
            i += 2;
            while i < bytes.len() && comments > 0 {
                if bytes[i..].starts_with(b"/*") {
                    comments += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    comments -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i] == b'"' {
            // A raw string's closing quote has the same hash suffix as its opener.
            let hashes = bytes[..i].iter().rev().take_while(|b| **b == b'#').count();
            let raw = i > hashes && bytes[i - hashes - 1] == b'r';
            i += 1;
            if raw {
                let suffix = format!("\"{}", "#".repeat(hashes));
                i += source[i..].find(&suffix)? + suffix.len();
            } else {
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            continue;
        }
        if bytes[i] == left {
            depth += 1;
        } else if bytes[i] == right {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

fn fn_body<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let start = source.find(&format!("fn {name}("))?;
    let open = source[start..].find('{')? + start;
    Some(&source[open..balanced_end(source, open, b'{', b'}')?])
}

fn execute_error_handler<'a>(body: &'a str, sql: &str) -> &'a str {
    let after_execute = body
        .split_once(sql)
        .expect("execute SQL anchor must exist")
        .1;
    let after_map = after_execute
        .split_once(".map_err(")
        .expect("execute failure must have an error handler")
        .1;
    let open = after_map.find('{').expect("error closure must have a body");
    &after_map[open..balanced_end(after_map, open, b'{', b'}').expect("closed error handler")]
}

fn commit_handler() -> &'static str {
    execute_error_handler(mark_queued_body(), "admission_mark_queued_sql()")
}

fn upsert_handler() -> &'static str {
    let consumer = production(include_str!("../src/queue_consumer.rs"));
    let edge_impl = consumer
        .split_once("impl MatchWriteRuntime for EdgeMatchWrite<'_>")
        .expect("edge upsert impl must exist")
        .1;
    let upsert = fn_body(edge_impl, "upsert").expect("edge upsert body must exist");
    execute_error_handler(upsert, "upsert_library_sql()")
}

/// Include only one-level helpers actually called from the commit failure arm.
fn commit_context() -> String {
    let body = commit_handler();
    let mut context = body.to_string();
    let sources = [
        production(include_str!("../src/lib.rs")),
        production(include_str!("../src/library.rs")),
    ];
    for (i, _) in body.match_indices('(') {
        let start = body[..i]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |j| j + 1);
        let name = &body[start..i];
        if name.is_empty() {
            continue;
        }
        for source in sources {
            if let Some(helper) = fn_body(source, name) {
                context.push('\n');
                context.push_str(helper);
            }
        }
    }
    context
}

fn compact(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Pin all four emit arguments together; another log cannot supply missing fields.
fn assert_failure_event(handler: &str, event: &str, trace: &str, build: &str, category: &str) {
    let calls: Vec<&str> = handler
        .match_indices("emit(")
        .filter(|(i, _)| {
            *i == 0
                || !handler.as_bytes()[i - 1].is_ascii_alphanumeric()
                    && handler.as_bytes()[i - 1] != b'_'
        })
        .filter_map(|(i, _)| {
            let open = i + "emit".len();
            let end = balanced_end(handler, open, b'(', b')')?;
            Some(&handler[i..end])
        })
        .filter(|call| call.contains(&format!("\"{event}\"")))
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "failure handler must emit {event} exactly once"
    );
    let call = compact(calls[0]);
    assert!(
        call.starts_with(&format!("emit(Level::Error,{trace},\"{event}\","))
            || call.starts_with(&format!(
                "emit(nntune_logging::Level::Error,{trace},\"{event}\","
            )),
        "failure event must keep Error level and the originating trace: {call}"
    );
    assert!(
        call.contains("serde_json::json!({"),
        "structured fields required: {call}"
    );
    assert!(
        call.contains(&format!("\"build_sha\":{build}")),
        "build identity missing: {call}"
    );
    assert!(
        call.contains(&format!("\"sqlstate\":{category}")),
        "SQLSTATE category missing: {call}"
    );
}

fn assert_commit_event(handler: &str) {
    assert_failure_event(
        handler,
        "ingest.admission.commit_failed",
        "&self.trace_id",
        "build_sha_from_env(&self.env)",
        "admission_commit_sqlstate(&e)",
    );
}

fn assert_upsert_event(handler: &str) {
    assert_failure_event(
        handler,
        "ingest.library.upsert_failed",
        "self.trace_id",
        "crate::build_sha_from_env(self.env)",
        "sqlstate.unwrap_or(\"none\")",
    );
}

#[test]
fn sibling_precedent_still_logs_sqlstate_only() {
    let handler = upsert_handler();
    assert!(handler.contains("error.as_db_error().map(|db| db.code().code())"));
    assert_upsert_event(handler);
}

#[test]
fn commit_failure_is_still_detail_free_transport_error() {
    let handler = commit_handler();
    assert!(
        handler
            .contains("MeterWriteError::Transport(\"admission ledger unavailable\".to_string())"),
        "execute failure must keep mapping to the detail-free transport error"
    );
    assert!(mark_queued_body().contains("admission_mark_queued_sql()"));
}

fn assert_sqlstate_extraction(context: &str) {
    assert!(
        context.contains("as_db_error()"),
        "DbError extraction required"
    );
    assert!(
        context.contains(".code().code()"),
        "SQLSTATE code extraction required"
    );
    assert!(
        context.contains("unwrap_or(\"none\")"),
        "non-DB errors require the none category"
    );
}

#[test]
fn commit_failure_log_carries_the_sqlstate_code() {
    assert_sqlstate_extraction(&commit_context());
    assert_commit_event(commit_handler());
}

#[test]
fn sqlstate_extraction_guard_rejects_missing_code_and_non_db_fallback() {
    let helper = r#"{ error.as_db_error().map(|db| db.code().code()).unwrap_or("none") }"#;
    assert_sqlstate_extraction(helper);
    for mutant in [
        helper.replace("as_db_error()", "other()"),
        helper.replace(".code().code()", ".message()"),
        helper.replace(".unwrap_or(\"none\")", ".unwrap()"),
        helper.replace("\"none\"", "\"unknown\""),
    ] {
        assert!(std::panic::catch_unwind(|| assert_sqlstate_extraction(&mutant)).is_err());
    }
}

#[test]
fn failure_contexts_never_read_server_details() {
    for context in [commit_context(), upsert_handler().to_string()] {
        for leak in [".message()", ".detail()", ".hint()"] {
            assert!(
                !context.contains(leak),
                "failure path must not read {leak}: {context}"
            );
        }
    }
}

#[test]
fn commit_failure_still_logs_at_the_commit_site() {
    assert_commit_event(commit_handler());
}

#[test]
fn structured_event_predicates_reject_missing_log_category_and_context() {
    // Hand-written fixtures, not production edits. Every mutation below removes
    // an operational/privacy contract field while retaining valid Rust syntax.
    let commit = r#"emit(Level::Error, &self.trace_id, "ingest.admission.commit_failed",
        serde_json::json!({"build_sha": build_sha_from_env(&self.env),
            "sqlstate": admission_commit_sqlstate(&e)}));"#;
    let upsert = r#"crate::emit(nntune_logging::Level::Error, self.trace_id, "ingest.library.upsert_failed",
        serde_json::json!({"build_sha": crate::build_sha_from_env(self.env),
            "sqlstate": sqlstate.unwrap_or("none")}));"#;
    assert_commit_event(commit);
    assert_upsert_event(upsert);
    for (fixture, predicate) in [
        (commit, assert_commit_event as fn(&str)),
        (upsert, assert_upsert_event as fn(&str)),
    ] {
        for (name, mutant) in [
            ("omitted event", String::new()),
            ("wrong event", fixture.replace("_failed", "_succeeded")),
            (
                "omitted category",
                fixture.replace("\"sqlstate\"", "\"other\""),
            ),
            (
                "omitted build identity",
                fixture.replace("\"build_sha\"", "\"other\""),
            ),
            (
                "wrong trace",
                fixture.replace("self.trace_id", "unrelated_trace"),
            ),
            (
                "wrong severity",
                fixture.replace("Level::Error", "Level::Info"),
            ),
        ] {
            assert!(
                std::panic::catch_unwind(|| predicate(&mutant)).is_err(),
                "guard accepted {name}: {mutant}"
            );
        }
    }
    let no_fallback = upsert.replace("sqlstate.unwrap_or(\"none\")", "sqlstate.unwrap()");
    assert!(std::panic::catch_unwind(|| assert_upsert_event(&no_fallback)).is_err());
    let raw_category = commit.replace("admission_commit_sqlstate(&e)", "format!(\"{e}\")");
    assert!(std::panic::catch_unwind(|| assert_commit_event(&raw_category)).is_err());
}

#[test]
fn execute_failure_anchor_excludes_unrelated_events() {
    // A matching event before the SQL or after the error closure must not mask
    // an omitted commit log. This exercises the same slice used on production.
    let body = r#"{
        emit(Level::Error, &self.trace_id, "ingest.admission.commit_failed",
            serde_json::json!({"build_sha": build_sha_from_env(&self.env),
                "sqlstate": admission_commit_sqlstate(&e)}));
        client.execute(admission_mark_queued_sql(), &[]).await.map_err(|e| {
            MeterWriteError::Transport("admission ledger unavailable".to_string())
        })?;
        emit(Level::Error, &self.trace_id, "ingest.admission.commit_failed",
            serde_json::json!({"build_sha": build_sha_from_env(&self.env),
                "sqlstate": admission_commit_sqlstate(&e)}));
    }"#;
    let handler = execute_error_handler(body, "admission_mark_queued_sql()");
    assert!(std::panic::catch_unwind(|| assert_commit_event(handler)).is_err());
}
