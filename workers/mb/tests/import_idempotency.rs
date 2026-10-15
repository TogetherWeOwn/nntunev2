//! Acceptance tests: container import idempotency with resume cursor and DLQ.
//!
//! Written first (RED): every test below fails until the import slice lands
//! three surfaces — a resume cursor for killed runs, a dead-letter table for
//! poison rows, and poison rerouting out of the upsert path. The Builder
//! builds on this branch, implements those surfaces, and never edits this
//! file.
//!
//! Why these tests read repo files instead of opening a database: the import
//! contract lives in versioned artifacts (the `containers/mb-import` job and
//! `migrations/*.sql`), and the shared CI job for this worker runs
//! `cargo test` with no database service. These tests pin the contract those
//! artifacts must satisfy; the Builder proves live behavior (dupe-rerun row
//! counts, kill-resume, DLQ query) against a scratch database on the build
//! card and pastes that evidence there.
//!
//! This file is self-contained on purpose: the worker crate ships as
//! `cdylib` for wasm, so integration tests cannot link it. No test here
//! imports the crate; each one asserts on the artifacts the Builder owns.

use std::path::{Path, PathBuf};

/// Curated dump tables the container import loads, in load order.
const TABLES: [&str; 3] = ["artist", "release", "recording"];

/// Locate the repo root by walking up from this package until the container
/// import job is found, so the tests run from any working directory.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("containers/mb-import/import.sh").is_file() {
            return dir;
        }
        if !dir.pop() {
            panic!("repo root not found above {}", env!("CARGO_MANIFEST_DIR"));
        }
    }
}

