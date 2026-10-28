//! PROTECTED acceptance pins; the Builder must not edit this file.
//!
//! Native behavior is in `library_db_deadline.rs`. Hyperdrive/Env cannot run
//! on the host, so these pins bind that tested seam to the five edge sites.
//! Wrap each site's DB work in `with_db_deadline(async { ... }, deadline,
//! on_expiry).await`, with a Worker Delay made from the shared positive finite
//! `library::LIBRARY_DB_DEADLINE_MS` constant. The whole multi-query admission
//! sequence shares one deadline; a timer beside a direct await is not a bound.
//!
//! Retain existing SQLSTATE handling and error shapes INSIDE that async block.
//! For release_claim, discard the wrapper's Result (`let _ = ... .await;`).
//! Expiry closures: `|| ()` for connect/release; `|| MeterWriteError::Transport(
//! "admission ledger unavailable".to_string())` for admission; and
//! `|| JobError::transient(ERR_WRITE_FAILED, "library database unavailable")`
//! for upsert. Import the error types as the existing edge code does.
//!
//! The exact existing binding-missing Debug log is grandfathered: the separate
//! structured-logging change owns it. This does NOT allow additional Debug,
//! raw driver Display, connection strings or host data in logs, and does NOT
//! require replacing console_log! with emit(). No live Hyperdrive is used.

fn production(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .map_or(source, |(head, _)| head)
}

// Mask comments and strings without moving byte offsets. The second mask
// leaves strings intact for error/log checks; fake tokens in comments cannot
// satisfy wiring pins. These sources use ordinary escaped string literals.
fn masked(source: &str, hide_strings: bool) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out[start..i].fill(b' ');
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out[start..i].fill(b' ');
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            if hide_strings {
                out[start..i].fill(b' ');
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("source masking preserves UTF-8")
}

fn closing(code: &str, open: usize, left: u8, right: u8) -> usize {
    assert_eq!(code.as_bytes()[open], left);
    let mut depth = 0;
    for (i, &ch) in code.as_bytes().iter().enumerate().skip(open) {
        if ch == left {
            depth += 1;
        }
        if ch == right {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    panic!("unclosed source delimiter");
}

fn body<'a>(source: &'a str, name: &str) -> &'a str {
    let code = masked(source, true);
    let start = code
        .find(&format!("fn {name}("))
        .expect("named function must exist");
    let open = start + code[start..].find('{').expect("function body must exist");
    &source[open + 1..closing(&code, open, b'{', b'}')]
}

fn compact(source: &str) -> String {
    masked(source, false).split_whitespace().collect()
}

fn lib() -> &'static str {
    production(include_str!("../src/lib.rs"))
}
fn admission(name: &str) -> &'static str {
    body(
        lib()
            .split_once("impl AdmissionLedger for PgAdmissionLedger")
            .unwrap()
            .1,
        name,
    )
}
fn upsert() -> &'static str {
    body(
        production(include_str!("../src/queue_consumer.rs"))
            .split_once("impl MatchWriteRuntime for EdgeMatchWrite")
            .unwrap()
            .1,
        "upsert",
    )
}

