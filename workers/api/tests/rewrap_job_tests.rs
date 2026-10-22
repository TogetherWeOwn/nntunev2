//! Bounded/resumable KEK re-wrap job over a synthetic staging-equivalent
//! store (`workers/api/src/rotation.rs`,
//! `docs/security/kek-rotation.md` §6).
//!
//! These are the implementer's integration tests: a rusqlite fixture stands
//! in for the PlanetScale/Hyperdrive vault (same `provider_vaults` columns
//! via migrations 0002 + 0015, same claim/CAS semantics), and the fake
//! executes the job's SQL consts so drift between the job and the normative
//! runbook fails loudly. Fixtures are synthetic only (public repo: never
//! real secrets, never real user ids); keys are random per test.
//!
//! Independent acceptance coverage lives with the Test Author; this file
//! must not collide with their `*_acceptance.rs` files.

use nntune_api::handlers::{Config, StoreError};
use nntune_api::rotation::{
    CasRewrapOutcome, JobError, KekSource, PassReport, RewrapClaim, RewrapParams, RewrapStore,
    RewrappedEnvelope, RunRecord, CAS_REWRAP_SQL, CLAIM_BATCH_SQL, MAX_BATCH_LIMIT,
    MAX_ROW_ATTEMPTS, ROTATION_RUNS_DDL,
};
use nntune_api::vault::{decrypt_vault, encrypt_vault, VaultBlob};
use rand::RngCore;
use rusqlite::{params, Connection};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::task::{Context as TaskContext, Poll};

const OLD_VERSION: u32 = 1;
const NEW_VERSION: u32 = 2;
const NOW: u64 = 1_800_000_000;

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = TaskContext::from_waker(waker);
    let mut future = Box::pin(future);
    for _ in 0..10_000 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
    panic!("deterministic fake unexpectedly awaited external work");
}

fn random_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

fn runbook_text() -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/security/kek-rotation.md"),
    )
    .expect("read KEK rotation runbook")
}

fn fixture_db() -> Connection {
    let db = Connection::open_in_memory().expect("open fixture DB");
    let initial = std::fs::read_to_string(migrations_dir().join("0002_auth_vault.sql"))
        .expect("read initial vault migration");
    db.execute_batch(&initial)
        .expect("apply initial vault migration");
    let follow_up = std::fs::read_to_string(migrations_dir().join("0015_kek_rotation.sql"))
        .expect("read KEK rotation follow-up migration");
    // SQLite rejects `ADD COLUMN IF NOT EXISTS`; strip it for this fixture
    // only (the acceptance file pins the raw Postgres text).
    db.execute_batch(&follow_up.replace("ADD COLUMN IF NOT EXISTS ", "ADD COLUMN "))
        .expect("apply KEK rotation follow-up migration");
    // The job's progress DDL must execute verbatim on the fixture: it is
    // written against the Postgres/sqlite overlap on purpose.
    db.execute_batch(ROTATION_RUNS_DDL)
        .expect("rotation_runs DDL must execute on the fixture");
    db
}

fn seed_user(db: &Connection, user_id: &str) {
    db.execute(
        "INSERT INTO users (id, email, created_at) VALUES (?1, ?2, ?3)",
        params![user_id, format!("{user_id}@example.test"), 10],
    )
    .expect("seed user");
}

fn insert_vault_row(db: &Connection, user_id: &str, blob: &VaultBlob, updated_at: i64) {
    db.execute(
        "INSERT INTO provider_vaults
            (user_id, kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            user_id,
            blob.kek_version as i64,
            blob.dek_wrapped_nonce_b64url,
            blob.dek_wrapped_b64url,
            blob.payload_nonce_b64url,
            blob.payload_b64url,
            updated_at,
        ],
    )
    .expect("seed encrypted vault row");
}