fn read_repo(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Concatenated contents of every `migrations/*.sql`, in filename order.
/// Migrations are forward-only, so the Builder's new tables land in a new
/// file and this helper picks them up without naming it.
fn migrations_sql() -> String {
    let dir = repo_root().join("migrations");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "expected migration files in {}",
        dir.display()
    );
    files
        .iter()
        .map(|path| {
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recursively collect every file under a repo-relative directory.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Lowercased concatenation of everything shipped with the container import
/// job (scripts, image definition, docs), so resume/DLQ wiring passes no
/// matter which job file the Builder puts it in.
fn import_job_text() -> String {
    let dir = repo_root().join("containers/mb-import");
    let mut files = Vec::new();
    collect_files(&dir, &mut files);
    assert!(!files.is_empty(), "expected job files in {}", dir.display());
    files
        .iter()
        .map(|path| {
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

/// Operator-facing docs that may carry the DLQ runbook query.
fn operator_docs() -> String {
    ["docs/ops.md", "containers/mb-import/README.md"]
        .iter()
        .map(|rel| read_repo(rel))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

/// First `radius` chars around the first occurrence of `needle`, for
/// proximity checks inside a large DDL blob.
fn window_around(haystack: &str, needle: &str, radius: usize) -> String {
    match haystack.find(needle) {
        Some(i) => haystack
            [i.saturating_sub(radius)..(i + needle.len() + radius).min(haystack.len())]
            .to_string(),
        None => String::new(),
    }
}

// --- Rerun idempotency: the same dump twice inserts zero dupes -------------

#[test]
fn rerun_same_dump_inserts_zero_dupes() {
    let import_sh = read_repo("containers/mb-import/import.sh");
    // Pin the staged upsert template: one COPY into a temp stage followed by
    // an upsert that only moves rows forward, so a rerun of the same dump
    // touches zero rows instead of duplicating or rewinding them.
    assert!(
        import_sh.contains("ON CONFLICT (mbid) DO UPDATE"),
        "rerun guard missing: staged upsert must carry ON CONFLICT (mbid)"
    );
    assert!(
        import_sh.contains("WHERE mb_${table}.updated_at < EXCLUDED.updated_at"),
        "rerun guard missing: upsert must move rows forward only \
         (WHERE <table>.updated_at < EXCLUDED.updated_at)"
    );
    for table in TABLES {
        assert!(
            import_sh.contains(&format!("mb_{table}")),
            "rerun guard missing: table mb_{table} is not covered by the upsert path"
        );
    }
    // A rerun only converges with zero dupes when poison rows leave the
    // upsert path for the DLQ: today one malformed row aborts the whole
    // per-table transaction, so the rerun replays the same abort instead of
    // loading the valid rows exactly once.
    let job = import_job_text();
    let migrations = migrations_sql().to_lowercase();
    assert!(
        job.contains("dlq") || migrations.contains("dlq"),
        "missing poison reroute: a rerun after a poison failure must route \
         bad rows to the DLQ instead of aborting the upsert; no job file or \
         migration mentions a dead-letter table yet"
    );
}

// --- Resume cursor: a killed import continues, not restarts -----------------

#[test]
fn import_resume_cursor_table_exists() {
    let migrations = migrations_sql().to_lowercase();
    assert!(
        migrations.contains("mb_import_cursor"),
        "missing resume cursor: migrations must create an import-progress \
         cursor table (mb_import_cursor) so a killed run knows what is \
         already loaded; only the replication packet cursor \
         (mb_import_state) exists today"
    );
    let ddl = window_around(&migrations, "mb_import_cursor", 1200);
    for identity in ["table_name", "file_name"] {
        if ddl.contains(identity) {
            for progress in ["rows_done", "offset", "position", "last_key"] {
                if ddl.contains(progress) {
                    assert!(
                        ddl.contains("updated_at"),
                        "cursor table must carry an updated_at timestamp"
                    );
                    return;
                }
            }
        }
    }
    panic!(
        "cursor table must record per-table/file progress \
         (table_name/file_name + rows_done/offset/position/last_key + \
         updated_at); got: {ddl}"
    );
}

#[test]
fn killed_import_resumes_without_full_restart() {
    // The cursor is only a resume cursor when the job itself consults it:
    // schema without job wiring still restarts from scratch after a kill.
    let job = import_job_text();
    assert!(
        job.contains("mb_import_cursor"),
        "missing resume wiring: nothing under containers/mb-import/ consults \
         the resume cursor, so a kill -9 still forces a full restart; the \
         import path must read the cursor and load only what is missing \
         (prove the live kill-resume against a scratch database on the \
         build card)"
    );
}

// --- DLQ: poison records land with reason + raw key, operator-queryable ----

#[test]
fn poison_records_land_in_dlq_with_reason_and_raw_key() {
    let migrations = migrations_sql().to_lowercase();
    assert!(
        migrations.contains("mb_import_dlq"),
        "missing DLQ table: migrations must create mb_import_dlq; poison \
         records have nowhere to go today"
    );
    let ddl = window_around(&migrations, "mb_import_dlq", 1500);
    assert!(
        ddl.contains("reason") && ddl.contains("text not null"),
        "DLQ must carry a NOT NULL reason column explaining why the row was \
         rejected; got: {ddl}"
    );
    assert!(
        ddl.contains("raw_key"),
        "DLQ must carry the raw record key (raw_key) identifying the \
         rejected dump row; got: {ddl}"
    );
    assert!(
        ddl.contains("table_name") || ddl.contains("source_table"),
        "DLQ must record which curated table the poison row came from \
         (table_name/source_table); got: {ddl}"
    );
}

#[test]
fn dlq_reprocessing_does_not_duplicate_rows() {
    let migrations = migrations_sql().to_lowercase();
    assert!(
        migrations.contains("mb_import_dlq"),
        "missing DLQ table: migrations must create mb_import_dlq first"
    );
    let ddl = window_around(&migrations, "mb_import_dlq", 1500);
    assert!(
        ddl.contains("unique") || ddl.contains("primary key"),
        "DLQ must deduplicate reprocessed poison rows with a uniqueness \
         guard on the record identity (UNIQUE/PRIMARY KEY over raw_key); \
         without it every rerun double-inserts the same poison row; \
         got: {ddl}"
    );
}

#[test]
fn dlq_is_queryable_by_operator_read_only() {
    // Least privilege: the operator role reads with SELECT only (runtime is
    // CRUD-without-DDL, read-only is SELECT-only), so the runbook query must
    // be a plain SELECT the read-only role can run — never DML/DDL.
    let docs = operator_docs();
    assert!(
        docs.contains("dlq"),
        "missing operator DLQ query: the runbook (docs/ops.md or \
         containers/mb-import/README.md) must document how an operator \
         lists DLQ rows; nothing mentions the DLQ yet"
    );
    let queries: Vec<&str> = docs
        .lines()
        .filter(|line| line.contains("select") && line.contains("dlq"))
        .collect();
    assert!(
        !queries.is_empty(),
        "operator DLQ access must be a documented SELECT over the DLQ table"
    );
    for query in queries {
        for forbidden in [
            "insert", "update", "delete", "drop", "alter", "create", "truncate", "grant",
        ] {
            assert!(
                !query.contains(forbidden),
                "operator DLQ query must stay read-only for the least-privilege \
                 matrix, but it contains {forbidden}: {query}"
            );
        }
        assert!(
            query.contains("reason"),
            "operator DLQ query should filter/order by reason so on-call can \
             triage poison rows; got: {query}"
        );
    }
}
