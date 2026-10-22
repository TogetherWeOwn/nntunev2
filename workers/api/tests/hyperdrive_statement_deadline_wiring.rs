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
//! a fresh timeout literal instead of the vault total, or expiry that
//! formats driver detail instead of returning the opaque error.

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
