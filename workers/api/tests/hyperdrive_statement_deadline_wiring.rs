//! Worker sockets cannot be constructed natively. These source pins prove every
//! PgStore statement runs inside the per-method bound, complementing the
//! connect-time pins in hyperdrive_store_deadline_wiring.rs (which this file
//! must not duplicate: connect keeps its own dial/total budgets plus SET).
//!
//! Builder contract: PgStore::bounded(operation, timeout_ms) races one store
//! operation against the shared with_deadline helper, expiring to the opaque
//! StoreError. Every `Store for PgStore` method and `SingleUseStore::claim`
//! must await exactly one Self::bounded call with TOTAL_TIMEOUT_MS whose
//! async block contains that method's self.connect() (connect plus all of
//! its statements share one bound: finishing one statement must not unbind
//! a later stalled one). Hyperdrive RESETs pooled backends, so this
//! per-method race is what bounds statements where they run.
//!
//! Breaks caught: a new Store method that connects without the seam, a
//! method that bounds an empty block while its statements run outside it,
//! a client that escapes the seam for a later statement, a rate-limit
//! pair that compensates in Rust instead of one transaction, a fresh
//! timeout literal instead of the vault total, or expiry that formats
//! driver detail instead of returning the opaque error.

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

fn method_name(piece: &str) -> &str {
    piece
        .split_once('(')
        .expect("method piece starts with a name")
        .0
}

fn assert_bounded(piece: &str, name: &str) {
    let bounded = calls(piece, "Self::bounded");
    assert_eq!(
        bounded.len(),
        1,
        "{name} must race exactly one operation through Self::bounded"
    );
    let call = &bounded[0];
    assert_eq!(
        call.args.len(),
        2,
        "{name} must pass (operation, budget) to Self::bounded"
    );
    assert!(
        call.args[0].starts_with("async{"),
        "{name} must bound one async operation"
    );
    assert!(
        call.args[0].contains("self.connect()"),
        "{name} must run its connect inside the bound operation"
    );
    assert_eq!(
        call.args[1], "TOTAL_TIMEOUT_MS",
        "{name} must use the shared vault total, not a fresh literal"
    );
    assert!(
        call.after.starts_with(".await"),
        "{name} must await the bound operation"
    );
    // Placement: the whole method connects exactly once, and no statement
    // runs outside the bounded argument. A client that escapes the seam —
    // an extra connect, or a query/execute/transaction after the awaited
    // bound — fails here, not in production.
    assert_eq!(
        piece.matches("self.connect()").count(),
        1,
        "{name} must connect exactly once, inside the bound operation"
    );
    let arg = call.args[0];
    let arg_start = arg.as_ptr() as usize - piece.as_ptr() as usize;
    let remainder = format!("{}{}", &piece[..arg_start], &piece[arg_start + arg.len()..]);
    for marker in [
        "self.connect()",
        ".query",
        ".execute",
        ".batch_execute",
        ".transaction(",
        ".prepare",
        ".simple_query",
        ".commit(",
        ".rollback(",
        // Statement helpers are statement calls too: a client escaping
        // the seam into a helper is invisible to the driver markers.
        "claim_rate_limit_slot(",
        "delete_expired_rate_limit_slots(",
    ] {
        assert!(
            !remainder.contains(marker),
            "{name} must run no statement outside the bound operation ({marker} escapes the seam)"
        );
    }
    // Durable escape rule: every statement, direct or via helper, needs an
    // await. The remainder keeps exactly one `.await` (the bounded call's
    // own: signatures cannot await, so the first one is it); strip it and
    // forbid any further await plus any handle use, so moving a helper
    // call after the awaited bound fails here, not in production.
    let awaited = ".await";
    let await_pos = remainder
        .find(awaited)
        .expect("the bounded call is awaited");
    let unawaited = format!(
        "{}{}",
        &remainder[..await_pos],
        &remainder[await_pos + awaited.len()..]
    );
    assert!(
        !unawaited.contains(awaited),
        "{name} must await nothing outside the bound operation (a second .await escapes the seam)"
    );
    for handle in ["&client", "&transaction"] {
        assert!(
            !unawaited.contains(handle),
            "{name} must use no client handle outside the bound operation ({handle} escapes the seam)"
        );
    }
}

#[test]
fn every_store_method_races_its_statements_through_the_shared_seam() {
    let source = production();
    let store = source
        .split_once("implStoreforPgStore{")
        .expect("Store impl")
        .1
        .split_once("implSingleUseStoreforPgStore{")
        .expect("SingleUseStore impl bounds the Store impl")
        .0;
    let mut pieces = store.split("asyncfn");
    pieces.next().expect("preamble before the first method");
    let pieces: Vec<&str> = pieces.collect();
    let names: Vec<&str> = pieces.iter().map(|piece| method_name(piece)).collect();
    // Anchors prove the splitter saw the whole impl, first method to last.
    for anchor in [
        "user_by_email",
        "replace_session_after_reauthentication",
        "reserve_rate_limit_pair",
        "release_test_connection_lease",
    ] {
        assert!(
            names.contains(&anchor),
            "Store impl slice must contain {anchor}"
        );
    }
    for piece in &pieces {
        assert_bounded(piece, method_name(piece));
    }
}

