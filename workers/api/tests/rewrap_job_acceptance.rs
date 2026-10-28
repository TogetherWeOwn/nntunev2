//! Independent acceptance tests for the bounded/resumable DEK re-wrap job
//! (`workers/api/src/rotation.rs`, `docs/security/kek-rotation.md` §6).
//!
//! Second-implementation coverage over a synthetic in-memory sqlite vault:
//! every test builds its own fixture (migrations 0002 + 0015, then the job's
//! `ROTATION_RUNS_DDL` verbatim), drives `run_rewrap_pass` through a local
//! `RewrapStore` that executes the job's SQL consts, and asserts the
//! end-state a rotation drill would check. Keys are random per test; user ids
//! and payloads are synthetic.
//!
//! Each test names the production change that would make it fail. A test
//! that cannot fail is not a test.

use nntune_api::b64;
use nntune_api::handlers::StoreError;
use nntune_api::rotation::{
    run_rewrap_pass, validate_new_run, CasRewrapOutcome, JobError, KekSource, RewrapClaim,
    RewrapParams, RewrapStore, RewrappedEnvelope, RunRecord, CAS_REWRAP_SQL, CLAIM_BATCH_SQL,
    MAX_BATCH_LIMIT, ROTATION_RUNS_DDL,
};
use nntune_api::vault::{decrypt_vault, encrypt_vault, rewrap_dek, VaultBlob};
use rand::RngCore;
use rusqlite::{params, Connection};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::task::{Context as TaskCx, Poll};

const V1: u32 = 1;
const V2: u32 = 2;
const TARGET: u32 = 3;
const PIN: u64 = 1_800_000_000;
const SEED_UPDATED_AT: i64 = 20;

fn drive<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = TaskCx::from_waker(waker);
    let mut pinned = Box::pin(future);
    for _ in 0..10_000 {
        if let Poll::Ready(out) = pinned.as_mut().poll(&mut cx) {
            return out;
        }
    }
    panic!("acceptance store awaited outside the sqlite fixture");
}

fn fresh_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn acceptance_db() -> Connection {
    let db = Connection::open_in_memory().expect("open acceptance fixture");
    let base = std::fs::read_to_string(workspace_root().join("migrations/0002_auth_vault.sql"))
        .expect("read base vault migration");
    db.execute_batch(&base).expect("apply base vault migration");
    let followup =
        std::fs::read_to_string(workspace_root().join("migrations/0015_kek_rotation.sql"))
            .expect("read rotation follow-up migration");
    // SQLite rejects `ADD COLUMN IF NOT EXISTS`; strip it for this fixture only.
    db.execute_batch(&followup.replace("ADD COLUMN IF NOT EXISTS ", "ADD COLUMN "))
        .expect("apply rotation follow-up migration");
    db.execute_batch(ROTATION_RUNS_DDL)
        .expect("progress DDL must run verbatim on the fixture");
    db
}

fn seed_holder(db: &Connection, user_id: &str) {
    db.execute(
        "INSERT INTO users (id, email, created_at) VALUES (?1, ?2, ?3)",
        params![user_id, format!("{user_id}@example.test"), 10],
    )
    .expect("seed holder");
}

fn place_row(db: &Connection, user_id: &str, blob: &VaultBlob, updated_at: i64) {
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
    .expect("place vault row");
}

fn read_row(db: &Connection, user_id: &str) -> (VaultBlob, i64, Option<i64>) {
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
    .expect("read vault row")
}

fn place_fixture(
    db: &Connection,
    user_id: &str,
    kek: &[u8; 32],
    version: u32,
    payload: &[u8],
) -> VaultBlob {
    seed_holder(db, user_id);
    let blob = encrypt_vault(kek, version, user_id, payload).expect("encrypt fixture");
    place_row(db, user_id, &blob, SEED_UPDATED_AT);
    blob
}

fn tally(db: &Connection, predicate: &str) -> i64 {
    db.query_row(
        &format!("SELECT COUNT(*) FROM provider_vaults WHERE {predicate}"),
        [],
        |row| row.get(0),
    )
    .expect("tally vault rows")
}

