//! Worker sockets cannot be constructed natively. These source pins complement
//! hyperdrive_deadline.rs, which must exercise the real shared deadline helper.
//! Comments and the in-module tests cannot satisfy these pins.
//!
//! Builder contract: worker_entry::with_deadline(future, timeout_ms, error).
//! PgStore::connect must await a DIAL_TIMEOUT_MS-bounded socket.opened() inside
//! ONE TOTAL_TIMEOUT_MS-bounded async block containing socket creation, Postgres
//! handshake, and the same client's SET statement_timeout. An enclosing timer
//! gives connect_raw only the remaining budget, never a fresh total after dial.
//! Export the helper for native tests; do not change these acceptance files.
//!
//! Breaks caught: timing only handshake or only dial, resetting total after dial,
//! setting the timeout on a second client, returning before SET finishes, losing
//! connection_error/build identity, or formatting connection details on expiry.
//! These pins are wiring evidence, not a live Hyperdrive/TLS cancellation test.

fn production() -> String {
    let source = include_str!("../src/worker_entry.rs")
        .split_once("#[cfg(test)]")
        .expect("worker_entry test-module boundary")
        .0;
    // Keep string literals intact while removing comments, then normalize
    // whitespace. Quotes are still tracked by the delimiter scanner below.
    let bytes = source.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut quoted = false;
    let mut block_depth = 0;
    while i < bytes.len() {
        if block_depth > 0 {
            if bytes[i..].starts_with(b"/*") {
                block_depth += 1;
                i += 2;
            } else if bytes[i..].starts_with(b"*/") {
                block_depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
        } else if quoted {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i] as char);
                out.push(bytes[i + 1] as char);
                i += 2;
            } else {
                quoted = bytes[i] != b'"';
                out.push(bytes[i] as char);
                i += 1;
            }
        } else if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            block_depth = 1;
            i += 2;
        } else {
            quoted = bytes[i] == b'"';
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn store(production: &str) -> &str {
    let implementation = production
        .split_once("implPgStore{")
        .expect("PgStore impl")
        .1
        .split_once("fnuser_from_row(")
        .expect("following function bounds PgStore")
        .0;
    function_body(implementation, "asyncfnconnect(&self)")
}

struct Call<'a> {
    args: Vec<&'a str>,
    after: &'a str,
}

// Split Rust call arguments on top-level commas only. Nested async blocks,
// json!/format! strings, and closures with nested calls remain single arguments.
fn calls<'a>(source: &'a str, name: &str) -> Vec<Call<'a>> {
    let needle = format!("{name}(");
    source
        .match_indices(&needle)
        .map(|(offset, _)| {
            let start = offset + needle.len();
            let mut arg_start = start;
            let mut depth = 1;
            let mut quoted = false;
            let mut escaped = false;
            let mut args = Vec::new();
            for (i, byte) in source.bytes().enumerate().skip(start) {
                if quoted {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        quoted = false;
                    }
                    continue;
                }
                match byte {
                    b'"' => quoted = true,
                    b'(' | b'{' | b'[' => depth += 1,
                    b')' | b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            if i > arg_start {
                                args.push(&source[arg_start..i]);
                            }
                            return Call {
                                args,
                                after: &source[i + 1..],
                            };
                        }
                    }
                    b',' if depth == 1 => {
                        args.push(&source[arg_start..i]);
                        arg_start = i + 1;
                    }
                    _ => {}
                }
            }
            panic!("unclosed {name} call")
        })
        .collect()
}

fn function_body<'a>(source: &'a str, declaration: &str) -> &'a str {
    let signature = source
        .split_once(declaration)
        .expect("production function exists")
        .1;
    let start = signature.find('{').expect("function body starts");
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, byte) in signature.bytes().enumerate().skip(start) {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &signature[start..=i];
                    }
                }
                _ => {}
            }
        }
    }
    panic!("unclosed function body")
}

fn budget_is(argument: &str, constant: &str) -> bool {
    argument.rsplit("::").next() == Some(constant)
}

fn total_future(connect: &str) -> &str {
    let bounded = calls(connect, "with_deadline")
        .into_iter()
        .find(|call| call.args.len() == 3 && budget_is(call.args[1], "TOTAL_TIMEOUT_MS"))
        .expect("PgStore::connect must await with_deadline(..., TOTAL_TIMEOUT_MS, StoreError)");
    assert_eq!(
        bounded.args[2], "StoreError",
        "expiry uses the opaque store error"
    );
    assert!(
        bounded.after.starts_with(".await"),
        "overall deadline must be awaited"
    );
    assert!(
        bounded.args[0].starts_with("async{"),
        "one overall async operation owns all phases"
    );
    bounded.args[0]
}

