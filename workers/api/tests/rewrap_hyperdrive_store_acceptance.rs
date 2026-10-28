//! Source pins for the Hyperdrive `RewrapStore` and the scheduled-event
//! glue. `PgStore` holds a Worker `Env`, so it cannot be constructed in
//! a native test; like the sibling Hyperdrive deadline pins, these tests
//! slice the production (non-test) source, strip comments, normalize
//! whitespace, and pin the wiring semantics the Builder must land.
//! Comments and in-module tests cannot satisfy them. They are wiring
//! evidence, not a live Hyperdrive test: real Postgres execution of the
//! claim/CAS/ledger shapes is covered by `tests/sql/rotation_runs.sql`
//! in the migration lane, and pass behavior over the trait is covered
//! by the scheduled-trigger acceptance bank.
//!
//! Builder contract pinned here:
//!
//! - `impl RewrapStore for PgStore` in `worker_entry.rs`, every method
//!   bounded by the shared `TOTAL_TIMEOUT_MS` budget, driver errors
//!   mapped to the opaque `StoreError` (never formatted).
//! - `claim_batch` executes the `CLAIM_BATCH_SQL` const verbatim (it is
//!   already positional); it must not inline a copy of the statement.
//! - `cas_rewrap` executes `CAS_REWRAP_POSTGRES_SQL` (the normative CAS
//!   rendered positionally), binds the rewrapped envelope before the
//!   captured envelope in const order, and maps exactly-one-row to
//!   `Rewrapped`, zero to `ClaimLost`, anything else to `StoreError`.
//! - Ledger methods speak the `rotation_runs` contract: `open_run`
//!   validates via `validate_new_run`, inserts, and maps ONLY a unique
//!   violation (SQLSTATE 23505) to `PassAlreadyRunning` (everything
//!   else is the opaque store failure); `load_open_run` filters
//!   unfinished by `(env, to_version)`; `save_progress` updates cursor
//!   and all four counters for an UNFINISHED run and fails closed on
//!   zero rows; `close_run` stamps `finished_at` on an unfinished run.
//! - The `RewrapStore` impl has no logging/debug-format path: no log
//!   macros or `emit_record`, no `{:?}`/`{:#?}`, no connection string.
//! - `lib.rs` registers the scheduled event and delegates to
//!   `pub async fn scheduled_rewrap(env: Env) -> WorkerResult<()>` in
//!   `worker_entry.rs`; that glue resolves the arming through
//!   `scheduled_rewrap_arming_from_lookup` (disabled = inert), builds
//!   the serving config (`config_from_env` / `kek_set_from_lookup`),
//!   constructs `PgStore`, and calls `run_scheduled_rewrap`. The glue
//!   never references `VAULT_KEK_CURRENT` itself: scheduling flips no
//!   write version; the ceremony's flip is a separate operator deploy.
//! - `wrangler.toml.example` documents the cron trigger and every
//!   ceremony variable, and no longer claims the rewrap job is
//!   unimplemented.
//!
//! Each test names the production change that would make it fail.

fn strip_comments(source: &str) -> String {
    // Keep string literals intact while removing comments, then remove
    // all whitespace. Mirrors the sibling Hyperdrive wiring pins.
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

fn worker_entry_production() -> String {
    let source = include_str!("../src/worker_entry.rs")
        .split_once("#[cfg(test)]")
        .expect("worker_entry test-module boundary")
        .0;
    strip_comments(source)
}

/// Extract the balanced `{ ... }` block whose opening brace is the
/// first one at or after `from` (byte index into `source`).
fn balanced_block(source: &str, from: usize) -> &str {
    let start = source[from..]
        .find('{')
        .map(|offset| from + offset)
        .expect("block opening brace");
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, byte) in source.bytes().enumerate().skip(start) {
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
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..=offset];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced block");
}

fn rewrap_store_impl(production: &str) -> &str {
    let marker = "implRewrapStoreforPgStore";
    let at = production
        .find(marker)
        .expect("PgStore must implement RewrapStore in worker_entry production code");
    balanced_block(production, at)
}

fn method_body<'a>(impl_block: &'a str, name: &str) -> &'a str {
    let marker = format!("asyncfn{name}(");
    let at = impl_block
        .find(&marker)
        .unwrap_or_else(|| panic!("RewrapStore impl must define async fn {name}"));
    balanced_block(impl_block, at)
}