fn vault_row(db: &Connection, user_id: &str) -> (VaultBlob, i64, Option<i64>) {
    db.query_row(
        "SELECT kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at, rewrapped_at
           FROM provider_vaults WHERE user_id = ?1",
        [user_id],
        |row| {
            Ok((
                VaultBlob {
                    kek_version: row.get::<_, i64>(0)? as u32,
                    dek_wrapped_nonce_b64url: row.get(1)?,
                    dek_wrapped_b64url: row.get(2)?,
                    payload_nonce_b64url: row.get(3)?,
                    payload_b64url: row.get(4)?,
                },
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )
    .expect("load vault fixture row")
}

fn count_where(db: &Connection, predicate: &str) -> i64 {
    db.query_row(
        &format!("SELECT COUNT(*) FROM provider_vaults WHERE {predicate}"),
        [],
        |row| row.get(0),
    )
    .expect("count vault rows")
}

/// Synthetic key set for the job: random per-test keys, no fixture reuse.
struct FakeKeks {
    keys: HashMap<u32, [u8; 32]>,
}

impl FakeKeks {
    fn pair() -> (Self, [u8; 32], [u8; 32]) {
        let old = random_key();
        let new = random_key();
        let mut keys = HashMap::new();
        keys.insert(OLD_VERSION, old);
        keys.insert(NEW_VERSION, new);
        (Self { keys }, old, new)
    }
}

impl KekSource for FakeKeks {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        self.keys.get(&version)
    }
}

/// Concurrent-write injector: before the CAS for `user_id`, either replace
/// the row envelope (fresh DEK + payload under the configured PUT version,
/// as a vault PUT would) or delete the row outright, `remaining` times. A
/// same-version racing PUT loses the CAS by design; a PUT already at the
/// target version (or a delete) exercises the Gone path instead.
struct ContentionRule {
    user_id: String,
    remaining: u64,
    delete: bool,
}

/// rusqlite-backed [`RewrapStore`]: executes the job's SQL consts with
/// `$name` placeholders substituted positionally, so the consts stay pinned
/// to real behavior rather than drifting from it.
struct SqliteRewrapStore<'a> {
    db: &'a Connection,
    run_seq: Cell<u64>,
    contention: RefCell<Option<ContentionRule>>,
    put_kek: [u8; 32],
    put_version: u32,
}

impl<'a> SqliteRewrapStore<'a> {
    fn new(db: &'a Connection, put_kek: [u8; 32], put_version: u32) -> Self {
        Self {
            db,
            run_seq: Cell::new(0),
            contention: RefCell::new(None),
            put_kek,
            put_version,
        }
    }

    fn contend(&self, user_id: &str, remaining: u64) {
        *self.contention.borrow_mut() = Some(ContentionRule {
            user_id: user_id.to_string(),
            remaining,
            delete: false,
        });
    }

    fn contend_delete(&self, user_id: &str) {
        *self.contention.borrow_mut() = Some(ContentionRule {
            user_id: user_id.to_string(),
            remaining: 1,
            delete: true,
        });
    }

    fn maybe_race(&self, user_id: &str) {
        let mut guard = self.contention.borrow_mut();
        let Some(rule) = guard.as_mut() else {
            return;
        };
        if rule.user_id != user_id || rule.remaining == 0 {
            return;
        }
        rule.remaining -= 1;
        if rule.delete {
            let removed = self
                .db
                .execute("DELETE FROM provider_vaults WHERE user_id = ?1", [user_id])
                .expect("racing delete removes the row");
            assert_eq!(removed, 1, "racing delete must remove exactly one row");
            return;
        }
        let replacement = encrypt_vault(
            &self.put_kek,
            self.put_version,
            user_id,
            format!("racing payload for {user_id}").as_bytes(),
        )
        .expect("encrypt racing PUT envelope");
        let changed = self
            .db
            .execute(
                "UPDATE provider_vaults
                    SET kek_version = ?1, dek_wrapped_nonce = ?2, dek_wrapped = ?3,
                        payload_nonce = ?4, payload = ?5, updated_at = ?6
                  WHERE user_id = ?7",
                params![
                    replacement.kek_version as i64,
                    replacement.dek_wrapped_nonce_b64url,
                    replacement.dek_wrapped_b64url,
                    replacement.payload_nonce_b64url,
                    replacement.payload_b64url,
                    NOW as i64 - 5,
                    user_id,
                ],
            )
            .expect("racing PUT replaces the row");
        assert_eq!(changed, 1, "racing PUT must replace exactly one row");
    }
}

fn sqlite_of(template: &str, bindings: &[(&str, String)]) -> (String, Vec<rusqlite::types::Value>) {
    // Substitute `$name` placeholders positionally; an unknown placeholder
    // fails loudly so a renamed binding cannot silently change the SQL.
    // All-digit bindings ride as integers (versions, limits, timestamps);
    // everything else rides as text (user ids, wrap ciphertext).
    let mut sql = String::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    let mut rest = template;
    while let Some(pos) = rest.find('$') {
        sql.push_str(&rest[..pos]);
        let tail = &rest[pos + 1..];
        let name_len = tail
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()))
            .unwrap_or(tail.len());
        let name = &tail[..name_len];
        let raw = bindings
            .iter()
            .find(|(key, _)| *key == name)
            .unwrap_or_else(|| panic!("unbound placeholder ${name} in job SQL"))
            .1
            .clone();
        let value = match raw.parse::<i64>() {
            Ok(number) => rusqlite::types::Value::Integer(number),
            Err(_) => rusqlite::types::Value::Text(raw),
        };
        sql.push('?');
        values.push(value);
        rest = &tail[name_len..];
    }
    sql.push_str(rest);
    (sql, values)
}