/// Return the expiry/timer arguments after proving EVERY DB await is inside
/// the async operation passed to the tested helper, not merely beside it.
fn bounded<'a>(site: &str, source: &'a str, required: &[&str]) -> &'a str {
    let code = masked(source, true);
    let start = code
        .find("with_db_deadline(")
        .unwrap_or_else(|| panic!("{site} must bound DB awaits through with_db_deadline"));
    let open = start + "with_db_deadline".len();
    let end = closing(&code, open, b'(', b')');
    let args = &code[open + 1..end];
    let async_at = args
        .find("async")
        .expect("first argument must be an async operation");
    assert!(
        args[..async_at].trim().is_empty(),
        "{site}: pass the DB operation first"
    );
    let block_open = open + 1 + async_at + args[async_at..].find('{').unwrap();
    let block_end = closing(&code, block_open, b'{', b'}');
    let operation = &source[block_open + 1..block_end];
    let operation_code = masked(operation, true);
    for needle in required {
        assert!(
            operation_code.contains(needle),
            "{site}: {needle} must be inside the raced operation"
        );
        assert_eq!(
            code.matches(needle).count(),
            operation_code.matches(needle).count(),
            "{site}: no {needle} may escape the raced operation"
        );
    }
    assert!(
        !code[..start].contains(".await"),
        "{site}: no unbounded await before the wrapper"
    );
    let tail = &source[block_end + 1..end];
    let timer = compact(&masked(tail, true));
    assert!(
        timer.contains("Delay::from(")
            && timer.contains("from_millis(")
            && timer.contains("LIBRARY_DB_DEADLINE_MS"),
        "{site}: arm a real finite Worker Delay"
    );
    assert!(
        !masked(tail, true).contains(".await"),
        "{site}: no await while building the timer/error"
    );
    assert!(
        compact(&source[end + 1..]).starts_with(".await"),
        "{site}: actually await the race"
    );
    assert_eq!(
        masked(&source[end + 1..], true).matches(".await").count(),
        1,
        "{site}: no DB await may follow the wrapper"
    );
    tail
}

#[test]
fn library_db_client_connect_is_deadline_bounded() {
    let source = body(lib(), "library_db_client");
    let tail = compact(bounded("library_db_client", source, &[".connect_raw("]));
    assert!(tail.contains("||()"), "connect expiry must remain Err(())");
}

#[test]
fn record_stage_once_bounds_ensure_claim_and_read_together() {
    let source = admission("record_stage_once");
    let tail = compact(bounded(
        "record_stage_once",
        source,
        &[
            ".client()",
            ".execute(",
            ".query_opt(",
            "admission_ensure_sql()",
            "admission_claim_sql()",
            "admission_read_mark_sql()",
        ],
    ));
    assert!(
        tail.contains("||MeterWriteError::Transport(\"admissionledgerunavailable\".to_string())")
    );
}

#[test]
fn mark_queued_commit_is_deadline_bounded() {
    let tail = compact(bounded(
        "mark_queued",
        admission("mark_queued"),
        &[".client()", ".execute(", "admission_mark_queued_sql()"],
    ));
    assert!(
        tail.contains("||MeterWriteError::Transport(\"admissionledgerunavailable\".to_string())")
    );
}

#[test]
fn release_claim_is_bounded_but_still_best_effort() {
    let source = admission("release_claim");
    let tail = compact(bounded(
        "release_claim",
        source,
        &[".client()", ".execute(", "admission_release_sql()"],
    ));
    assert!(
        tail.contains("||()"),
        "release expiry must use a unit error"
    );
    let code = compact(source);
    assert!(
        code.contains("let_=with_db_deadline(")
            || code.contains("let_=crate::library::with_db_deadline("),
        "release must discard the bounded result, including expiry"
    );
    assert!(
        code.ends_with(".await;"),
        "best-effort release must return after its bounded await"
    );
}

#[test]
fn edge_upsert_is_bounded_without_losing_sqlstate_or_row_fence() {
    let source = upsert();
    let tail = compact(bounded(
        "EdgeMatchWrite::upsert",
        source,
        &["library_db_client(", ".execute(", "upsert_library_sql()"],
    ));
    assert!(tail.contains("||JobError::transient(ERR_WRITE_FAILED,\"librarydatabaseunavailable\")"));
    let code = compact(source);
    for expected in [
        "is_deterministic_sqlstate(",
        "JobError::permanent(ERR_WRITE_FAILED,",
        "upsert_job_result(affected)",
    ] {
        assert!(code.contains(expected), "upsert must preserve {expected}");
    }
}