/// Every trait method exists, runs inside the shared total budget, and
/// maps driver failure to the opaque store error.
///
/// Mutants killed: an unbounded statement path (a hung Hyperdrive call
/// wedging the scheduled invocation); a method surfacing driver detail.
#[test]
fn pgstore_implements_rewrap_store_with_bounded_opaque_methods() {
    let production = worker_entry_production();
    let impl_block = rewrap_store_impl(&production);
    for method in [
        "claim_batch",
        "reload_row",
        "cas_rewrap",
        "open_run",
        "load_open_run",
        "save_progress",
        "close_run",
    ] {
        let body = method_body(impl_block, method);
        assert!(
            body.contains("TOTAL_TIMEOUT_MS"),
            "{method} must run inside the shared TOTAL_TIMEOUT_MS budget"
        );
        assert!(
            body.contains("StoreError"),
            "{method} must map failures to the opaque StoreError/JobError::Store"
        );
        assert!(
            body.contains("self.connect()") || body.contains(".connect()"),
            "{method} must execute over its own Hyperdrive connection"
        );
    }
}

/// Claim executes the normative const; CAS executes the positional
/// rendering, binds captured fields after rewrapped fields (const
/// order), and distinguishes rewrapped / lost-claim / store failure.
///
/// Mutants killed: an inlined re-typed claim that drifts from the
/// runbook; the named-placeholder CAS sent to Postgres; zero rows
/// counted as progress; bind order scrambling the CAS predicates.
#[test]
fn claim_and_cas_use_the_normative_and_positional_sql() {
    let production = worker_entry_production();
    let impl_block = rewrap_store_impl(&production);

    let claim = method_body(impl_block, "claim_batch");
    assert!(
        claim.contains("CLAIM_BATCH_SQL"),
        "claim executes the normative const verbatim"
    );
    assert!(
        !claim.contains("\"SELECTuser_id"),
        "claim must not inline a copy of the normative statement"
    );

    let cas = method_body(impl_block, "cas_rewrap");
    assert!(
        cas.contains("CAS_REWRAP_POSTGRES_SQL"),
        "CAS executes the positional Postgres rendering"
    );
    assert!(
        !cas.contains("$captured_wrapped"),
        "named placeholders must not reach the Postgres driver"
    );
    let bind_order = [
        "rewrapped.kek_version",
        "rewrapped.dek_wrapped",
        "rewrapped.dek_wrapped_nonce",
        "captured.kek_version",
        "captured.dek_wrapped",
        "captured.dek_wrapped_nonce",
    ];
    let mut last = 0;
    for field in bind_order {
        let at = cas[last..]
            .find(field)
            .unwrap_or_else(|| panic!("cas_rewrap must bind {field} after the previous binds"))
            + last;
        last = at + field.len();
    }
    assert!(
        cas.contains("CasRewrapOutcome::Rewrapped"),
        "exactly one affected row is the rewrap"
    );
    assert!(
        cas.contains("CasRewrapOutcome::ClaimLost"),
        "zero affected rows is a lost claim, never progress"
    );
    assert!(
        cas.contains("Err(StoreError)"),
        "any other row count fails closed"
    );

    let reload = method_body(impl_block, "reload_row");
    assert!(
        reload.contains("WHEREuser_id=$1"),
        "reload reads one row by its bound user id"
    );
    assert!(
        reload.contains("dek_wrapped") && reload.contains("dek_wrapped_nonce"),
        "reload returns the captured-envelope fields for the retry"
    );
}

/// The ledger statements implement the `rotation_runs` contract the
/// orchestrator is written against, including unique-violation-only
/// `PassAlreadyRunning` discrimination at open.
///
/// Mutants killed: any insert error reported as "already running"
/// (hiding an outage as benign overlap); `save_progress` landing on a
/// closed run; `load_open_run` returning a finished run.
#[test]
fn ledger_methods_match_the_rotation_runs_contract() {
    let production = worker_entry_production();
    let impl_block = rewrap_store_impl(&production);

    let open = method_body(impl_block, "open_run");
    assert!(
        open.contains("validate_new_run("),
        "open validates the run shape shared with every store"
    );
    assert!(
        open.contains("INSERTINTOrotation_runs"),
        "open inserts the durable run record"
    );
    assert!(
        open.contains("JobError::PassAlreadyRunning"),
        "open conflict maps to PassAlreadyRunning"
    );
    assert!(
        open.contains("UNIQUE_VIOLATION") || open.contains("23505"),
        "only a unique violation (the partial open-run index) may map to PassAlreadyRunning"
    );
    assert!(
        open.contains("JobError::Store"),
        "every other open failure is the opaque store failure"
    );

    let load = method_body(impl_block, "load_open_run");
    assert!(load.contains("FROMrotation_runs"), "load reads the ledger");
    assert!(
        load.contains("env=$1") && load.contains("to_version=$2"),
        "load is keyed by the bound (env, to_version)"
    );
    assert!(
        load.contains("finished_atISNULL"),
        "load returns only the unfinished run"
    );

    let save = method_body(impl_block, "save_progress");
    assert!(
        save.contains("UPDATErotation_runs"),
        "save updates the ledger"
    );
    for column in [
        "cursor",
        "rows_done",
        "rows_failed",
        "rows_deferred",
        "claims_retried",
        "updated_at",
    ] {
        assert!(
            save.contains(column),
            "save persists {column} with the resumable progress"
        );
    }
    assert!(
        save.contains("finished_atISNULL"),
        "save never lands on a closed run"
    );
    assert!(
        save.contains("Err(StoreError)"),
        "zero saved rows (closed/unknown run) fails closed"
    );

    let close = method_body(impl_block, "close_run");
    assert!(
        close.contains("UPDATErotation_runs") && close.contains("finished_at="),
        "close stamps finished_at"
    );
    assert!(
        close.contains("finished_atISNULL"),
        "close only stamps an unfinished run (double close is a no-op)"
    );
}