impl RewrapStore for SqliteRewrapStore<'_> {
    async fn claim_batch(
        &self,
        to_version: u32,
        after_user_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<RewrapClaim>, StoreError> {
        let (sql, values) = sqlite_of(
            CLAIM_BATCH_SQL,
            &[
                ("1", to_version.to_string()),
                ("2", after_user_id.unwrap_or("").to_string()),
                ("3", limit.to_string()),
            ],
        );
        let mut stmt = self.db.prepare(&sql).map_err(|_| StoreError)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok(RewrapClaim {
                    user_id: row.get(0)?,
                    kek_version: row.get::<_, i64>(1)? as u32,
                    dek_wrapped: row.get(2)?,
                    dek_wrapped_nonce: row.get(3)?,
                })
            })
            .map_err(|_| StoreError)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|_| StoreError)
    }

    async fn reload_row(&self, user_id: &str) -> Result<Option<RewrapClaim>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT user_id, kek_version, dek_wrapped, dek_wrapped_nonce
                   FROM provider_vaults WHERE user_id = ?1",
            )
            .map_err(|_| StoreError)?;
        let mut rows = stmt.query([user_id]).map_err(|_| StoreError)?;
        match rows.next().map_err(|_| StoreError)? {
            Some(row) => Ok(Some(RewrapClaim {
                user_id: row.get(0).map_err(|_| StoreError)?,
                kek_version: row.get::<_, i64>(1).map_err(|_| StoreError)? as u32,
                dek_wrapped: row.get(2).map_err(|_| StoreError)?,
                dek_wrapped_nonce: row.get(3).map_err(|_| StoreError)?,
            })),
            None => Ok(None),
        }
    }

    async fn cas_rewrap(
        &self,
        user_id: &str,
        captured: &RewrapClaim,
        rewrapped: &RewrappedEnvelope,
        now_secs: u64,
    ) -> Result<CasRewrapOutcome, StoreError> {
        // A test-configured racing write lands between the claim read and
        // the CAS write, exactly the interleaving the CAS guard exists for.
        self.maybe_race(user_id);
        let (sql, values) = sqlite_of(
            CAS_REWRAP_SQL,
            &[
                ("new", rewrapped.kek_version.to_string()),
                ("w", rewrapped.dek_wrapped.clone()),
                ("n", rewrapped.dek_wrapped_nonce.clone()),
                ("now", (now_secs as i64).to_string()),
                ("u", user_id.to_string()),
                ("old", captured.kek_version.to_string()),
                ("captured_wrapped", captured.dek_wrapped.clone()),
                ("captured_wrapped_nonce", captured.dek_wrapped_nonce.clone()),
            ],
        );
        let changed = self
            .db
            .execute(&sql, rusqlite::params_from_iter(values))
            .map_err(|_| StoreError)?;
        match changed {
            1 => Ok(CasRewrapOutcome::Rewrapped),
            0 => Ok(CasRewrapOutcome::ClaimLost),
            _ => Err(StoreError),
        }
    }

    async fn open_run(
        &self,
        env: &str,
        from_version: u32,
        to_version: u32,
        now_secs: u64,
    ) -> Result<RunRecord, JobError> {
        // Every backend enforces the shared run shape before inserting.
        nntune_api::rotation::validate_new_run(env, from_version, to_version)?;
        let seq = self.run_seq.get() + 1;
        self.run_seq.set(seq);
        let id = format!("run-{seq}");
        let inserted = self.db.execute(
            "INSERT INTO rotation_runs
                    (id, env, from_version, to_version, cursor, rows_done, rows_failed,
                     rows_deferred, claims_retried, started_at, updated_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4, NULL, 0, 0, 0, 0, ?5, ?5, NULL)",
            params![
                id,
                env,
                from_version as i64,
                to_version as i64,
                now_secs as i64,
            ],
        );
        match inserted {
            Ok(1) => Ok(RunRecord {
                id,
                env: env.to_string(),
                from_version,
                to_version,
                cursor: None,
                rows_done: 0,
                rows_failed: 0,
                rows_deferred: 0,
                claims_retried: 0,
                started_at: now_secs,
                updated_at: now_secs,
                finished_at: None,
            }),
            // The partial-unique open-run index fired: a pass is already
            // running for this (env, target). Any other outcome is a bug.
            _ => Err(JobError::PassAlreadyRunning),
        }
    }

    async fn load_open_run(
        &self,
        env: &str,
        to_version: u32,
    ) -> Result<Option<RunRecord>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, env, from_version, to_version, cursor, rows_done, rows_failed,
                        rows_deferred, claims_retried, started_at, updated_at, finished_at
                   FROM rotation_runs
                  WHERE env = ?1 AND to_version = ?2 AND finished_at IS NULL",
            )
            .map_err(|_| StoreError)?;
        let mut rows = stmt
            .query(params![env, to_version as i64])
            .map_err(|_| StoreError)?;
        match rows.next().map_err(|_| StoreError)? {
            Some(row) => Ok(Some(RunRecord {
                id: row.get(0).map_err(|_| StoreError)?,
                env: row.get(1).map_err(|_| StoreError)?,
                from_version: row.get::<_, i64>(2).map_err(|_| StoreError)? as u32,
                to_version: row.get::<_, i64>(3).map_err(|_| StoreError)? as u32,
                cursor: row.get(4).map_err(|_| StoreError)?,
                rows_done: row.get::<_, i64>(5).map_err(|_| StoreError)? as u64,
                rows_failed: row.get::<_, i64>(6).map_err(|_| StoreError)? as u64,
                rows_deferred: row.get::<_, i64>(7).map_err(|_| StoreError)? as u64,
                claims_retried: row.get::<_, i64>(8).map_err(|_| StoreError)? as u64,
                started_at: row.get::<_, i64>(9).map_err(|_| StoreError)? as u64,
                updated_at: row.get::<_, i64>(10).map_err(|_| StoreError)? as u64,
                finished_at: row
                    .get::<_, Option<i64>>(11)
                    .map_err(|_| StoreError)?
                    .map(|v| v as u64),
            })),
            None => Ok(None),
        }
    }

    async fn save_progress(&self, run: &RunRecord) -> Result<(), StoreError> {
        let changed = self
            .db
            .execute(
                "UPDATE rotation_runs
                    SET cursor = ?1, rows_done = ?2, rows_failed = ?3, rows_deferred = ?4,
                        claims_retried = ?5, updated_at = ?6
                  WHERE id = ?7 AND finished_at IS NULL",
                params![
                    run.cursor,
                    run.rows_done as i64,
                    run.rows_failed as i64,
                    run.rows_deferred as i64,
                    run.claims_retried as i64,
                    run.updated_at as i64,
                    run.id,
                ],
            )
            .map_err(|_| StoreError)?;
        if changed == 1 {
            Ok(())
        } else {
            // Zero rows: the run closed (or never existed) under this pass.
            // Fail closed rather than report progress into the void.
            Err(StoreError)
        }
    }

    async fn close_run(&self, run_id: &str, now_secs: u64) -> Result<(), StoreError> {
        self.db
            .execute(
                "UPDATE rotation_runs SET finished_at = ?1 WHERE id = ?2 AND finished_at IS NULL",
                params![now_secs as i64, run_id],
            )
            .map_err(|_| StoreError)?;
        // Idempotent: a double close (crash between close and ack) is a
        // no-op, never an error.
        Ok(())
    }
}

fn seed_vault_row(
    db: &Connection,
    user_id: &str,
    kek: &[u8; 32],
    version: u32,
    payload: &[u8],
) -> VaultBlob {
    seed_user(db, user_id);
    let blob = encrypt_vault(kek, version, user_id, payload).expect("encrypt fixture row");
    insert_vault_row(db, user_id, &blob, 20);
    blob
}

fn params_for(to_version: u32) -> RewrapParams {
    RewrapParams {
        to_version,
        batch_limit: MAX_BATCH_LIMIT,
        max_batches: 10,
        dry_run: false,
        now_secs: NOW,
    }
}