fn assert_rows_equal(
    user_id: &str,
    left: &(VaultBlob, i64, Option<i64>),
    right: &(VaultBlob, i64, Option<i64>),
) {
    assert_eq!(left.0.kek_version, right.0.kek_version, "{user_id} version");
    assert_eq!(
        left.0.dek_wrapped_b64url, right.0.dek_wrapped_b64url,
        "{user_id} wrap"
    );
    assert_eq!(
        left.0.dek_wrapped_nonce_b64url, right.0.dek_wrapped_nonce_b64url,
        "{user_id} wrap nonce"
    );
    assert_eq!(
        left.0.payload_b64url, right.0.payload_b64url,
        "{user_id} payload"
    );
    assert_eq!(
        left.0.payload_nonce_b64url, right.0.payload_nonce_b64url,
        "{user_id} payload nonce"
    );
    assert_eq!(left.1, right.1, "{user_id} updated_at");
    assert_eq!(left.2, right.2, "{user_id} rewrapped_at");
}

/// Synthetic three-version key set: random per test, never reused.
struct TrialKeks {
    keys: HashMap<u32, [u8; 32]>,
}

impl TrialKeks {
    fn three() -> (Self, [u8; 32], [u8; 32], [u8; 32]) {
        let first = fresh_key();
        let second = fresh_key();
        let third = fresh_key();
        let keys = HashMap::from([(V1, first), (V2, second), (TARGET, third)]);
        (Self { keys }, first, second, third)
    }

    /// Ends only: the middle version is well-formed in fixtures but has no
    /// provisioned key, so rows at it fail per-row below the target.
    fn ends() -> (Self, [u8; 32], [u8; 32]) {
        let first = fresh_key();
        let third = fresh_key();
        let keys = HashMap::from([(V1, first), (TARGET, third)]);
        (Self { keys }, first, third)
    }
}

impl KekSource for TrialKeks {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        self.keys.get(&version)
    }
}

fn bind_sql(template: &str, pairs: &[(&str, String)]) -> (String, Vec<rusqlite::types::Value>) {
    // Record placeholder order first, then substitute the longest name first
    // so a short name ($n) can never corrupt a longer one ($new, $now).
    let mut order: Vec<String> = Vec::new();
    let mut scan = template;
    while let Some(dollar) = scan.find('$') {
        let tail = &scan[dollar + 1..];
        let len = tail
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()))
            .unwrap_or(tail.len());
        assert!(len > 0, "bare $ in job SQL");
        order.push(tail[..len].to_string());
        scan = &tail[len..];
    }
    let mut names: Vec<&str> = pairs.iter().map(|(name, _)| *name).collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let mut sql = template.to_string();
    for name in names {
        sql = sql.replace(&format!("${name}"), "?");
    }
    let values = order
        .iter()
        .map(|name| {
            let raw = pairs
                .iter()
                .find(|(key, _)| key == name)
                .unwrap_or_else(|| panic!("unbound ${name} in job SQL"))
                .1
                .clone();
            match raw.parse::<i64>() {
                Ok(number) => rusqlite::types::Value::Integer(number),
                Err(_) => rusqlite::types::Value::Text(raw),
            }
        })
        .collect();
    (sql, values)
}

/// sqlite-backed [`RewrapStore`] executing the job's SQL consts, with a
/// programmable racing PUT between claim and CAS.
struct AcceptanceStore<'a> {
    db: &'a Connection,
    seq: Cell<u64>,
    races: RefCell<HashMap<String, u32>>,
    race_kek: [u8; 32],
    race_version: u32,
}

impl<'a> AcceptanceStore<'a> {
    fn new(db: &'a Connection, race_kek: [u8; 32], race_version: u32) -> Self {
        Self {
            db,
            seq: Cell::new(0),
            races: RefCell::new(HashMap::new()),
            race_kek,
            race_version,
        }
    }

    fn race(&self, user_id: &str, times: u32) {
        self.races.borrow_mut().insert(user_id.to_string(), times);
    }

    fn land_racing_put(&self, user_id: &str) {
        let mut races = self.races.borrow_mut();
        let Some(remaining) = races.get_mut(user_id) else {
            return;
        };
        if *remaining == 0 {
            return;
        }
        *remaining -= 1;
        drop(races);
        let replacement = encrypt_vault(
            &self.race_kek,
            self.race_version,
            user_id,
            format!("newer payload for {user_id}").as_bytes(),
        )
        .expect("encrypt racing envelope");
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
                    SEED_UPDATED_AT,
                    user_id,
                ],
            )
            .expect("racing PUT lands");
        assert_eq!(changed, 1, "racing PUT must replace one row");
    }
}