/// The Hyperdrive rewrap path has no route for row material into logs:
/// no log/emit calls, no debug formatting (either spelling), and no
/// connection-string access outside `connect`.
///
/// Mutants killed: a per-row diagnostic printing a claim or envelope;
/// an error string built from driver/connection detail.
#[test]
fn rewrap_impl_has_no_log_or_debug_format_path() {
    let production = worker_entry_production();
    let impl_block = rewrap_store_impl(&production);
    for forbidden in [
        "println!",
        "eprintln!",
        "print!",
        "eprint!",
        "dbg!",
        "emit_record(",
        "console_",
        "tracing::",
        "log::",
        "{:?",
        "{:#?",
        "connection_string",
    ] {
        assert!(
            !impl_block.contains(forbidden),
            "RewrapStore impl must not contain {forbidden}"
        );
    }
}

/// The scheduled event is registered and delegates to glue that arms
/// through the pure lookup, stays inert while unarmed, and hands off
/// to `run_scheduled_rewrap` over `PgStore` — without ever flipping
/// the write version itself.
///
/// Mutants killed: the cron handler reimplementing orchestration
/// inline; an unarmed invocation still touching the store; scheduling
/// somehow rotating `VAULT_KEK_CURRENT` or retiring keys.
#[test]
fn scheduled_glue_arms_orchestrates_and_never_flips_the_write_version() {
    let lib = strip_comments(include_str!("../src/lib.rs"));
    assert!(
        lib.contains("event(scheduled)"),
        "lib.rs must register the scheduled event"
    );
    assert!(
        lib.contains("worker_entry::scheduled_rewrap("),
        "the scheduled handler delegates to the worker_entry glue"
    );

    let production = worker_entry_production();
    let marker = "pubasyncfnscheduled_rewrap(";
    let at = production
        .find(marker)
        .expect("worker_entry must expose pub async fn scheduled_rewrap");
    let glue = balanced_block(&production, at);
    assert!(
        glue.contains("scheduled_rewrap_arming_from_lookup("),
        "glue resolves the ceremony through the pure arming lookup"
    );
    assert!(
        glue.contains("ScheduledRewrapArming::Disabled"),
        "glue must handle the unarmed (inert) case explicitly"
    );
    assert!(
        glue.contains("ScheduledRewrapArming::Armed"),
        "glue must handle the armed ceremony explicitly"
    );
    assert!(
        glue.contains("run_scheduled_rewrap("),
        "glue hands the pass to the rotation orchestrator"
    );
    assert!(
        glue.contains("config_from_env(") || glue.contains("kek_set_from_lookup("),
        "glue builds the serving KEK config instead of reading raw secrets itself"
    );
    assert!(
        glue.contains("PgStore"),
        "glue runs the pass over the Hyperdrive PgStore"
    );
    assert!(
        !glue.contains("VAULT_KEK_CURRENT"),
        "scheduling never flips or reads the write version directly"
    );
    for forbidden in ["println!", "eprintln!", "dbg!", "{:?", "{:#?"] {
        assert!(
            !glue.contains(forbidden),
            "scheduled glue must not contain {forbidden}"
        );
    }
}

/// The example Worker config documents the cron trigger and every
/// ceremony variable, and stops claiming the job is unimplemented.
///
/// Mutants killed: code landing while the operator-facing config
/// still says there is no trigger (or documents none of its vars).
#[test]
fn wrangler_example_documents_the_cron_and_ceremony_vars() {
    let example = include_str!("../wrangler.toml.example");
    assert!(
        example.contains("[triggers]") && example.contains("crons"),
        "the example documents the cron trigger"
    );
    for var in [
        "VAULT_REWRAP_ENABLED",
        "VAULT_REWRAP_ENV",
        "VAULT_REWRAP_FROM_VERSION",
        "VAULT_REWRAP_TO_VERSION",
        "VAULT_REWRAP_DRY_RUN",
    ] {
        assert!(example.contains(var), "the example documents {var}");
    }
    let flattened: String = example
        .replace('#', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !flattened.contains("rewrap job is NOT implemented"),
        "the example must not keep claiming the rewrap job is unimplemented"
    );
}