#[test]
fn default_database_deadline_is_positive_and_finite() {
    let source = compact(production(include_str!("../src/library.rs")));
    let after = source
        .split_once("pubconstLIBRARY_DB_DEADLINE_MS:u64=")
        .expect("publish one shared finite database deadline in library.rs")
        .1;
    let literal = after.split(';').next().unwrap().replace('_', "");
    let value: u64 = literal
        .parse()
        .expect("deadline must be an explicit finite millisecond value");
    assert!(value > 0, "a zero deadline rejects all database operations");
}

fn assert_safe_logs(site: &str, source: &str) {
    let legacy = compact("console_log!(\"library db binding missing: {:?}\", e)");
    let code = masked(source, true);
    for name in [
        "console_log!",
        "console_error!",
        "println!",
        "eprintln!",
        "format!",
        "emit",
    ] {
        for (start, _) in code.match_indices(&format!("{name}(")) {
            let open = start + name.len();
            let end = closing(&code, open, b'(', b')');
            let log = compact(&source[start..=end]);
            if site == "library_db_client" && log == legacy {
                continue;
            }
            for forbidden in [
                ":?}",
                ":#?}",
                "connection_string",
                ".host(",
                "{host}",
                "hyperdrive",
                "{e}",
                "{error}",
                ",e)",
                ",error)",
                ".to_string()",
                ".message()",
                ".detail()",
                ".hint()",
            ] {
                assert!(
                    !log.contains(forbidden),
                    "{site}: log must not expose {forbidden}: {log}"
                );
            }
        }
    }
}

#[test]
fn five_sites_do_not_gain_connection_or_driver_detail_logs() {
    for (site, source) in [
        ("library_db_client", body(lib(), "library_db_client")),
        ("record_stage_once", admission("record_stage_once")),
        ("mark_queued", admission("mark_queued")),
        ("release_claim", admission("release_claim")),
        ("EdgeMatchWrite::upsert", upsert()),
    ] {
        assert_safe_logs(site, source);
    }
}

#[test]
fn log_guard_rejects_new_debug_display_host_and_connection_string_leaks() {
    for bad in [
        "console_log!(\"failed: {:?}\", renamed)",
        "console_log!(\"failed: {renamed:#?}\")",
        "console_log!(\"failed: {error}\")",
        "console_log!(\"host={}\", hyperdrive.host())",
        "console_log!(\"dsn={}\", hyperdrive.connection_string())",
    ] {
        assert!(
            std::panic::catch_unwind(|| assert_safe_logs("mark_queued", bad)).is_err(),
            "log guard must reject {bad}"
        );
    }
    assert_safe_logs(
        "mark_queued",
        "console_log!(\"commit failed: sqlstate={}\", sqlstate.unwrap_or(\"none\"))",
    );
}

#[test]
fn wiring_guard_accepts_a_raced_query_but_rejects_an_escaped_await() {
    let valid = r#"
        with_db_deadline(async { client.execute(sql(), &[]).await },
            worker::Delay::from(Duration::from_millis(LIBRARY_DB_DEADLINE_MS)), || ()).await
    "#;
    bounded("fixture", valid, &[".execute("]);
    for invalid in [
        valid.replace("client.execute(sql(), &[]).await", "Ok(())")
            + "; client.execute(sql(), &[]).await",
        valid.replace(
            "worker::Delay::from(Duration::from_millis(LIBRARY_DB_DEADLINE_MS))",
            "pending()",
        ),
        valid.replace("with_db_deadline", "unused_timer"),
    ] {
        assert!(std::panic::catch_unwind(|| bounded("fixture", &invalid, &[".execute("])).is_err());
    }
}

#[test]
fn source_slices_ignore_comment_and_string_decoys() {
    let fake = "// with_db_deadline(async {\nfn sample() { let text = \"} with_db_deadline(\"; /* } */ direct().await; }";
    let sample = body(fake, "sample");
    assert!(sample.contains("direct().await"));
    assert!(!masked(sample, true).contains("with_db_deadline("));
}