#[test]
fn single_use_claim_races_its_statement_and_keeps_the_invalid_mapping() {
    let source = production();
    let single = source
        .split_once("implSingleUseStoreforPgStore{")
        .expect("SingleUseStore impl")
        .1
        .split_once("structDohDns")
        .expect("following item bounds the SingleUseStore impl")
        .0;
    assert_bounded(single, "claim");
    assert!(
        single.contains("AuthError::Invalid"),
        "claim must keep mapping store failure to AuthError::Invalid"
    );
}

#[test]
fn rate_limit_pair_commits_both_claims_in_one_cancellation_safe_transaction() {
    // Deadline expiry drops the raced future, so an Err-arm release written
    // inside it can never run: the pair must commit its delete plus both
    // claims in one database transaction (a dropped tokio-postgres
    // transaction rolls back) instead of compensating in Rust. PgStore
    // needs a Worker Env, so this source pin plus the seam drop test in
    // worker_entry.rs are the native proof; the drop itself is pinned by
    // hyperdrive_deadline.rs against the shared helper.
    let source = production();
    let store = source
        .split_once("implStoreforPgStore{")
        .expect("Store impl")
        .1
        .split_once("implSingleUseStoreforPgStore{")
        .expect("SingleUseStore impl bounds the Store impl")
        .0;
    let mut pieces = store.split("asyncfn");
    pieces.next().expect("preamble before the first method");
    let pair = pieces
        .find(|piece| method_name(piece) == "reserve_rate_limit_pair")
        .expect("pair piece");
    let in_transaction = pair
        .split_once(".transaction(")
        .expect("pair opens one transaction")
        .1;
    assert_eq!(
        in_transaction
            .matches("claim_rate_limit_slot(&transaction,")
            .count(),
        2,
        "both claims must run on the pair transaction handle"
    );
    assert!(
        in_transaction.contains("delete_expired_rate_limit_slots(&transaction,"),
        "the pair delete must run on the pair transaction handle"
    );
    assert!(
        in_transaction.contains(".commit("),
        "an allowed pair must commit both claims together"
    );
    assert!(
        in_transaction.contains(".rollback("),
        "a denied pair must roll back instead of compensating in Rust"
    );
    assert!(
        !pair.contains("release_rate_limit_slot"),
        "no Err-arm release may remain: rollback owns pair atomicity"
    );
    assert!(
        !source.contains("release_rate_limit_slot"),
        "the compensating helper is deleted, not just unused"
    );
}

#[test]
fn mint_path_statements_run_unnamed_through_query_typed() {
    // TWO-222: tokio-postgres names prepared statements `sN` per isolate, and
    // a named statement can collide on, or go missing from, a
    // Hyperdrive-pooled backend between round-trips. The mint seam's three
    // statements must stay unnamed (`query_typed`); reverting any of them to
    // a named-preparing call fails here, not as a staging 500.
    let source = production();
    let store = source
        .split_once("implStoreforPgStore{")
        .expect("Store impl")
        .1
        .split_once("implSingleUseStoreforPgStore{")
        .expect("SingleUseStore impl bounds the Store impl")
        .0;
    let mut pieces = store.split("asyncfn");
    pieces.next().expect("preamble before the first method");
    let pieces: Vec<&str> = pieces.collect();
    for name in ["user_by_email", "insert_user", "insert_session"] {
        let piece = pieces
            .iter()
            .find(|piece| method_name(piece) == name)
            .unwrap_or_else(|| panic!("{name} piece"));
        assert_eq!(
            piece.matches(".query_typed(").count(),
            1,
            "{name} must run exactly one unnamed query_typed statement"
        );
        for named in [
            ".query(",
            ".query_one(",
            ".query_opt(",
            ".query_raw(",
            ".execute(",
            ".execute_raw(",
            ".prepare",
        ] {
            assert!(
                !piece.contains(named),
                "{name} must not run a named prepared statement ({named})"
            );
        }
    }
}