impl RewrapStore for AcceptanceStore<'_> {
    async fn claim_batch(
        &self,
        to_version: u32,
        after_user_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<RewrapClaim>, StoreError> {
        let (sql, values) = bind_sql(
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
        self.land_racing_put(user_id);
        let (sql, values) = bind_sql(
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
        validate_new_run(env, from_version, to_version)?;
        let next = self.seq.get() + 1;
        self.seq.set(next);
        let id = format!("accept-run-{next}");
        match self.db.execute(
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
        ) {
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
            Ok(_) => Err(JobError::Store),
            Err(rusqlite::Error::SqliteFailure(failure, _))
                if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(JobError::PassAlreadyRunning)
            }
            Err(_) => Err(JobError::Store),
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
        Ok(())
    }
}

fn live_params(to_version: u32) -> RewrapParams {
    RewrapParams {
        to_version,
        batch_limit: MAX_BATCH_LIMIT,
        max_batches: 10,
        dry_run: false,
        now_secs: PIN,
    }
}

fn runbook_text() -> String {
    std::fs::read_to_string(workspace_root().join("docs/security/kek-rotation.md"))
        .expect("read rotation runbook")
}

fn section_six(runbook: &str) -> &str {
    let start = runbook.find("## 6.").expect("runbook keeps a section 6");
    let rest = &runbook[start..];
    let end = rest[1..]
        .find("\n## ")
        .map(|offset| offset + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Mixed versions below the target re-wrap DEK-only; rows at/above the
/// target pass through byte-identical.
///
/// Mutants killed: claim `<=` (the at-target row rewraps); CAS touching
/// payload columns (payload bytes change); version bump without
/// re-encryption (new-key decrypt fails); missing `rewrapped_at` write.
#[test]
fn mixed_versions_rewrap_below_target_only() {
    let db = acceptance_db();
    let (keks, first_kek, second_kek, third_kek) = TrialKeks::three();
    let above_kek = fresh_key();
    let below_old = place_fixture(&db, "below-old", &first_kek, V1, b"old synthetic payload");
    let below_mid = place_fixture(&db, "below-mid", &second_kek, V2, b"mid synthetic payload");
    let at_target = place_fixture(
        &db,
        "at-target",
        &third_kek,
        TARGET,
        b"target synthetic payload",
    );
    let above = place_fixture(
        &db,
        "above-target",
        &above_kek,
        TARGET + 2,
        b"above synthetic payload",
    );

    let store = AcceptanceStore::new(&db, first_kek, V1);
    let mut run = drive(store.open_run("acceptance", V1, TARGET, PIN)).expect("open run");
    let report = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &live_params(TARGET),
    ))
    .expect("pass runs");
    assert_eq!(
        (report.scanned, report.done, report.failed, report.deferred),
        (2, 2, 0, 0)
    );
    assert!(report.complete, "two rows fit in one batch");

    for (user_id, before, old_kek, payload) in [
        (
            "below-old",
            &below_old,
            &first_kek,
            b"old synthetic payload".as_slice(),
        ),
        (
            "below-mid",
            &below_mid,
            &second_kek,
            b"mid synthetic payload".as_slice(),
        ),
    ] {
        let (after, updated_at, rewrapped_at) = read_row(&db, user_id);
        assert_eq!(after.kek_version, TARGET, "{user_id} reaches the target");
        assert_ne!(
            after.dek_wrapped_b64url, before.dek_wrapped_b64url,
            "{user_id} wrap rotates"
        );
        assert_eq!(
            after.payload_b64url, before.payload_b64url,
            "{user_id} payload bytes identical"
        );
        assert_eq!(
            after.payload_nonce_b64url, before.payload_nonce_b64url,
            "{user_id} payload nonce identical"
        );
        assert_eq!(updated_at, PIN as i64);
        assert_eq!(rewrapped_at, Some(PIN as i64));
        assert_eq!(
            decrypt_vault(&third_kek, user_id, &after).expect("new-key decrypt"),
            payload
        );
        assert!(
            decrypt_vault(old_kek, user_id, &after).is_err(),
            "{user_id} no longer reads under the old key"
        );
    }

    for (user_id, before) in [("at-target", &at_target), ("above-target", &above)] {
        let after = read_row(&db, user_id);
        let expected = (before.clone(), SEED_UPDATED_AT, None::<i64>);
        assert_rows_equal(user_id, &after, &expected);
    }
}

/// A bound-stopped pass persists cursor + counts; a reloaded run finishes
/// with no recount and no double re-wrap.
///
/// Mutants killed: cursor never persisted (the resumed pass recounts the
/// poison row, failing twice); progress saved once per pass instead of per
/// batch (reloaded counts read zero); bound stop reported complete.
#[test]
fn bound_stopped_pass_resumes_without_recount_or_double_wrap() {
    let db = acceptance_db();
    let (keks, first_kek, _, third_kek) = TrialKeks::three();
    // The poison row sorts first: a pass that aborts instead of skipping
    // would never reach the good rows behind it.
    seed_holder(&db, "resume-aa-poison");
    let mut poison =
        encrypt_vault(&first_kek, V1, "resume-aa-poison", b"poison").expect("encrypt poison");
    poison.dek_wrapped_b64url = "!!!not-base64!!!".to_string();
    place_row(&db, "resume-aa-poison", &poison, SEED_UPDATED_AT);
    for index in 0..4 {
        let user_id = format!("resume-b-{index:02}");
        place_fixture(
            &db,
            &user_id,
            &first_kek,
            V1,
            format!("resume payload {index}").as_bytes(),
        );
    }

    let store = AcceptanceStore::new(&db, first_kek, V1);
    let mut run = drive(store.open_run("acceptance", V1, TARGET, PIN)).expect("open run");
    let first = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &RewrapParams {
            batch_limit: 2,
            max_batches: 1,
            ..live_params(TARGET)
        },
    ))
    .expect("first pass runs");
    assert_eq!((first.scanned, first.done, first.failed), (2, 1, 1));
    assert!(!first.complete, "the batch bound must stop the pass");
    assert_eq!(run.cursor.as_deref(), Some("resume-b-00"));
    assert_eq!((run.rows_done, run.rows_failed), (1, 1));
    let (done_after_pass_one, _, _) = read_row(&db, "resume-b-00");

    // Eviction shape: a fresh handle reloads the same run from the ledger.
    let mut resumed = drive(store.load_open_run("acceptance", TARGET))
        .expect("load run")
        .expect("run survives eviction");
    assert_eq!(resumed.cursor.as_deref(), Some("resume-b-00"));
    assert_eq!((resumed.rows_done, resumed.rows_failed), (1, 1));
    let second = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut resumed,
        &live_params(TARGET),
    ))
    .expect("resumed pass runs");
    assert_eq!((second.scanned, second.done, second.failed), (3, 3, 0));
    assert!(second.complete);
    assert_eq!((resumed.rows_done, resumed.rows_failed), (4, 1));

    let (done_final, _, _) = read_row(&db, "resume-b-00");
    assert_eq!(
        done_final.dek_wrapped_b64url, done_after_pass_one.dek_wrapped_b64url,
        "resumed pass must not rewrap twice"
    );
    for index in 0..4 {
        let user_id = format!("resume-b-{index:02}");
        let (blob, _, rewrapped_at) = read_row(&db, &user_id);
        assert_eq!(blob.kek_version, TARGET);
        assert_eq!(rewrapped_at, Some(PIN as i64));
        assert_eq!(
            decrypt_vault(&third_kek, &user_id, &blob).expect("decrypt resumed row"),
            format!("resume payload {index}").as_bytes()
        );
    }
    let (poison_after, _, poison_mark) = read_row(&db, "resume-aa-poison");
    assert_eq!(poison_after.kek_version, V1);
    assert_eq!(poison_mark, None);
}