fn runbook_section<'a>(runbook: &'a str, header: &str) -> &'a str {
    let start = runbook
        .find(header)
        .unwrap_or_else(|| panic!("runbook must have a {header} section"));
    let section = &runbook[start..];
    let end = section[1..]
        .find("\n## ")
        .map(|offset| offset + 1)
        .unwrap_or(section.len());
    &section[..end]
}

#[test]
fn job_sql_consts_match_the_runbook_claim_and_cas_contract() {
    let runbook = runbook_text();
    let section_6 = runbook_section(&runbook, "## 6.");

    // The claim SELECT is normative §6 text with a bound batch size (`LIMIT
    // $3`, §6: batch ≤ 100 rows); the job const must carry it verbatim.
    let claim_start = section_6
        .find("SELECT user_id, kek_version, dek_wrapped, dek_wrapped_nonce")
        .expect("runbook §6 must document the claim SELECT");
    let claim_end = section_6[claim_start..]
        .find(';')
        .expect("runbook §6 claim must be a terminated statement");
    assert_eq!(
        CLAIM_BATCH_SQL,
        &section_6[claim_start..claim_start + claim_end + 1],
        "job claim SQL drifted from the normative §6 claim"
    );

    let update_start = section_6
        .find("UPDATE provider_vaults")
        .expect("runbook §6 must document the conditional rewrap UPDATE");
    let update_end = section_6[update_start..]
        .find(';')
        .expect("runbook §6 conditional UPDATE must be a terminated statement");
    assert_eq!(
        CAS_REWRAP_SQL,
        &section_6[update_start..update_start + update_end + 1],
        "job CAS SQL drifted from the normative §6 UPDATE"
    );
}

#[test]
fn rotation_runs_ddl_creates_the_designed_redacted_progress_record() {
    let db = fixture_db();
    let columns: Vec<(String, String)> = db
        .prepare("PRAGMA table_info(rotation_runs)")
        .expect("inspect rotation_runs columns")
        .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
        .expect("query rotation_runs columns")
        .map(|column| column.expect("read rotation_runs column"))
        .collect();
    let names: Vec<&str> = columns.iter().map(|(name, _)| name.as_str()).collect();
    // Designed §6 shape (id, env, from/to, cursor, counts, started/finished)
    // plus the reconciled drill counts (§6: rows_deferred, claims_retried)
    // and updated_at. No user lists, no key material columns — by
    // construction there is nowhere to put them.
    assert_eq!(
        names,
        vec![
            "id",
            "env",
            "from_version",
            "to_version",
            "cursor",
            "rows_done",
            "rows_failed",
            "rows_deferred",
            "claims_retried",
            "started_at",
            "updated_at",
            "finished_at",
        ]
    );
    for (name, column_type) in &columns {
        assert!(
            column_type == "TEXT" || column_type == "INTEGER" || column_type == "BIGINT",
            "unexpected rotation_runs column type for {name}: {column_type}"
        );
    }
}

#[test]
fn mixed_versions_rewrap_only_old_rows_and_preserve_payloads() {
    let db = fixture_db();
    let (keks, old_kek, new_kek) = FakeKeks::pair();
    let old_payload = b"old-version synthetic credentials";
    let current_payload = b"current-version synthetic credentials";
    seed_vault_row(&db, "old-user", &old_kek, OLD_VERSION, old_payload);
    seed_vault_row(&db, "current-user", &new_kek, NEW_VERSION, current_payload);

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report: PassReport = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds");
    assert_eq!(
        (report.scanned, report.done, report.failed, report.deferred),
        (1, 1, 0, 0)
    );
    assert!(report.complete, "single batch exhausts the fixture");

    let (old_after, updated_at, rewrapped_at) = vault_row(&db, "old-user");
    assert_eq!(old_after.kek_version, NEW_VERSION);
    assert_eq!(updated_at, NOW as i64);
    assert_eq!(rewrapped_at, Some(NOW as i64));
    assert_eq!(
        decrypt_vault(&new_kek, "old-user", &old_after).expect("decrypt under the new KEK"),
        old_payload
    );
    assert!(
        decrypt_vault(&old_kek, "old-user", &old_after).is_err(),
        "rewrapped row must decrypt only through its stamped version"
    );

    let (current_after, _, current_rewrapped_at) = vault_row(&db, "current-user");
    assert_eq!(current_after.kek_version, NEW_VERSION);
    assert_eq!(current_rewrapped_at, None, "current rows are never touched");
    assert_eq!(
        decrypt_vault(&new_kek, "current-user", &current_after).expect("current row intact"),
        current_payload
    );

    assert_eq!(
        (
            run.rows_done,
            run.rows_failed,
            run.rows_deferred,
            run.claims_retried
        ),
        (1, 0, 0, 0)
    );
    let persisted = block_on(store.load_open_run("staging", NEW_VERSION))
        .expect("load run")
        .expect("run persists");
    assert_eq!(persisted.rows_done, 1);
    assert_eq!(persisted.cursor.as_deref(), Some("old-user"));
}