#[test]
fn signup_path_statements_run_unnamed_through_query_typed() {
    // TWO-434: after the mint path went unnamed, staging `register/start`
    // still answered 500 through Hyperdrive. Every statement the public
    // signup start and finish, its mail issuance and the code seam run must
    // stay unnamed too: the Store methods below plus the shared free helpers
    // they call (the reaper, the challenge prune, the rate-limit slot
    // claims and the registration transaction with its single-use claims).
    let source = production();
    let store = source
        .split_once("implStoreforPgStore{")
        .expect("Store impl")
        .1
        .split_once("implSingleUseStoreforPgStore{")
        .expect("SingleUseStore impl bounds the Store impl")
        .0;
    let methods = [
        "insert_challenge",
        "challenge_by_id",
        "credentials_for_user",
        "magic_by_hash",
        "invite_by_hash",
        "insert_audit",
        "insert_magic",
        "record_qa_issued_code",
        "qa_issued_code",
    ]
    .map(|name| (name, function_body(store, &format!("asyncfn{name}("))));
    let helpers = [
        "reap_unverified_users_on",
        "prune_expired_challenges_on",
        "delete_expired_rate_limit_slots",
        "claim_rate_limit_slot",
        "complete_registration_on",
        "claim_single_use_on",
        "insert_credential_on",
    ]
    .map(|name| (name, function_body(&source, &format!("asyncfn{name}("))));
    for (name, body) in methods.iter().chain(helpers.iter()) {
        assert!(
            body.contains(".query_typed("),
            "{name} must run its statements unnamed via query_typed"
        );
        for named in [
            ".query(",
            ".query_one(",
            ".query_opt(",
            ".query_raw(",
            ".execute(",
            ".execute_raw(",
            ".prepare",
        ] {
            assert!(
                !body.contains(named),
                "{name} must not run a named prepared statement ({named})"
            );
        }
    }
}

#[test]
fn magic_finish_path_statements_run_unnamed_through_query_typed() {
    // TWO-539: after the signup path went unnamed, staging magic-link
    // finish still answered 500 through Hyperdrive (login-magic-link and
    // sign-out-everywhere on run 41278067667). Every statement those two
    // legs run must stay unnamed too: the redemption transaction's claim,
    // session insert and audit insert (in `magic_redemption.rs`, not
    // `worker_entry.rs`), plus the Store methods the legs hit next —
    // the session and vault reads behind `authenticate`/`GET /v1/vault`,
    // and the two revoke-all sweeps. The finish path's rate-limit claim
    // and `magic_by_hash` are already pinned by the signup test above.
    const NAMED: [&str; 7] = [
        ".query(",
        ".query_one(",
        ".query_opt(",
        ".query_raw(",
        ".execute(",
        ".execute_raw(",
        ".prepare",
    ];
    let source = production();
    let store = source
        .split_once("implStoreforPgStore{")
        .expect("Store impl")
        .1
        .split_once("implSingleUseStoreforPgStore{")
        .expect("SingleUseStore impl bounds the Store impl")
        .0;
    for name in [
        "session_by_id",
        "vault_by_user",
        "revoke_all_sessions",
        "revoke_all_native_credentials",
    ] {
        let body = function_body(store, &format!("asyncfn{name}("));
        assert!(
            body.contains(".query_typed("),
            "{name} must run its statements unnamed via query_typed"
        );
        for named in NAMED {
            assert!(
                !body.contains(named),
                "{name} must not run a named prepared statement ({named})"
            );
        }
    }
    // The production redemption transaction lives outside worker_entry:
    // slice its `tokio_postgres::Transaction` impl (comments stripped,
    // whitespace normalized, like `production()` above) and pin the
    // same rule. `SET LOCAL` stays on `batch_execute` (simple-query
    // protocol, never a named prepared statement).
    let redemption: String = include_str!("../src/magic_redemption.rs")
        .split_once("#[cfg(all(test,")
        .expect("magic_redemption test-module boundary")
        .0
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<String>()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    let pg_impl = redemption
        .split_once("implRedemptionTransactionfortokio_postgres::Transaction<'_>{")
        .expect("production redemption transaction impl")
        .1
        .split_once("pub(crate)asyncfnredeem(")
        .expect("redeem bounds the transaction impl")
        .0;
    assert_eq!(
        pg_impl.matches(".query_typed(").count(),
        3,
        "claim, session insert and audit insert must each run unnamed via query_typed"
    );
    for named in NAMED {
        assert!(
            !pg_impl.contains(named),
            "redemption transaction must not run a named prepared statement ({named})"
        );
    }
}

#[test]
fn seam_delegates_to_the_shared_helper_with_the_opaque_error() {
    let source = production();
    let pgstore = source
        .split_once("implPgStore{")
        .expect("PgStore impl")
        .1
        .split_once("fnuser_from_row(")
        .expect("following function bounds PgStore")
        .0;
    let seam = function_body(pgstore, "asyncfnbounded");
    assert!(
        seam.contains("with_deadline("),
        "the seam must delegate to the shared with_deadline helper"
    );
    assert!(
        seam.contains("StoreError"),
        "expiry uses the opaque store error"
    );
    assert!(
        seam.contains(".await"),
        "the raced operation must be awaited"
    );
    for own_budget in ["TOTAL_TIMEOUT_MS", "DIAL_TIMEOUT_MS"] {
        assert!(
            !seam.contains(own_budget),
            "the budget is a caller parameter, never a literal in the seam"
        );
    }
    for forbidden in ["format!", "emit_record(", "console_"] {
        assert!(
            !seam.contains(forbidden),
            "the seam must not format or log on the expiry path"
        );
    }
}