/// A same-version PUT between claim and CAS loses with zero rows; the job
/// reloads, retries, and preserves the newer envelope.
///
/// Mutants killed: version-only CAS guard (the stale write matches and the
/// newer payload is destroyed); lost claim counted as progress; no
/// reload/retry (the row is left at the old version).
#[test]
fn stale_claim_loses_and_retries_on_current_envelope() {
    let db = acceptance_db();
    let (keks, first_kek, _, third_kek) = TrialKeks::three();
    place_fixture(&db, "race-row", &first_kek, V1, b"captured payload");
    let store = AcceptanceStore::new(&db, first_kek, V1);

    // Capture, then land a same-version PUT underneath the capture.
    let claimed = drive(store.claim_batch(TARGET, None, MAX_BATCH_LIMIT)).expect("claim");
    assert_eq!(claimed.len(), 1);
    let captured = claimed.into_iter().next().expect("one row");
    let newer =
        encrypt_vault(&first_kek, V1, "race-row", b"first newer payload").expect("newer PUT");
    let changed = db
        .execute(
            "UPDATE provider_vaults
                SET kek_version = ?1, dek_wrapped_nonce = ?2, dek_wrapped = ?3,
                    payload_nonce = ?4, payload = ?5, updated_at = ?6
              WHERE user_id = ?7",
            params![
                newer.kek_version as i64,
                newer.dek_wrapped_nonce_b64url,
                newer.dek_wrapped_b64url,
                newer.payload_nonce_b64url,
                newer.payload_b64url,
                SEED_UPDATED_AT,
                "race-row",
            ],
        )
        .expect("same-version PUT");
    assert_eq!(changed, 1);

    // A CAS built from the stale capture must lose with zero rows affected.
    let stale_blob = VaultBlob {
        kek_version: captured.kek_version,
        dek_wrapped_nonce_b64url: captured.dek_wrapped_nonce.clone(),
        dek_wrapped_b64url: captured.dek_wrapped.clone(),
        payload_nonce_b64url: String::new(),
        payload_b64url: String::new(),
    };
    let stale_rewrap = rewrap_dek(&first_kek, &third_kek, TARGET, "race-row", &stale_blob)
        .expect("stale rewrap computes");
    let stale_envelope = RewrappedEnvelope {
        kek_version: TARGET,
        dek_wrapped: stale_rewrap.dek_wrapped_b64url,
        dek_wrapped_nonce: stale_rewrap.dek_wrapped_nonce_b64url,
    };
    assert_eq!(
        drive(store.cas_rewrap("race-row", &captured, &stale_envelope, PIN))
            .expect("stale CAS runs"),
        CasRewrapOutcome::ClaimLost
    );
    let (kept, _, kept_mark) = read_row(&db, "race-row");
    assert_eq!(
        kept.payload_b64url, newer.payload_b64url,
        "newer envelope survives the stale CAS"
    );
    assert_eq!(kept_mark, None);

    // The job path: a racing PUT inside the pass loses once, then the retry
    // rewraps the current envelope instead of the stale one.
    store.race("race-row", 1);
    let mut run = drive(store.open_run("acceptance", V1, TARGET, PIN)).expect("open run");
    let report = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &live_params(TARGET),
    ))
    .expect("pass runs");
    assert_eq!(
        (report.done, report.failed, report.deferred, report.retried),
        (1, 0, 0, 1)
    );
    let (after, _, rewrapped_at) = read_row(&db, "race-row");
    assert_eq!(after.kek_version, TARGET);
    assert_eq!(rewrapped_at, Some(PIN as i64));
    assert_eq!(
        decrypt_vault(&third_kek, "race-row", &after).expect("decrypt retried row"),
        b"newer payload for race-row".as_slice()
    );
}