#[test]
fn pass_resumes_from_persisted_cursor_without_rewrapping_twice() {
    let db = fixture_db();
    let (keks, old_kek, new_kek) = FakeKeks::pair();
    for index in 0..5 {
        let user_id = format!("user-{index:02}");
        seed_vault_row(
            &db,
            &user_id,
            &old_kek,
            OLD_VERSION,
            format!("synthetic credentials {index}").as_bytes(),
        );
    }

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    // Pass 1: one batch of two rows, then the bound stops the pass.
    let first = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &RewrapParams {
            batch_limit: 2,
            max_batches: 1,
            ..params_for(NEW_VERSION)
        },
    ))
    .expect("first pass succeeds");
    assert_eq!((first.scanned, first.done), (2, 2));
    assert!(!first.complete, "the bound must stop the pass early");
    assert_eq!(run.cursor.as_deref(), Some("user-01"));

    // A fresh pass handle resumes the SAME run from the persisted cursor
    // (Worker-eviction shape: no in-memory cursor survives).
    let mut resumed = block_on(store.load_open_run("staging", NEW_VERSION))
        .expect("load run")
        .expect("run persists across passes");
    assert_eq!(resumed.cursor.as_deref(), Some("user-01"));
    assert_eq!(resumed.rows_done, 2);
    let second = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut resumed,
        &params_for(NEW_VERSION),
    ))
    .expect("resumed pass succeeds");
    assert_eq!((second.scanned, second.done), (3, 3));
    assert!(second.complete);
    assert_eq!(resumed.rows_done, 5);

    assert_eq!(count_where(&db, "kek_version = 2"), 5);
    assert_eq!(count_where(&db, "rewrapped_at IS NULL"), 0);
    for index in 0..5 {
        let user_id = format!("user-{index:02}");
        let (blob, _, _) = vault_row(&db, &user_id);
        assert_eq!(
            decrypt_vault(&new_kek, &user_id, &blob).expect("decrypt resumed row"),
            format!("synthetic credentials {index}").as_bytes()
        );
    }
    block_on(store.close_run(&run.id, NOW + 1)).expect("close run");
    assert!(
        block_on(store.load_open_run("staging", NEW_VERSION))
            .expect("load after close")
            .is_none(),
        "a closed run is no longer resumable"
    );
}

#[test]
fn stale_claim_reloads_and_retries_preserving_the_newer_envelope() {
    let db = fixture_db();
    let (keks, old_kek, new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "race-user",
        &old_kek,
        OLD_VERSION,
        b"captured synthetic credentials",
    );

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    // One racing PUT lands between this row's claim and its CAS.
    store.contend("race-user", 1);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds");
    assert_eq!(
        (report.done, report.failed, report.deferred, report.retried),
        (1, 0, 0, 1),
        "the lost claim must reload and retry exactly once"
    );

    let (after, _, rewrapped_at) = vault_row(&db, "race-user");
    assert_eq!(after.kek_version, NEW_VERSION);
    assert_eq!(rewrapped_at, Some(NOW as i64));
    assert_eq!(
        decrypt_vault(&new_kek, "race-user", &after).expect("newer envelope survives"),
        b"racing payload for race-user",
        "the retry must rewrap the CURRENT envelope, never the stale one"
    );
}

#[test]
fn persistent_contention_defers_the_row_for_a_later_run() {
    let db = fixture_db();
    let (keks, old_kek, new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "hot-user",
        &old_kek,
        OLD_VERSION,
        b"hot synthetic credentials",
    );
    seed_vault_row(
        &db,
        "quiet-user",
        &old_kek,
        OLD_VERSION,
        b"quiet synthetic credentials",
    );

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    // Every CAS attempt for the hot row loses to a fresh PUT.
    store.contend("hot-user", u64::MAX);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds");
    assert_eq!((report.done, report.failed, report.deferred), (1, 0, 1));
    assert_eq!(report.retried, u64::from(MAX_ROW_ATTEMPTS));
    assert!(report.complete, "a hot row must not wedge the pass");

    let (hot_after, _, hot_rewrapped_at) = vault_row(&db, "hot-user");
    assert_eq!(
        hot_after.kek_version, OLD_VERSION,
        "the deferred row keeps its version for a later run"
    );
    assert_eq!(hot_rewrapped_at, None);
    // The racing PUTs (not the stale rewraps) own the row: it decrypts
    // under the OLD key with the racing payload intact.
    assert_eq!(
        decrypt_vault(&old_kek, "hot-user", &hot_after).expect("hot row intact"),
        b"racing payload for hot-user"
    );

    // A later run (contention gone) picks the deferred row up.
    block_on(store.close_run(&run.id, NOW + 1)).expect("close run");
    *store.contention.borrow_mut() = None;
    let mut later = block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW + 2))
        .expect("open later run");
    let later_report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut later,
        &params_for(NEW_VERSION),
    ))
    .expect("later pass succeeds");
    assert_eq!((later_report.done, later_report.deferred), (1, 0));
    let (hot_final, _, _) = vault_row(&db, "hot-user");
    assert_eq!(hot_final.kek_version, NEW_VERSION);
    assert_eq!(
        decrypt_vault(&new_kek, "hot-user", &hot_final).expect("deferred row rewrapped"),
        b"racing payload for hot-user"
    );
}

#[test]
fn poison_rows_are_counted_and_skipped_without_aborting_the_pass() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "good-user",
        &old_kek,
        OLD_VERSION,
        b"good synthetic credentials",
    );

    // Poison 1: malformed wrapped DEK (not base64).
    seed_user(&db, "corrupt-user");
    let mut corrupt = encrypt_vault(&old_kek, OLD_VERSION, "corrupt-user", b"corrupt").unwrap();
    corrupt.dek_wrapped_b64url = "!!!not-base64!!!".to_string();
    insert_vault_row(&db, "corrupt-user", &corrupt, 20);

    // Poison 2: KEK version with no provisioned key. Version 0 claims
    // (0 < to_version) but no key set provisions it, so the version-pinned
    // selector fails the row without attempting crypto.
    seed_vault_row(
        &db,
        "unknown-user",
        &old_kek,
        0,
        b"unknown-version synthetic",
    );

    // Poison 3: tampered wrap tag (valid alphabet, wrong ciphertext).
    seed_user(&db, "tampered-user");
    let mut tampered = encrypt_vault(&old_kek, OLD_VERSION, "tampered-user", b"tampered").unwrap();
    let mut chars: Vec<char> = tampered.dek_wrapped_b64url.chars().collect();
    let last = chars.len() - 1;
    chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
    tampered.dek_wrapped_b64url = chars.into_iter().collect();
    assert_ne!(
        tampered.dek_wrapped_b64url, corrupt.dek_wrapped_b64url,
        "tamper must keep the wrap decodable so the AEAD tag fails, not the base64"
    );
    insert_vault_row(&db, "tampered-user", &tampered, 20);

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds");
    assert_eq!(
        (report.scanned, report.done, report.failed, report.deferred),
        (4, 1, 3, 0)
    );
    assert!(report.complete, "poison rows must not abort the pass");

    for user_id in ["corrupt-user", "unknown-user", "tampered-user"] {
        let (blob, _, rewrapped_at) = vault_row(&db, user_id);
        assert_eq!(
            rewrapped_at, None,
            "poison row {user_id} must stay unmarked for the next run's dead-letter count"
        );
        assert!(
            blob.kek_version == OLD_VERSION || blob.kek_version == 0,
            "poison row {user_id} must keep its version"
        );
    }
}

