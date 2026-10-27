//! Acceptance tests for the production `rotation_runs` migration: the
//! durable re-wrap ledger as the next repo-root app migration after
//! 0022, reconciled with the fixture-validated `ROTATION_RUNS_DDL`
//! record in `workers/api/src/rotation.rs`, plus the runbook's
//! truthfulness constraints for the trigger slice
//! (`docs/security/kek-rotation.md` sections 6-8).
//!
//! Evidence split, stated plainly: the SQLite execution below is a
//! stand-in that shares the Postgres/SQLite SQL overlap — it is NOT
//! proof of Postgres execution. The Postgres proof for this table and
//! for the positional claim/CAS/ledger shapes lives in
//! `workers/api/tests/sql/rotation_runs.sql`, which the existing
//! migration lane runs against a real scratch Postgres after applying
//! every root migration in order.
//!
//! The migration is discovered by filename suffix (`*_rotation_runs.sql`),
//! not by a fixed number: its number must be unique, greater than 0022,
//! and listed after `0022_library_artifacts.sql` in the manifest with
//! `target = "app"`, so a concurrent migration landing first does not
//! silently renumber this contract away.
//!
//! Each test names the production change that would make it fail.

use nntune_api::rotation::ROTATION_RUNS_DDL;
use rusqlite::{params, Connection};
use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn migrations_dir() -> PathBuf {
    workspace_root().join("migrations")
}

fn migration_number(filename: &str) -> u32 {
    let (number, _) = filename
        .split_once('_')
        .unwrap_or_else(|| panic!("migration filename must have a numeric prefix: {filename}"));
    assert_eq!(
        number.len(),
        4,
        "migration prefix must be four digits: {filename}"
    );
    number.parse().expect("migration prefix is numeric")
}

fn migration_files() -> Vec<String> {
    let mut files = fs::read_dir(migrations_dir())
        .expect("read migrations directory")
        .map(|entry| entry.expect("migration directory entry").path())
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "sql")
        })
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("migration filename is UTF-8")
                .to_owned()
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// Exactly one `*_rotation_runs.sql` must exist; these tests were
/// written when none did.
fn rotation_runs_migration() -> (String, String) {
    let matches = migration_files()
        .into_iter()
        .filter(|filename| filename.ends_with("_rotation_runs.sql"))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one *_rotation_runs.sql production migration, found {matches:?}"
    );
    let filename = matches[0].clone();
    let sql =
        fs::read_to_string(migrations_dir().join(&filename)).expect("read rotation_runs migration");
    (filename, sql)
}

struct ManifestEntry {
    file: String,
    target: String,
    sha256: String,
}

fn manifest_entries() -> Vec<ManifestEntry> {
    let manifest = fs::read_to_string(migrations_dir().join("manifest.toml"))
        .expect("read migration manifest");
    manifest
        .split("[[migration]]")
        .skip(1)
        .map(|block| {
            let grab = |key: &str| {
                block
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once('=')?;
                        (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
                    })
                    .unwrap_or_else(|| panic!("manifest entry is missing {key}"))
            };
            ManifestEntry {
                file: grab("file"),
                target: grab("target"),
                sha256: grab("sha256"),
            }
        })
        .collect()
}