#[test]
fn socket_open_uses_the_vault_dial_budget_and_the_shared_helper() {
    let source = production();
    let connect = store(&source);
    let dial = calls(connect, "with_deadline")
        .into_iter()
        .find(|call| call.args.len() == 3 && budget_is(call.args[1], "DIAL_TIMEOUT_MS"))
        .expect("socket.opened() must be raced with the vault DIAL_TIMEOUT_MS");
    assert!(dial.args[0].contains("socket.opened()"));
    assert_eq!(dial.args[2], "StoreError");
    assert!(
        dial.after.starts_with(".await?"),
        "dial expiry must propagate before handshake"
    );
    assert!(connect.contains("SecureTransport::StartTls"));
}

#[test]
fn total_budget_encloses_dial_handshake_and_statement_setup_without_reset() {
    let source = production();
    let operation = total_future(store(&source));
    for stage in [
        ".connect(hyperdrive.host(),hyperdrive.port())",
        "socket.opened()",
        "DIAL_TIMEOUT_MS",
        ".connect_raw(socket,worker::postgres_tls::PassthroughTls)",
        "client.batch_execute(",
        "Ok(client)",
    ] {
        assert!(
            operation.contains(stage),
            "overall budget must contain {stage}"
        );
    }
    assert!(operation.find("socket.opened()").unwrap() < operation.find(".connect_raw(").unwrap());
    assert!(
        operation.find(".connect_raw(").unwrap() < operation.find("client.batch_execute(").unwrap()
    );
    assert!(
        operation.find("client.batch_execute(").unwrap() < operation.find("Ok(client)").unwrap()
    );
}

#[test]
fn returned_client_finishes_setting_statement_timeout_to_the_overall_budget() {
    let source = production();
    let operation = total_future(store(&source));
    assert_eq!(
        operation.matches(".connect_raw(").count(),
        1,
        "no second connection for SET"
    );
    assert!(
        operation.contains("let(client,connection)="),
        "SET must use the handshake's client"
    );
    assert_eq!(
        operation.matches("statement_timeout").count(),
        1,
        "one SET per connection"
    );
    let sets = calls(operation, "client.batch_execute");
    assert_eq!(sets.len(), 1);
    assert!(sets[0].args[0].contains("SETstatement_timeout="));
    assert!(
        sets[0].args[0].contains("TOTAL_TIMEOUT_MS"),
        "SET uses the shared total, not a fresh literal"
    );
    assert!(
        sets[0].after.starts_with(".await.map_err(|_|StoreError)?"),
        "SET must finish or fail before returning the client"
    );
    assert!(
        operation.find("spawn_local(").unwrap() < operation.find("client.batch_execute(").unwrap(),
        "the connection driver must be running before awaiting SET"
    );
}

#[test]
fn background_connection_error_keeps_the_existing_redacted_build_identity() {
    let source = production();
    let connect = store(&source);
    assert!(connect.contains("connection.await.is_err()"));
    assert!(connect.contains("\"api.db.connection_error\""));
    let fields = calls(connect, "json!");
    assert!(
        !fields.is_empty(),
        "connection error keeps structured fields"
    );
    for call in fields {
        assert_eq!(call.args.len(), 1);
        assert!(
            matches!(
                call.args[0],
                "{\"error_class\":\"service_unavailable\",\"build_sha\":build_sha}"
                    | "{\"build_sha\":build_sha,\"error_class\":\"service_unavailable\"}"
            ),
            "database failure fields must contain only error_class and build_sha"
        );
    }
}

#[test]
fn expiry_never_formats_host_role_or_connection_string() {
    let source = production();
    let connect = store(&source);
    let deadlines = calls(connect, "with_deadline");
    assert!(
        deadlines.len() >= 2,
        "both deadline expiry sites must exist"
    );
    for deadline in deadlines {
        assert_eq!(deadline.args.len(), 3);
        assert_eq!(
            deadline.args[2], "StoreError",
            "no formatted driver detail on expiry"
        );
    }
    // PgStore may format the numeric SET, but nothing else. In particular both
    // Debug spellings (:? and :#?) and interpolated host/role/DSN are refused.
    for call in calls(connect, "format!") {
        assert!(
            (call.args.len() == 1 && call.args[0] == "\"SETstatement_timeout={TOTAL_TIMEOUT_MS}\"")
                || (call.args.len() == 2
                    && call.args[0] == "\"SETstatement_timeout={}\""
                    && budget_is(call.args[1], "TOTAL_TIMEOUT_MS")),
            "the only allowed connection formatting is the numeric statement timeout"
        );
    }
    for forbidden in [
        "dbg!",
        "println!",
        "write!",
        "writeln!",
        "format_args!",
        ".to_string()",
        "console_",
    ] {
        assert!(
            !connect.contains(forbidden),
            "connection path must not expose driver details via {forbidden}"
        );
    }
    // The shared helper itself must return the supplied opaque error rather
    // than emitting or formatting a future, hostname, role, or driver error.
    let helper = function_body(&source, "pubasyncfnwith_deadline");
    for forbidden in [
        "format!",
        "format_args!",
        "dbg!",
        "println!",
        "write!",
        "emit_record(",
        "console_",
    ] {
        assert!(
            !helper.contains(forbidden),
            "deadline helper must not format or log errors"
        );
    }
}