#[test]
fn second_full_pass_is_a_noop() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "only-user",
        &old_kek,
        OLD_VERSION,
        b"only synthetic credentials",
    );

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let first = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("first pass succeeds");
    assert_eq!((first.done, first.complete), (1, true));
    block_on(store.close_run(&run.id, NOW + 1)).expect("close run");

    // The §5 invariant-3 second pass: a fresh run finds nothing left.
    let mut second_run = block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW + 2))
        .expect("open second run");
    let second = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut second_run,
        &params_for(NEW_VERSION),
    ))
    .expect("second pass succeeds");
    assert_eq!(
        (
            second.scanned,
            second.done,
            second.failed,
            second.deferred,
            second.complete
        ),
        (0, 0, 0, 0, true),
        "a converged vault must produce an empty complete pass"
    );
}

#[test]
fn dry_run_mutates_nothing_but_reports_would_rewrap_counts() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "dry-user",
        &old_kek,
        OLD_VERSION,
        b"dry synthetic credentials",
    );

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let transient = RunRecord::transient("staging", OLD_VERSION, NEW_VERSION, NOW);
    let mut run = transient;
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &RewrapParams {
            dry_run: true,
            ..params_for(NEW_VERSION)
        },
    ))
    .expect("dry run succeeds");
    assert_eq!((report.scanned, report.done), (1, 1));
    assert!(report.complete);

    let (after, updated_at, rewrapped_at) = vault_row(&db, "dry-user");
    assert_eq!(after.kek_version, OLD_VERSION, "dry run must not rewrap");
    assert_eq!(updated_at, 20, "dry run must not touch updated_at");
    assert_eq!(rewrapped_at, None);
    assert!(
        block_on(store.load_open_run("staging", NEW_VERSION))
            .expect("load run")
            .is_none(),
        "dry run must not open or persist a run record"
    );
    assert_eq!(
        (run.rows_done, run.rows_failed),
        (0, 0),
        "dry run must not accumulate into the transient record"
    );
}

#[test]
fn invalid_params_are_rejected_before_any_write() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "param-user",
        &old_kek,
        OLD_VERSION,
        b"param synthetic credentials",
    );
    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);

    // open_run rejects a non-rotation (from >= to) and an empty env.
    assert!(matches!(
        block_on(store.open_run("staging", NEW_VERSION, NEW_VERSION, NOW)),
        Err(JobError::InvalidParams(_))
    ));
    assert!(matches!(
        block_on(store.open_run("", OLD_VERSION, NEW_VERSION, NOW)),
        Err(JobError::InvalidParams(_))
    ));
    assert!(
        block_on(store.load_open_run("staging", NEW_VERSION))
            .expect("load run")
            .is_none(),
        "rejected opens must not persist a run"
    );

    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    for params in [
        RewrapParams {
            batch_limit: 0,
            ..params_for(NEW_VERSION)
        },
        RewrapParams {
            batch_limit: MAX_BATCH_LIMIT + 1,
            ..params_for(NEW_VERSION)
        },
        RewrapParams {
            max_batches: 0,
            ..params_for(NEW_VERSION)
        },
        // Target mismatch between the pass and its run: fail closed.
        RewrapParams {
            to_version: NEW_VERSION + 1,
            ..params_for(NEW_VERSION + 1)
        },
    ] {
        assert!(
            matches!(
                block_on(nntune_api::rotation::run_rewrap_pass(
                    &store, &keks, &mut run, &params
                )),
                Err(JobError::InvalidParams(_))
            ),
            "params must be rejected before any claim"
        );
    }
    // A live pass over a transient (never-opened) run is a caller bug: the
    // pass must refuse rather than rewrap rows it cannot record.
    let mut transient = RunRecord::transient("staging", OLD_VERSION, NEW_VERSION, NOW);
    assert!(matches!(
        block_on(nntune_api::rotation::run_rewrap_pass(
            &store,
            &keks,
            &mut transient,
            &params_for(NEW_VERSION)
        )),
        Err(JobError::InvalidParams(_))
    ));

    assert_eq!(count_where(&db, "rewrapped_at IS NOT NULL"), 0);
    let persisted = block_on(store.load_open_run("staging", NEW_VERSION))
        .expect("load run")
        .expect("run persists");
    assert_eq!(
        (persisted.rows_done, persisted.rows_failed, persisted.cursor),
        (0, 0, None)
    );
}

#[test]
fn unknown_target_key_fails_closed_before_any_claim() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "key-user",
        &old_kek,
        OLD_VERSION,
        b"key synthetic credentials",
    );
    // The key set holds OLD + NEW only; the run targets an unprovisioned 3.
    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let mut run = block_on(store.open_run("staging", OLD_VERSION, 3, NOW)).expect("open run");
    let outcome = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(3),
    ));
    assert!(
        matches!(outcome, Err(JobError::UnknownKekVersion(3))),
        "missing target key must fail the pass, got {outcome:?}"
    );
    assert_eq!(count_where(&db, "rewrapped_at IS NOT NULL"), 0);
}