/// Poison rows count as failed and skip past; the pass completes and the
/// good rows behind them still rewrap.
///
/// Mutants killed: per-row `?` aborting the pass (goods behind poison stay
/// old); unknown version falling back to another key; tampered tag retried
/// as contention (deferred instead of failed).
#[test]
fn poison_rows_fail_per_row_and_skip_past() {
    let db = acceptance_db();
    let (keks, first_kek, third_kek) = TrialKeks::ends();

    seed_holder(&db, "poison-aa-malformed");
    let mut malformed =
        encrypt_vault(&first_kek, V1, "poison-aa-malformed", b"x").expect("encrypt malformed");
    malformed.dek_wrapped_b64url = "!!!not-base64!!!".to_string();
    place_row(&db, "poison-aa-malformed", &malformed, SEED_UPDATED_AT);

    let orphan_kek = fresh_key();
    place_fixture(
        &db,
        "poison-ab-unprovisioned",
        &orphan_kek,
        V2,
        b"orphan payload",
    );

    seed_holder(&db, "poison-ac-tampered");
    let mut tampered =
        encrypt_vault(&first_kek, V1, "poison-ac-tampered", b"y").expect("encrypt tampered");
    let mut raw = b64::decode(&tampered.dek_wrapped_b64url).expect("wrap decodes");
    raw[0] ^= 0x01;
    tampered.dek_wrapped_b64url = b64::encode(&raw);
    place_row(&db, "poison-ac-tampered", &tampered, SEED_UPDATED_AT);

    place_fixture(
        &db,
        "poison-zz-good-01",
        &first_kek,
        V1,
        b"good payload one",
    );
    place_fixture(
        &db,
        "poison-zz-good-02",
        &first_kek,
        V1,
        b"good payload two",
    );

    let store = AcceptanceStore::new(&db, first_kek, V1);
    let mut run = drive(store.open_run("acceptance", V1, TARGET, PIN)).expect("open run");
    let report = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &live_params(TARGET),
    ))
    .expect("pass runs");
    assert_eq!(
        (report.scanned, report.done, report.failed, report.deferred),
        (5, 2, 3, 0)
    );
    assert!(report.complete, "poison must not abort the pass");

    for (user_id, version) in [
        ("poison-aa-malformed", V1),
        ("poison-ab-unprovisioned", V2),
        ("poison-ac-tampered", V1),
    ] {
        let (blob, _, rewrapped_at) = read_row(&db, user_id);
        assert_eq!(blob.kek_version, version, "{user_id} keeps its version");
        assert_eq!(rewrapped_at, None, "{user_id} stays unmarked");
    }
    for (user_id, payload) in [
        ("poison-zz-good-01", b"good payload one".as_slice()),
        ("poison-zz-good-02", b"good payload two".as_slice()),
    ] {
        let (blob, _, rewrapped_at) = read_row(&db, user_id);
        assert_eq!(blob.kek_version, TARGET);
        assert_eq!(rewrapped_at, Some(PIN as i64));
        assert_eq!(
            decrypt_vault(&third_kek, user_id, &blob).expect("decrypt good row"),
            payload
        );
    }
}

