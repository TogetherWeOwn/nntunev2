//! Structural obligations complement the raw-row behavioral acceptance.
//! These are deliberately NOT PostgreSQL-wire/Hyperdrive-runtime evidence.
//! The real adapter must feed the native-tested shared decoder, keep the
//! existing Hyperdrive connection precedent, and remain serving-path-unwired.
//! A Builder changing this contract returns test corrections to its author.

use std::path::{Path, PathBuf};

const ENTRY: &str = include_str!("../src/worker_entry.rs");
const LIB: &str = include_str!("../src/lib.rs");

fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .replace(",)", ")")
}

fn reader() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/billing_reader.rs"))
        .expect("missing production billing_reader module: acceptance is not a fake reader")
}

fn implementation<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing {signature}"));
    let body = &source[start..];
    let opening = body.find('{').expect("implementation body");
    let mut depth = 0;
    for (index, character) in body
        .char_indices()
        .skip_while(|(index, _)| *index < opening)
    {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[..=index];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated {signature}");
}

#[test]
fn production_reader_reuses_shared_policy_selector_decoder_and_deadline() {
    assert!(
        compact(LIB).contains("pubmodbilling_reader;"),
        "native acceptance must import the production module"
    );
    let source = compact(&reader());
    for token in [
        "subscription_live_select_statement()",
        "resolve_effective_plan(",
        "decode_subscription_row(",
        "with_deadline(",
    ] {
        assert!(
            source.contains(token),
            "production reader must reuse {token}"
        );
    }
    for forbidden in [
        "billing_account_select_statement(",
        "stripe_customer_id",
        "SystemTime::now(",
        "Date::now(",
        "Utc::now(",
        "std::env::",
        "env.var(",
        "env.secret(",
    ] {
        assert!(
            !source.contains(forbidden),
            "inert reader must not introduce {forbidden}"
        );
    }
}

#[test]
fn real_pgstore_source_connects_once_and_passes_sql_and_one_bound_identity_to_query() {
    let block = compact(implementation(ENTRY, "impl SubscriptionSource for PgStore"));
    assert_eq!(
        block.matches("self.connect().await").count(),
        1,
        "existing Hyperdrive connect, no retry loop"
    );
    assert_eq!(
        block.matches(".query(").count(),
        1,
        "raw row vector is required to detect cardinality"
    );
    assert!(
        block.contains(".query(sql,"),
        "use the SQL supplied by the real reader, not another query"
    );
    assert!(
        block.contains(".query(sql,&[&params[0]])"),
        "exactly one positional binding from the reader's authoritative params[0]"
    );
    assert!(
        block.contains("StoreError"),
        "driver failures must become the opaque storage error"
    );
    for forbidden in [
        ".execute(",
        ".batch_execute(",
        ".query_opt(",
        ".query_one(",
        "format!(",
        "billing_accounts",
        "stripe",
        "loop{",
        "while",
        "for_",
    ] {
        assert!(
            !block.contains(forbidden),
            "read-only adapter must not contain {forbidden}"
        );
    }
}

#[test]
fn postgres_decoder_is_a_real_typed_driver_adapter_not_a_fake_only_projection() {
    let combined = format!("{}\n{}", reader(), ENTRY);
    let block = compact(implementation(
        &combined,
        "impl SubscriptionRow for tokio_postgres::Row",
    ));
    assert!(
        block.contains("try_get"),
        "typed nullable BIGINT/text/bool driver access is required"
    );
    assert!(
        block.contains("StoreError"),
        "type/missing-column failures remain errors"
    );
    for forbidden in [".unwrap_or_default(", ".unwrap_or(None)", ".ok().flatten()"] {
        assert!(
            !block.contains(forbidden),
            "decode failure must not become SQL NULL: {forbidden}"
        );
    }
}

fn rust_files(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

#[test]
fn reader_stays_out_of_handlers_dispatch_admission_and_cap_consumers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    for path in files {
        if path
            .file_name()
            .is_some_and(|name| name == "billing_reader.rs" || name == "lib.rs")
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let source = if path
            .file_name()
            .is_some_and(|name| name == "worker_entry.rs")
        {
            // Only the inert raw source/driver adapter is permitted here; no
            // handler invokes the read yet. Trait imports alone are harmless.
            assert!(
                !compact(&source).contains("read_effective_plan("),
                "Worker entry must not invoke the reader"
            );
            continue;
        } else {
            source
        };
        for forbidden in [
            "billing_reader",
            "read_effective_plan(",
            "select_subscription(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} unexpectedly consumes {forbidden}",
                path.display()
            );
        }
    }
}