#[test]
fn second_open_run_for_same_env_and_target_is_rejected() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "lock-user",
        &old_kek,
        OLD_VERSION,
        b"lock synthetic credentials",
    );
    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);

    let run = block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    assert!(
        matches!(
            block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)),
            Err(JobError::PassAlreadyRunning)
        ),
        "a second pass for the same (env, target) must be refused"
    );
    // A different environment is an independent rotation.
    let prod = block_on(store.open_run("prod", OLD_VERSION, NEW_VERSION, NOW)).expect("prod run");
    // A different target is an independent rotation.
    let v3 = block_on(store.open_run("staging", NEW_VERSION, 3, NOW)).expect("v3 run");
    block_on(store.close_run(&run.id, NOW + 1)).expect("close run");
    block_on(store.close_run(&run.id, NOW + 1)).expect("double close is a no-op");
    let reopened = block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW + 2))
        .expect("reopen after close");
    assert_ne!(reopened.id, run.id);

    // The singleton is per (env, target): the prod and v3 runs neither see
    // staging rows (all fixtures target v2) nor disturb them.
    let _ = (keks, prod, v3);
}

#[test]
fn empty_vault_completes_immediately() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    assert!(block_on(store.load_open_run("staging", NEW_VERSION))
        .expect("load run")
        .is_none());
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds");
    assert_eq!(
        (
            report.scanned,
            report.done,
            report.failed,
            report.deferred,
            report.complete
        ),
        (0, 0, 0, 0, true)
    );
}

#[test]
fn claim_and_envelope_debug_redacts_wrap_ciphertext() {
    let sentinel_wrapped = "d3JhcHBlZC1zZW50aW5lbC1jaXBoZXJ0ZXh0";
    let sentinel_nonce = "bm9uY2Utc2VudGluZWw";
    let claim = RewrapClaim {
        user_id: "redact-user".to_string(),
        kek_version: OLD_VERSION,
        dek_wrapped: sentinel_wrapped.to_string(),
        dek_wrapped_nonce: sentinel_nonce.to_string(),
    };
    let dumped = format!("{claim:?}");
    assert!(
        dumped.contains("redact-user"),
        "claim Debug must keep the routing identity: {dumped}"
    );
    assert!(
        !dumped.contains(sentinel_wrapped) && !dumped.contains(sentinel_nonce),
        "claim Debug must never carry wrap ciphertext: {dumped}"
    );
    let envelope = RewrappedEnvelope {
        kek_version: NEW_VERSION,
        dek_wrapped: sentinel_wrapped.to_string(),
        dek_wrapped_nonce: sentinel_nonce.to_string(),
    };
    let dumped = format!("{envelope:?}");
    assert!(
        !dumped.contains(sentinel_wrapped) && !dumped.contains(sentinel_nonce),
        "envelope Debug must never carry wrap ciphertext: {dumped}"
    );
    // The alternate format must redact too: `{:#?}` leaks through a
    // derived pretty printer when only `{}` is manual.
    let pretty = format!("{claim:#?}");
    assert!(
        !pretty.contains(sentinel_wrapped) && !pretty.contains(sentinel_nonce),
        "pretty Debug must redact as well: {pretty}"
    );
}

#[test]
fn job_module_emits_no_log_calls_and_errors_carry_no_ciphertext() {
    let source =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/rotation.rs"))
            .expect("read job module source");
    for forbidden in [
        "println!",
        "eprintln!",
        "dbg!",
        "tracing::",
        "log::",
        "logging::",
        "format!(\"{",
    ] {
        assert!(
            !source.contains(forbidden),
            "job module must not contain {forbidden} (ciphertext must have no log path)"
        );
    }
    // Job errors are scalar-only: no variant can format a blob.
    let sentinel = "c2VudGluZWwtY2lwaGVydGV4dA";
    for error in [
        JobError::InvalidParams("synthetic reason"),
        JobError::UnknownKekVersion(7),
        JobError::PassAlreadyRunning,
        JobError::Store,
        JobError::RandomnessUnavailable,
    ] {
        let dumped = format!("{error:?} {error}");
        assert!(
            !dumped.contains(sentinel),
            "job error must not carry ciphertext: {dumped}"
        );
    }
}

#[test]
fn worker_config_serves_as_the_job_key_source() {
    let old_kek = random_key();
    let new_kek = random_key();
    let config = Config::new(
        "example.test",
        "https://example.test",
        random_key().to_vec(),
        new_kek,
        NEW_VERSION,
        3600,
        false,
        false,
        None,
    )
    .expect("valid synthetic config")
    .with_prior_keks(vec![(OLD_VERSION, old_kek)]);
    // The job resolves row keys through the version-pinned selector: the
    // current key for the write version, retained priors otherwise, and no
    // fallback for unknown versions.
    assert_eq!(KekSource::kek(&config, NEW_VERSION), Some(&new_kek));
    assert_eq!(KekSource::kek(&config, OLD_VERSION), Some(&old_kek));
    assert_eq!(KekSource::kek(&config, 99), None);

    let db = fixture_db();
    seed_vault_row(
        &db,
        "selector-user",
        &old_kek,
        OLD_VERSION,
        b"selector synthetic credentials",
    );
    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &config,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds through the Config selector");
    assert_eq!((report.done, report.failed), (1, 0));
    let (after, _, _) = vault_row(&db, "selector-user");
    assert_eq!(
        decrypt_vault(&new_kek, "selector-user", &after).expect("decrypt rewrapped row"),
        b"selector synthetic credentials"
    );
}