/// A second full run over a converged vault is an empty complete pass.
///
/// Mutants killed: claim `<=` (target rows re-claimed); no-op pass
/// re-stamping `rewrapped_at`; cursor reset rescanning finished rows.
#[test]
fn converged_vault_yields_empty_complete_pass() {
    let db = acceptance_db();
    let (keks, first_kek, _, _) = TrialKeks::three();
    place_fixture(&db, "converged-01", &first_kek, V1, b"payload one");
    place_fixture(&db, "converged-02", &first_kek, V1, b"payload two");
    let store = AcceptanceStore::new(&db, first_kek, V1);

    let mut run = drive(store.open_run("acceptance", V1, TARGET, PIN)).expect("open run");
    let first = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut run,
        &live_params(TARGET),
    ))
    .expect("first pass runs");
    assert_eq!((first.done, first.complete), (2, true));
    drive(store.close_run(&run.id, PIN + 1)).expect("close run");

    let before_one = read_row(&db, "converged-01");
    let before_two = read_row(&db, "converged-02");
    let mut again =
        drive(store.open_run("acceptance", V1, TARGET, PIN + 2)).expect("open second run");
    let second = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut again,
        &live_params(TARGET),
    ))
    .expect("second pass runs");
    assert_eq!(
        (
            second.scanned,
            second.done,
            second.failed,
            second.deferred,
            second.complete
        ),
        (0, 0, 0, 0, true)
    );
    assert_rows_equal("converged-01", &read_row(&db, "converged-01"), &before_one);
    assert_rows_equal("converged-02", &read_row(&db, "converged-02"), &before_two);
}

/// Dry-run reports would-rewrap counts and mutates nothing: no vault
/// writes, no run record, no cursor or count movement.
///
/// Mutants killed: dry-run guard dropped on the CAS (rows rewrapped);
/// dry-run persisting progress (a run row appears); dry-run advancing
/// the transient cursor.
#[test]
fn dry_run_reports_counts_and_writes_nothing() {
    let db = acceptance_db();
    let (keks, first_kek, _, third_kek) = TrialKeks::three();
    place_fixture(&db, "dry-01", &first_kek, V1, b"dry payload one");
    place_fixture(&db, "dry-02", &first_kek, V1, b"dry payload two");
    place_fixture(&db, "dry-control", &third_kek, TARGET, b"control");
    let before_one = read_row(&db, "dry-01");
    let before_two = read_row(&db, "dry-02");
    let before_control = read_row(&db, "dry-control");

    let store = AcceptanceStore::new(&db, first_kek, V1);
    let mut rehearsal = RunRecord::transient("acceptance", V1, TARGET, PIN);
    let report = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut rehearsal,
        &RewrapParams {
            dry_run: true,
            ..live_params(TARGET)
        },
    ))
    .expect("dry run reports");
    assert_eq!((report.scanned, report.done, report.failed), (2, 2, 0));
    assert!(report.complete);

    assert_rows_equal("dry-01", &read_row(&db, "dry-01"), &before_one);
    assert_rows_equal("dry-02", &read_row(&db, "dry-02"), &before_two);
    assert_rows_equal(
        "dry-control",
        &read_row(&db, "dry-control"),
        &before_control,
    );
    let runs: i64 = db
        .query_row("SELECT COUNT(*) FROM rotation_runs", [], |row| row.get(0))
        .expect("count runs");
    assert_eq!(runs, 0, "dry run must not persist a run record");
    assert_eq!(rehearsal.cursor, None);
    assert_eq!(
        (
            rehearsal.rows_done,
            rehearsal.rows_failed,
            rehearsal.rows_deferred,
            rehearsal.claims_retried
        ),
        (0, 0, 0, 0)
    );
}