/// Column names of a `CREATE TABLE ... rotation_runs (...)` statement,
/// in declaration order. Shared by the production migration and the
/// `ROTATION_RUNS_DDL` const so the two shapes must reconcile exactly.
fn created_columns(sql: &str) -> Vec<String> {
    let create_at = sql
        .find("CREATE TABLE")
        .expect("DDL creates the rotation_runs table");
    let table_at = sql[create_at..]
        .find("rotation_runs")
        .map(|offset| create_at + offset)
        .expect("DDL names the rotation_runs table");
    let open = sql[table_at..]
        .find('(')
        .map(|offset| table_at + offset)
        .expect("column list opens");
    let mut depth = 0;
    let mut close = None;
    for (offset, ch) in sql.char_indices().skip_while(|(index, _)| *index < open) {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &sql[open + 1..close.expect("column list closes")];
    let mut columns = Vec::new();
    let mut part_start = 0;
    let mut part_depth = 0;
    let mut parts = Vec::new();
    for (offset, ch) in body.char_indices() {
        match ch {
            '(' => part_depth += 1,
            ')' => part_depth -= 1,
            ',' if part_depth == 0 => {
                parts.push(&body[part_start..offset]);
                part_start = offset + 1;
            }
            _ => {}
        }
    }
    parts.push(&body[part_start..]);
    for part in parts {
        let Some(first) = part.split_whitespace().next() else {
            continue;
        };
        let name = first.trim_matches(|c| c == '"' || c == '`').to_lowercase();
        if !matches!(
            name.as_str(),
            "primary" | "unique" | "check" | "foreign" | "constraint"
        ) {
            columns.push(name);
        }
    }
    columns
}

fn collapsed(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The ledger migration is the next discoverable app migration in root
/// ordering and in the manifest, exactly once, with a real checksum
/// slot (checksum equality itself is enforced by the migrations gate).
///
/// Mutants killed: the DDL landing only as a code const, under the
/// mirror target, or unlisted so deployment discovery skips it.
#[test]
fn rotation_runs_migration_is_the_next_discoverable_app_migration() {
    let (filename, _sql) = rotation_runs_migration();
    let number = migration_number(&filename);
    assert!(
        number > 22,
        "{filename} must follow 0022_library_artifacts.sql, the last migration before this slice"
    );
    assert_eq!(
        migration_files()
            .iter()
            .filter(|other| migration_number(other) == number)
            .count(),
        1,
        "migration number {number:04} must be unique"
    );

    let entries = manifest_entries();
    let listed = entries
        .iter()
        .filter(|entry| entry.file == filename)
        .collect::<Vec<_>>();
    assert_eq!(listed.len(), 1, "{filename} listed exactly once");
    assert_eq!(
        listed[0].target, "app",
        "rotation_runs belongs to the app database with provider_vaults"
    );
    assert!(
        listed[0].sha256.len() == 64
            && listed[0]
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && listed[0].sha256.chars().any(|c| c != '0'),
        "manifest checksum must be a real lowercase sha256, not a placeholder"
    );
    let own_position = entries
        .iter()
        .position(|entry| entry.file == filename)
        .expect("listed");
    let previous_position = entries
        .iter()
        .position(|entry| entry.file == "0022_library_artifacts.sql")
        .expect("0022 still listed");
    assert!(
        own_position > previous_position,
        "manifest entry lands after 0022 in apply order"
    );
    let numbers = entries
        .iter()
        .map(|entry| migration_number(&entry.file))
        .collect::<Vec<_>>();
    assert!(
        numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "manifest numbers stay strictly increasing"
    );
}

/// The production DDL carries the same redacted record the fixture
/// proved: same columns in the same order, epoch-`BIGINT` timestamps,
/// defaulted counters, and the partial-unique open-run guard — all
/// replay-safe (`IF NOT EXISTS`), because the migration lane reapplies
/// every migration and the Postgres disaster-recovery path replays too.
///
/// Mutants killed: a Postgres-native redesign drifting from
/// `RunRecord` (a timestamptz/serial ledger the store cannot bind);
/// dropping `rows_deferred`/`claims_retried`/`updated_at`; a plain
/// (non-partial) unique index that forbids a second ceremony forever.
#[test]
fn production_ddl_reconciles_with_the_fixture_record() {
    let (_filename, sql) = rotation_runs_migration();
    assert_eq!(
        created_columns(&sql),
        created_columns(ROTATION_RUNS_DDL),
        "production columns reconcile with ROTATION_RUNS_DDL in order"
    );
    let flat = collapsed(&sql);
    for fragment in [
        "CREATE TABLE IF NOT EXISTS rotation_runs",
        "id TEXT PRIMARY KEY",
        "env TEXT NOT NULL",
        "from_version INTEGER NOT NULL",
        "to_version INTEGER NOT NULL",
        "cursor TEXT",
        "rows_done BIGINT NOT NULL DEFAULT 0",
        "rows_failed BIGINT NOT NULL DEFAULT 0",
        "rows_deferred BIGINT NOT NULL DEFAULT 0",
        "claims_retried BIGINT NOT NULL DEFAULT 0",
        "started_at BIGINT NOT NULL",
        "updated_at BIGINT NOT NULL",
        "finished_at BIGINT",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_rotation_runs_open",
        "ON rotation_runs (env, to_version)",
        "WHERE finished_at IS NULL",
    ] {
        assert!(
            flat.contains(fragment),
            "production DDL must contain: {fragment}"
        );
    }
    for sqlite_only in ["AUTOINCREMENT", "WITHOUT ROWID", "`"] {
        assert!(
            !sql.contains(sqlite_only),
            "production DDL must be Postgres SQL, not the stand-in dialect ({sqlite_only})"
        );
    }
}

/// The migration executes verbatim on the SQLite stand-in after 0002 +
/// 0015 and enforces at most one open run per `(env, to_version)`,
/// freeing the slot when a run closes. Stand-in evidence only; the
/// Postgres lane proves the same semantics on Postgres.
///
/// Mutants killed: DDL that no engine accepts; an index that is not
/// actually partial or not actually unique.
#[test]
fn migration_executes_on_the_sqlite_stand_in_and_enforces_one_open_run() {
    let (_filename, sql) = rotation_runs_migration();
    let db = Connection::open_in_memory().expect("open stand-in");
    let base = fs::read_to_string(workspace_root().join("migrations/0002_auth_vault.sql"))
        .expect("read base vault migration");
    db.execute_batch(&base).expect("apply base migration");
    let followup = fs::read_to_string(workspace_root().join("migrations/0015_kek_rotation.sql"))
        .expect("read rewrap follow-up migration");
    db.execute_batch(&followup.replace("ADD COLUMN IF NOT EXISTS ", "ADD COLUMN "))
        .expect("apply follow-up migration");
    db.execute_batch(&sql)
        .expect("production rotation_runs DDL executes verbatim on the stand-in");

    let open = |id: &str, env: &str, to: i64| {
        db.execute(
            "INSERT INTO rotation_runs
                (id, env, from_version, to_version, cursor, rows_done, rows_failed,
                 rows_deferred, claims_retried, started_at, updated_at, finished_at)
             VALUES (?1, ?2, 1, ?3, NULL, 0, 0, 0, 0, 1700000000, 1700000000, NULL)",
            params![id, env, to],
        )
    };
    open("run-one", "stand-in-env", 3).expect("first open run lands");
    let duplicate = open("run-two", "stand-in-env", 3);
    assert!(
        matches!(
            duplicate,
            Err(rusqlite::Error::SqliteFailure(failure, _))
                if failure.code == rusqlite::ErrorCode::ConstraintViolation
        ),
        "a second open run for the same (env, to_version) must violate the partial-unique index, got {duplicate:?}"
    );
    open("run-other-env", "other-env", 3).expect("another env is independent");
    open("run-other-target", "stand-in-env", 4).expect("another target is independent");

    // Progress persists on the unfinished run and the resume read sees it.
    db.execute(
        "UPDATE rotation_runs
            SET cursor = 'user-ledger-1', rows_done = 5, rows_failed = 1,
                rows_deferred = 2, claims_retried = 3, updated_at = 1700000100
          WHERE id = 'run-one' AND finished_at IS NULL",
        [],
    )
    .expect("save progress on the open run");
    let loaded: (String, Option<String>, i64, i64, i64, i64) = db
        .query_row(
            "SELECT id, cursor, rows_done, rows_failed, rows_deferred, claims_retried
               FROM rotation_runs
              WHERE env = 'stand-in-env' AND to_version = 3 AND finished_at IS NULL",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("load the open run");
    assert_eq!(
        loaded,
        (
            "run-one".to_string(),
            Some("user-ledger-1".to_string()),
            5,
            1,
            2,
            3
        )
    );

    // Closing frees the slot for the next ceremony; double close is a
    // zero-row no-op; a closed run accepts no further progress.
    let first_close = db
        .execute(
            "UPDATE rotation_runs SET finished_at = 1700000200
              WHERE id = 'run-one' AND finished_at IS NULL",
            [],
        )
        .expect("close the run");
    assert_eq!(first_close, 1);
    let second_close = db
        .execute(
            "UPDATE rotation_runs SET finished_at = 1700000300
              WHERE id = 'run-one' AND finished_at IS NULL",
            [],
        )
        .expect("double close runs");
    assert_eq!(second_close, 0, "double close changes nothing");
    let late_save = db
        .execute(
            "UPDATE rotation_runs SET rows_done = 99
              WHERE id = 'run-one' AND finished_at IS NULL",
            [],
        )
        .expect("late save runs");
    assert_eq!(late_save, 0, "progress never lands on a closed run");
    open("run-next", "stand-in-env", 3).expect("closed run frees the open slot");
}

fn runbook_text() -> String {
    fs::read_to_string(workspace_root().join("docs/security/kek-rotation.md"))
        .expect("read rotation runbook")
}

/// After this slice the runbook names the wiring that exists (the
/// migration and the scheduled orchestrator) and drops its
/// not-implemented claims for them — while the gates this slice does
/// NOT prove stay explicitly unchecked: the staging run of the
/// ceremony, retire-after retention, and the compromise runner.
///
/// Mutants killed: code landing while section 7/8 still claim nothing
/// is executable; an overclaim checking the staging/retire/compromise
/// boxes on the strength of code alone.
#[test]
fn runbook_keeps_unproven_gates_unchecked_and_names_the_wiring() {
    let (filename, _sql) = rotation_runs_migration();
    let runbook = runbook_text();
    assert!(
        runbook.contains(&filename),
        "runbook names the production rotation_runs migration"
    );
    assert!(
        runbook.contains("run_scheduled_rewrap"),
        "runbook names the scheduled orchestration entry point"
    );
    let flattened = collapsed(&runbook).to_lowercase();
    for stale in [
        "no production migration exists",
        "no scheduled trigger invokes",
    ] {
        assert!(
            !flattened.contains(stale),
            "runbook must not keep claiming: {stale}"
        );
    }

    let lines = runbook.lines().collect::<Vec<_>>();
    for gate in [
        "followed on staging",
        "Retire-after date recorded",
        "Compromise path",
    ] {
        let at = lines
            .iter()
            .position(|line| line.contains(gate))
            .unwrap_or_else(|| panic!("runbook keeps the gate item: {gate}"));
        let mut cursor = Some(at);
        let mut stamp = None;
        while let Some(index) = cursor {
            let trimmed = lines[index].trim_start();
            if trimmed.starts_with("- [") {
                stamp = Some(trimmed.to_string());
                break;
            }
            cursor = index.checked_sub(1);
        }
        let stamp = stamp.unwrap_or_else(|| panic!("gate {gate} is a checklist item"));
        assert!(
            stamp.starts_with("- [ ]"),
            "unproven gate stays unchecked: {gate} (found {stamp})"
        );
    }
}