/// A multi-batch dry-run must page through every batch exactly once: five
/// rows with `batch_limit: 2` report scanned/done 5/5 and complete, while
/// the run record (cursor, counts) and every vault byte stay untouched.
///
/// Mutant killed: a cursor that only advances for live passes re-claims
/// the first batch forever (scanned/done 20/20, complete false).
#[test]
fn dry_run_multi_batch_paginates_and_leaves_the_run_untouched() {
    let db = fixture_db();
    let (keks, old_kek, _new_kek) = FakeKeks::pair();
    let mut before = Vec::new();
    for index in 0..5 {
        let user_id = format!("page-{index:02}");
        let blob = seed_vault_row(
            &db,
            &user_id,
            &old_kek,
            OLD_VERSION,
            format!("page payload {index}").as_bytes(),
        );
        before.push((user_id, blob));
    }

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    let mut run = RunRecord::transient("staging", OLD_VERSION, NEW_VERSION, NOW);
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &RewrapParams {
            batch_limit: 2,
            max_batches: 10,
            dry_run: true,
            ..params_for(NEW_VERSION)
        },
    ))
    .expect("dry run succeeds");
    assert_eq!(
        (report.scanned, report.done, report.complete),
        (5, 5, true),
        "a dry-run must page forward, never re-scan the first batch"
    );
    assert_eq!(run.cursor, None, "dry run must not advance the run cursor");
    assert_eq!(
        (
            run.rows_done,
            run.rows_failed,
            run.rows_deferred,
            run.claims_retried
        ),
        (0, 0, 0, 0),
        "dry run must not accumulate into the transient record"
    );
    for (user_id, blob) in &before {
        let (after, updated_at, rewrapped_at) = vault_row(&db, user_id);
        assert_eq!(after.kek_version, OLD_VERSION, "dry run must not rewrap");
        assert_eq!(after.dek_wrapped, blob.dek_wrapped);
        assert_eq!(after.dek_wrapped_nonce, blob.dek_wrapped_nonce);
        assert_eq!(after.payload, blob.payload);
        assert_eq!(after.payload_nonce, blob.payload_nonce);
        assert_eq!(updated_at, 20, "dry run must not touch updated_at");
        assert_eq!(rewrapped_at, None);
    }
    assert!(
        block_on(store.load_open_run("staging", NEW_VERSION))
            .expect("load run")
            .is_none(),
        "dry run must not open or persist a run record"
    );
}

/// A row PUT at the target version between claim and CAS is Gone: it
/// counts scanned-only (no done/failed/deferred), still advances the
/// cursor, and the newer envelope survives untouched.
///
/// Mutant killed: treating the reload-at-target arm as Failed (or
/// retrying it) turns this pass red.
#[test]
fn row_put_at_target_mid_pass_counts_gone_and_keeps_the_newer_envelope() {
    let db = fixture_db();
    let (keks, old_kek, new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "gone-user",
        &old_kek,
        OLD_VERSION,
        b"stale synthetic credentials",
    );

    // The racing PUT lands at the TARGET version: by CAS time this row no
    // longer needs the pass.
    let store = SqliteRewrapStore::new(&db, new_kek, NEW_VERSION);
    store.contend("gone-user", 1);
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds");
    assert_eq!(
        (
            report.scanned,
            report.done,
            report.failed,
            report.deferred,
            report.retried
        ),
        (1, 0, 0, 0, 1),
        "a gone row counts scanned-only after exactly one lost claim"
    );
    assert!(report.complete);
    assert_eq!(
        run.cursor.as_deref(),
        Some("gone-user"),
        "a gone row must still advance the cursor past itself"
    );

    let (after, _, _) = vault_row(&db, "gone-user");
    assert_eq!(after.kek_version, NEW_VERSION);
    assert_eq!(
        decrypt_vault(&new_kek, "gone-user", &after).expect("newer envelope intact"),
        b"racing payload for gone-user",
        "the newer envelope must survive untouched, never rewrapped"
    );
}

/// A row deleted between claim and CAS is Gone: it counts scanned-only,
/// siblings still rewrap, and the cursor ends past the surviving row.
///
/// Mutant killed: treating the reload-None arm as Failed (or retrying
/// it) turns this pass red.
#[test]
fn row_deleted_mid_pass_counts_gone_without_touching_siblings() {
    let db = fixture_db();
    let (keks, old_kek, new_kek) = FakeKeks::pair();
    seed_vault_row(
        &db,
        "aaa-deleted",
        &old_kek,
        OLD_VERSION,
        b"doomed synthetic credentials",
    );
    seed_vault_row(
        &db,
        "zzz-kept",
        &old_kek,
        OLD_VERSION,
        b"kept synthetic credentials",
    );

    let store = SqliteRewrapStore::new(&db, old_kek, OLD_VERSION);
    store.contend_delete("aaa-deleted");
    let mut run =
        block_on(store.open_run("staging", OLD_VERSION, NEW_VERSION, NOW)).expect("open run");
    let report = block_on(nntune_api::rotation::run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &params_for(NEW_VERSION),
    ))
    .expect("pass succeeds");
    assert_eq!(
        (report.scanned, report.done, report.failed, report.deferred),
        (2, 1, 0, 0),
        "a deleted row counts scanned-only while its sibling rewraps"
    );
    assert!(report.complete);
    assert_eq!(
        run.cursor.as_deref(),
        Some("zzz-kept"),
        "the cursor must end past the surviving row"
    );
    assert_eq!(
        count_where(&db, "user_id = 'aaa-deleted'"),
        0,
        "the deleted row must stay deleted"
    );
    let (kept, _, kept_rewrapped_at) = vault_row(&db, "zzz-kept");
    assert_eq!(kept.kek_version, NEW_VERSION);
    assert_eq!(kept_rewrapped_at, Some(NOW as i64));
    assert_eq!(
        decrypt_vault(&new_kek, "zzz-kept", &kept).expect("sibling rewrapped"),
        b"kept synthetic credentials"
    );
}