/// Out-of-range bounds, target mismatch, malformed runs, and live passes
/// over a transient record are rejected before any claim; nothing mutates.
///
/// Mutants killed: missing `batch_limit`/`max_batches` checks (`LIMIT 0`
/// completes vacuously, over-limit batches over-claim); mismatch ignored
/// (the wrong target rewraps); transient live pass allowed.
#[test]
fn invalid_bounds_are_rejected_before_any_write() {
    let db = acceptance_db();
    let (keks, first_kek, _, _) = TrialKeks::three();
    place_fixture(&db, "bounds-row", &first_kek, V1, b"bounds payload");
    let store = AcceptanceStore::new(&db, first_kek, V1);

    for (env, from, to) in [
        ("", V1, TARGET),
        ("acceptance", TARGET, TARGET),
        ("acceptance", TARGET + 1, TARGET),
        ("acceptance", V1, 0),
    ] {
        assert!(
            matches!(
                drive(store.open_run(env, from, to, PIN)),
                Err(JobError::InvalidParams(_))
            ),
            "open ({env:?}, {from}, {to}) must be rejected"
        );
    }
    assert!(
        drive(store.load_open_run("acceptance", TARGET))
            .expect("load")
            .is_none(),
        "rejected opens persist nothing"
    );

    let mut run = drive(store.open_run("acceptance", V1, TARGET, PIN)).expect("open run");
    for params in [
        RewrapParams {
            batch_limit: 0,
            ..live_params(TARGET)
        },
        RewrapParams {
            batch_limit: MAX_BATCH_LIMIT + 1,
            ..live_params(TARGET)
        },
        RewrapParams {
            max_batches: 0,
            ..live_params(TARGET)
        },
        RewrapParams {
            to_version: TARGET + 1,
            ..live_params(TARGET + 1)
        },
    ] {
        assert!(
            matches!(
                drive(run_rewrap_pass(&store, &keks, &mut run, &params)),
                Err(JobError::InvalidParams(_))
            ),
            "params {params:?} must be rejected"
        );
    }
    let mut transient = RunRecord::transient("acceptance", V1, TARGET, PIN);
    assert!(
        matches!(
            drive(run_rewrap_pass(
                &store,
                &keks,
                &mut transient,
                &live_params(TARGET)
            )),
            Err(JobError::InvalidParams(_))
        ),
        "live pass over a transient run must be rejected"
    );

    assert_eq!(tally(&db, "rewrapped_at IS NOT NULL"), 0);
    let persisted = drive(store.load_open_run("acceptance", TARGET))
        .expect("load")
        .expect("run persists");
    assert_eq!(
        (persisted.rows_done, persisted.rows_failed, persisted.cursor),
        (0, 0, None)
    );
}

/// An unprovisioned target key fails the pass before any claim, and a
/// second open run for the same environment + target is refused.
///
/// Mutants killed: lazy target-key lookup (per-row failures instead of
/// fail-closed); missing partial-unique open-run index (double open runs).
#[test]
fn unknown_target_key_and_double_open_fail_closed() {
    let db = acceptance_db();
    let (keks, first_kek, _, _) = TrialKeks::three();
    place_fixture(&db, "key-row", &first_kek, V1, b"key payload");
    let store = AcceptanceStore::new(&db, first_kek, V1);

    let mut future = drive(store.open_run("acceptance", V1, TARGET + 4, PIN)).expect("open run");
    let outcome = drive(run_rewrap_pass(
        &store,
        &keks,
        &mut future,
        &live_params(TARGET + 4),
    ));
    assert!(
        matches!(outcome, Err(JobError::UnknownKekVersion(v)) if v == TARGET + 4),
        "got {outcome:?}"
    );
    assert_eq!(tally(&db, "rewrapped_at IS NOT NULL"), 0);
    assert_eq!(future.cursor, None);
    assert_eq!((future.rows_done, future.rows_failed), (0, 0));

    let first = drive(store.open_run("acceptance", V1, TARGET, PIN)).expect("open first");
    assert!(
        matches!(
            drive(store.open_run("acceptance", V1, TARGET, PIN)),
            Err(JobError::PassAlreadyRunning)
        ),
        "second open for the same env + target is refused"
    );
    drive(store.open_run("acceptance-other", V1, TARGET, PIN)).expect("other env is independent");
    drive(store.open_run("acceptance", V2, TARGET + 1, PIN)).expect("other target is independent");
    drive(store.close_run(&first.id, PIN + 1)).expect("close");
    drive(store.close_run(&first.id, PIN + 1)).expect("double close is a no-op");
    let reopened =
        drive(store.open_run("acceptance", V1, TARGET, PIN + 2)).expect("reopen after close");
    assert_ne!(reopened.id, first.id);
}

/// Claim/envelope `Debug` redacts wrap ciphertext in both display modes.
///
/// Mutants killed: derived `Debug` (ciphertext leaks); manual `Debug`
/// covering `{}` while `{:#?}` leaks.
#[test]
fn claim_and_envelope_debug_redact_wrap() {
    let wrapped = "d3JhcHBlZC1hY2NlcHRhbmNlLXNlbnRpbmVs";
    let nonce = "bm9uY2UtYWNjZXB0YW5jZS1zZW50aW5lbA";
    let claim = RewrapClaim {
        user_id: "redact-claim".to_string(),
        kek_version: V1,
        dek_wrapped: wrapped.to_string(),
        dek_wrapped_nonce: nonce.to_string(),
    };
    for dumped in [format!("{claim:?}"), format!("{claim:#?}")] {
        assert!(dumped.contains("redact-claim"), "identity stays: {dumped}");
        assert!(
            !dumped.contains(wrapped) && !dumped.contains(nonce),
            "wrap leaks: {dumped}"
        );
        assert!(dumped.contains("redacted"), "redaction marker: {dumped}");
    }
    let envelope = RewrappedEnvelope {
        kek_version: TARGET,
        dek_wrapped: wrapped.to_string(),
        dek_wrapped_nonce: nonce.to_string(),
    };
    for dumped in [format!("{envelope:?}"), format!("{envelope:#?}")] {
        assert!(
            !dumped.contains(wrapped) && !dumped.contains(nonce),
            "envelope leaks: {dumped}"
        );
        assert!(dumped.contains("redacted"), "redaction marker: {dumped}");
    }
}

/// The job module emits no log calls and its errors carry no ciphertext.
///
/// Mutants killed: an added `println!`/tracing/log call (a ciphertext log
/// path); an error variant gaining a blob or cursor field (also breaks the
/// `Copy` bound below at compile time).
#[test]
fn job_module_has_no_log_path_and_scalar_errors() {
    fn assert_scalar<T: Copy + std::fmt::Debug + std::fmt::Display>() {}
    assert_scalar::<JobError>();
    let source = std::fs::read_to_string(workspace_root().join("workers/api/src/rotation.rs"))
        .expect("read job module");
    for forbidden in [
        "println!",
        "eprintln!",
        "print!",
        "eprint!",
        "dbg!",
        "tracing::",
        "log::",
        "tracing!",
        "debug!",
        "info!",
        "warn!",
        "error!",
        "console_log!",
    ] {
        assert!(
            !source.contains(forbidden),
            "job module must not contain {forbidden}"
        );
    }
    let sentinel = "Y2lwaGVyLWFjY2VwdGFuY2Utc2VudGluZWw";
    for error in [
        JobError::InvalidParams("acceptance reason"),
        JobError::UnknownKekVersion(11),
        JobError::PassAlreadyRunning,
        JobError::Store,
        JobError::RandomnessUnavailable,
    ] {
        let dumped = format!("{error:?} {error}");
        assert!(!dumped.contains(sentinel), "error leaks: {dumped}");
    }
}

/// The job's claim/CAS consts equal the normative runbook text byte for byte.
///
/// Mutants killed: a const edited without updating the runbook (or the
/// reverse); an unbound batch size; a CAS dropping a captured-envelope
/// guard.
#[test]
fn job_sql_matches_normative_runbook() {
    let runbook = runbook_text();
    let section = section_six(&runbook);
    let claim_at = section
        .find("SELECT user_id, kek_version, dek_wrapped, dek_wrapped_nonce")
        .expect("runbook documents the claim");
    let claim_end = section[claim_at..].find(';').expect("claim terminated");
    assert_eq!(
        CLAIM_BATCH_SQL,
        &section[claim_at..claim_at + claim_end + 1],
        "claim drifted from the runbook"
    );
    let cas_at = section
        .find("UPDATE provider_vaults")
        .expect("runbook documents the CAS");
    let cas_end = section[cas_at..].find(';').expect("CAS terminated");
    assert_eq!(
        CAS_REWRAP_SQL,
        &section[cas_at..cas_at + cas_end + 1],
        "CAS drifted from the runbook"
    );
    assert!(
        CLAIM_BATCH_SQL.contains("LIMIT $3"),
        "claim stays batch-bound"
    );
    assert!(
        CAS_REWRAP_SQL.contains("$captured_wrapped"),
        "CAS stays envelope-conditional"
    );
}
