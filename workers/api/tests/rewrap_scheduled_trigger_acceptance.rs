//! Acceptance tests for the scheduled vault re-wrap trigger: the durable
//! orchestration that turns one scheduled invocation into at most one
//! bounded `run_rewrap_pass` over a persisted `rotation_runs` record
//! (`docs/security/kek-rotation.md` sections 6-8).
//!
//! These tests are written against the contract below, before the trigger
//! exists. The Builder implements this exact seam in
//! `workers/api/src/rotation.rs` and does not change these files. The
//! Cloudflare scheduled-event glue is exercised separately (source pins);
//! everything here is native behavior over a synthetic `RewrapStore` with
//! real vault cryptography, generated keys, and synthetic user ids.
//!
//! # Builder contract (public items in `nntune_api::rotation`)
//!
//! ```text
//! pub struct ScheduledRewrapConfig {
//!     pub env: String,
//!     pub from_version: u32,
//!     pub to_version: u32,
//!     pub batch_limit: u32,
//!     pub max_batches: u32,
//!     pub dry_run: bool,
//!     pub now_secs: u64,
//! }
//!
//! #[derive(Debug)]  // count-only fields, exactly these three
//! pub struct ScheduledRewrapReport {
//!     pub pass: PassReport,
//!     pub resumed: bool,
//!     pub closed: bool,
//! }
//!
//! pub async fn run_scheduled_rewrap(
//!     store: &impl RewrapStore,
//!     keks: &impl KekSource,
//!     config: &ScheduledRewrapConfig,
//! ) -> Result<ScheduledRewrapReport, JobError>;
//!
//! pub enum ScheduledRewrapArming {
//!     Disabled,
//!     Armed(ScheduledRewrapConfig),
//! }
//!
//! pub fn scheduled_rewrap_arming_from_lookup(
//!     get: impl Fn(&str) -> Option<String>,
//!     now_secs: u64,
//! ) -> Result<ScheduledRewrapArming, JobError>;
//!
//! pub const CAS_REWRAP_POSTGRES_SQL: &str = ...; // see below
//! ```
//!
//! ## `run_scheduled_rewrap` semantics
//!
//! 1. Fail closed BEFORE any mutation: the ceremony validates
//!    (`validate_new_run`, `batch_limit` within `1..=MAX_BATCH_LIMIT`,
//!    `max_batches >= 1`) and the target KEK resolves
//!    (`JobError::UnknownKekVersion(to_version)`) before `open_run`,
//!    `cas_rewrap`, `save_progress`, or `close_run` can be called. A
//!    misconfigured invocation writes nothing, not even a ledger row.
//! 2. Dry-run: run the pass over `RunRecord::transient`, return
//!    `{ resumed: false, closed: false }`, and never call `open_run`,
//!    `cas_rewrap`, `save_progress`, or `close_run` — even when an open
//!    run already exists (it stays byte-identical).
//! 3. Live: `load_open_run(env, to_version)` first. A found run is
//!    RESUMED (same id, `resumed: true`); its `(env, from_version,
//!    to_version)` must equal the configured ceremony or the invocation
//!    fails with `InvalidParams` and writes nothing. When none is found,
//!    `open_run` creates it; if that returns `PassAlreadyRunning`
//!    (a concurrent invocation won the load-to-open race), load again
//!    and resume THAT run instead of failing or opening a second one.
//!    The partial-unique open-run index alone does not serialize two
//!    overlapping invocations — this reload is what makes them converge.
//! 4. Run one bounded pass. On `run_rewrap_pass` error (including a
//!    failed `save_progress`), propagate the error and do NOT close:
//!    the run stays open so the next invocation recovers from the
//!    durable ledger, with no in-memory progress anywhere.
//! 5. Close only when the pass reports `complete` (a bound-stopped pass
//!    leaves the run open). Closing on completion is also correct for
//!    poison/deferred rows: they belong to a LATER run, whose fresh
//!    cursor reaches them again. A failed `close_run` propagates as
//!    `JobError::Store` and leaves the run open for a retry close.
//! 6. Duplicate delivery after close is a vault no-op: a new empty run
//!    may open and close, but no vault row is touched.
//!
//! ## `scheduled_rewrap_arming_from_lookup` semantics (explicit ceremony)
//!
//! The scheduled event by itself never rotates anything. Only an
//! operator-armed, complete configuration produces a pass; the arming
//! names its ONE environment and its `from -> to` versions explicitly.
//! It never derives a target (no `current + 1`), never flips the write
//! version, and never retires material.
//!
//! - `VAULT_REWRAP_ENABLED`: absent, `""`, `"0"`, `"false"`, `"FALSE"`
//!   mean `Disabled`; `"1"`, `"true"`, `"TRUE"` arm; any other value is
//!   `Err(InvalidParams)`, never a silent disable.
//! - While armed, `VAULT_REWRAP_ENV`, `VAULT_REWRAP_FROM_VERSION`, and
//!   `VAULT_REWRAP_TO_VERSION` are required. The env names exactly one
//!   environment: non-empty, with no whitespace, `,`, or `;`. Versions
//!   parse as `u32` and satisfy `validate_new_run`.
//! - `VAULT_REWRAP_BATCH_LIMIT` and `VAULT_REWRAP_MAX_BATCHES` are
//!   optional bounds (implementation-chosen defaults within the
//!   validated ranges); when present they must parse and validate.
//! - `VAULT_REWRAP_DRY_RUN` uses the same truthy/falsy sets as the
//!   arming flag; absent means live.`now_secs` passes through unchanged.
//!
//! ## `CAS_REWRAP_POSTGRES_SQL`
//!
//! `CAS_REWRAP_SQL` is normative with NAMED placeholders, which Postgres
//! (and the Postgres driver) cannot execute. The production store must
//! therefore execute a positional rendering of the same statement: this
//! constant is `CAS_REWRAP_SQL` byte-for-byte with `$new -> $1`,
//! `$w -> $2`, `$n -> $3`, `$now -> $4` (both occurrences), `$u -> $5`,
//! `$old -> $6`, `$captured_wrapped -> $7`,
//! `$captured_wrapped_nonce -> $8`. The normative const stays unchanged.
//!
//! Each test names the production change that would make it fail.

use nntune_api::handlers::StoreError;
use nntune_api::rotation::{
    run_scheduled_rewrap, scheduled_rewrap_arming_from_lookup, validate_new_run, CasRewrapOutcome,
    JobError, KekSource, RewrapClaim, RewrapStore, RewrappedEnvelope, RunRecord,
    ScheduledRewrapArming, ScheduledRewrapConfig, CAS_REWRAP_POSTGRES_SQL, CAS_REWRAP_SQL,
    MAX_BATCH_LIMIT, MAX_ROW_ATTEMPTS,
};
use nntune_api::vault::{decrypt_vault, encrypt_vault, VaultBlob};
use rand::RngCore;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::task::{Context as TaskCx, Poll};

const V1: u32 = 1;
const TARGET: u32 = 3;
const PIN: u64 = 1_800_000_000;
const SEED_UPDATED_AT: i64 = 20;
const ENV: &str = "acceptance-env";

fn drive<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = TaskCx::from_waker(waker);
    let mut pinned = Box::pin(future);
    for _ in 0..10_000 {
        if let Poll::Ready(out) = pinned.as_mut().poll(&mut cx) {
            return out;
        }
    }
    panic!("scheduled-trigger fake awaited outside the synthetic store");
}

fn fresh_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

struct FakeKeks {
    keys: HashMap<u32, [u8; 32]>,
}

impl FakeKeks {
    /// Provisioned versions 1 and TARGET, plus the raw keys for fixtures.
    fn with_target() -> (Self, [u8; 32], [u8; 32]) {
        let old = fresh_key();
        let new = fresh_key();
        (
            Self {
                keys: HashMap::from([(V1, old), (TARGET, new)]),
            },
            old,
            new,
        )
    }

    /// Only the old version: the target key is unprovisioned.
    fn without_target() -> (Self, [u8; 32]) {
        let old = fresh_key();
        (
            Self {
                keys: HashMap::from([(V1, old)]),
            },
            old,
        )
    }
}

impl KekSource for FakeKeks {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        self.keys.get(&version)
    }
}

#[derive(Clone)]
struct RowState {
    blob: VaultBlob,
    updated_at: i64,
    rewrapped_at: Option<i64>,
}

/// In-memory `RewrapStore` with a durable ledger model: runs are stored
/// records, so resumption is proven through `load_open_run`, never
/// through state the orchestrator itself held between invocations.
/// Counters record every mutating entry point so fail-closed tests can
/// prove "before any mutation" rather than inferring it.
struct FakeStore {
    rows: RefCell<BTreeMap<String, RowState>>,
    runs: RefCell<Vec<RunRecord>>,
    next_id: Cell<u64>,
    races: RefCell<BTreeMap<String, VecDeque<VaultBlob>>>,
    fail_saves: Cell<u32>,
    fail_closes: Cell<u32>,
    hide_open_loads: Cell<u32>,
    open_calls: Cell<u64>,
    save_calls: Cell<u64>,
    close_calls: Cell<u64>,
    cas_calls: Cell<u64>,
    load_envs: RefCell<Vec<String>>,
}

impl FakeStore {
    fn new() -> Self {
        Self {
            rows: RefCell::new(BTreeMap::new()),
            runs: RefCell::new(Vec::new()),
            next_id: Cell::new(0),
            races: RefCell::new(BTreeMap::new()),
            fail_saves: Cell::new(0),
            fail_closes: Cell::new(0),
            hide_open_loads: Cell::new(0),
            open_calls: Cell::new(0),
            save_calls: Cell::new(0),
            close_calls: Cell::new(0),
            cas_calls: Cell::new(0),
            load_envs: RefCell::new(Vec::new()),
        }
    }

    fn seed(&self, user_id: &str, kek: &[u8; 32], version: u32, payload: &[u8]) -> VaultBlob {
        let blob = encrypt_vault(kek, version, user_id, payload).expect("encrypt fixture");
        self.rows.borrow_mut().insert(
            user_id.to_string(),
            RowState {
                blob: blob.clone(),
                updated_at: SEED_UPDATED_AT,
                rewrapped_at: None,
            },
        );
        blob
    }

    fn row(&self, user_id: &str) -> (VaultBlob, i64, Option<i64>) {
        let rows = self.rows.borrow();
        let row = rows.get(user_id).expect("seeded row");
        (row.blob.clone(), row.updated_at, row.rewrapped_at)
    }

    fn queue_race(&self, user_id: &str, replacement: VaultBlob) {
        self.races
            .borrow_mut()
            .entry(user_id.to_string())
            .or_default()
            .push_back(replacement);
    }

    fn runs_created(&self) -> usize {
        self.runs.borrow().len()
    }

    fn run_by_id(&self, id: &str) -> RunRecord {
        self.runs
            .borrow()
            .iter()
            .find(|run| run.id == id)
            .expect("run exists")
            .clone()
    }

    fn only_run(&self) -> RunRecord {
        let runs = self.runs.borrow();
        assert_eq!(runs.len(), 1, "exactly one durable run");
        runs[0].clone()
    }

    fn mutation_calls(&self) -> (u64, u64, u64, u64) {
        (
            self.open_calls.get(),
            self.cas_calls.get(),
            self.save_calls.get(),
            self.close_calls.get(),
        )
    }
}

impl RewrapStore for FakeStore {
    async fn claim_batch(
        &self,
        to_version: u32,
        after_user_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<RewrapClaim>, StoreError> {
        Ok(self
            .rows
            .borrow()
            .iter()
            .filter(|(user_id, row)| {
                row.blob.kek_version < to_version
                    && after_user_id.is_none_or(|after| user_id.as_str() > after)
            })
            .take(limit as usize)
            .map(|(user_id, row)| RewrapClaim {
                user_id: user_id.clone(),
                kek_version: row.blob.kek_version,
                dek_wrapped: row.blob.dek_wrapped_b64url.clone(),
                dek_wrapped_nonce: row.blob.dek_wrapped_nonce_b64url.clone(),
            })
            .collect())
    }

    async fn reload_row(&self, user_id: &str) -> Result<Option<RewrapClaim>, StoreError> {
        Ok(self.rows.borrow().get(user_id).map(|row| RewrapClaim {
            user_id: user_id.to_string(),
            kek_version: row.blob.kek_version,
            dek_wrapped: row.blob.dek_wrapped_b64url.clone(),
            dek_wrapped_nonce: row.blob.dek_wrapped_nonce_b64url.clone(),
        }))
    }

    async fn cas_rewrap(
        &self,
        user_id: &str,
        captured: &RewrapClaim,
        rewrapped: &RewrappedEnvelope,
        now_secs: u64,
    ) -> Result<CasRewrapOutcome, StoreError> {
        self.cas_calls.set(self.cas_calls.get() + 1);
        // A queued racing PUT lands between claim and CAS, replacing the
        // whole envelope at the same KEK version.
        let race = self
            .races
            .borrow_mut()
            .get_mut(user_id)
            .and_then(|queue| queue.pop_front());
        if let Some(replacement) = race {
            if let Some(row) = self.rows.borrow_mut().get_mut(user_id) {
                row.blob = replacement;
            }
        }
        let mut rows = self.rows.borrow_mut();
        let Some(row) = rows.get_mut(user_id) else {
            return Err(StoreError);
        };
        if row.blob.kek_version == captured.kek_version
            && row.blob.dek_wrapped_b64url == captured.dek_wrapped
            && row.blob.dek_wrapped_nonce_b64url == captured.dek_wrapped_nonce
        {
            row.blob.kek_version = rewrapped.kek_version;
            row.blob.dek_wrapped_b64url = rewrapped.dek_wrapped.clone();
            row.blob.dek_wrapped_nonce_b64url = rewrapped.dek_wrapped_nonce.clone();
            row.updated_at = now_secs as i64;
            row.rewrapped_at = Some(now_secs as i64);
            Ok(CasRewrapOutcome::Rewrapped)
        } else {
            Ok(CasRewrapOutcome::ClaimLost)
        }
    }

    async fn open_run(
        &self,
        env: &str,
        from_version: u32,
        to_version: u32,
        now_secs: u64,
    ) -> Result<RunRecord, JobError> {
        self.open_calls.set(self.open_calls.get() + 1);
        validate_new_run(env, from_version, to_version)?;
        let mut runs = self.runs.borrow_mut();
        if runs
            .iter()
            .any(|run| run.env == env && run.to_version == to_version && run.finished_at.is_none())
        {
            return Err(JobError::PassAlreadyRunning);
        }
        let next = self.next_id.get() + 1;
        self.next_id.set(next);
        let run = RunRecord {
            id: format!("fake-run-{next}"),
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
        };
        runs.push(run.clone());
        Ok(run)
    }

    async fn load_open_run(
        &self,
        env: &str,
        to_version: u32,
    ) -> Result<Option<RunRecord>, StoreError> {
        self.load_envs.borrow_mut().push(env.to_string());
        if self.hide_open_loads.get() > 0 {
            self.hide_open_loads.set(self.hide_open_loads.get() - 1);
            return Ok(None);
        }
        Ok(self
            .runs
            .borrow()
            .iter()
            .find(|run| run.env == env && run.to_version == to_version && run.finished_at.is_none())
            .cloned())
    }

    async fn save_progress(&self, run: &RunRecord) -> Result<(), StoreError> {
        self.save_calls.set(self.save_calls.get() + 1);
        if self.fail_saves.get() > 0 {
            self.fail_saves.set(self.fail_saves.get() - 1);
            return Err(StoreError);
        }
        let mut runs = self.runs.borrow_mut();
        match runs
            .iter_mut()
            .find(|stored| stored.id == run.id && stored.finished_at.is_none())
        {
            Some(stored) => {
                *stored = run.clone();
                Ok(())
            }
            None => Err(StoreError),
        }
    }

    async fn close_run(&self, run_id: &str, now_secs: u64) -> Result<(), StoreError> {
        self.close_calls.set(self.close_calls.get() + 1);
        if self.fail_closes.get() > 0 {
            self.fail_closes.set(self.fail_closes.get() - 1);
            return Err(StoreError);
        }
        if let Some(run) = self
            .runs
            .borrow_mut()
            .iter_mut()
            .find(|run| run.id == run_id && run.finished_at.is_none())
        {
            run.finished_at = Some(now_secs);
        }
        Ok(())
    }
}

fn config() -> ScheduledRewrapConfig {
    ScheduledRewrapConfig {
        env: ENV.to_string(),
        from_version: V1,
        to_version: TARGET,
        batch_limit: MAX_BATCH_LIMIT,
        max_batches: 10,
        dry_run: false,
        now_secs: PIN,
    }
}

fn invoke(
    store: &FakeStore,
    keks: &FakeKeks,
    config: &ScheduledRewrapConfig,
) -> Result<nntune_api::rotation::ScheduledRewrapReport, JobError> {
    drive(run_scheduled_rewrap(store, keks, config))
}

/// One invocation opens a run; a bound-stopped pass leaves it open with
/// durable cursor/counts; the next invocation resumes the SAME run and
/// closes it on completion.
///
/// Mutants killed: closing at the batch bound; opening a fresh run per
/// invocation; resuming from in-memory state instead of the ledger
/// (the second invocation shares nothing with the first but the store).
#[test]
fn scheduled_pass_closes_only_when_complete_and_resumes_same_run() {
    let store = FakeStore::new();
    let (keks, old_kek, new_kek) = FakeKeks::with_target();
    for (user, payload) in [
        ("sched-01", b"first payload".as_slice()),
        ("sched-02", b"second payload".as_slice()),
        ("sched-03", b"third payload".as_slice()),
    ] {
        store.seed(user, &old_kek, V1, payload);
    }
    let bounded = ScheduledRewrapConfig {
        batch_limit: 2,
        max_batches: 1,
        ..config()
    };

    let first = invoke(&store, &keks, &bounded).expect("first invocation runs");
    assert_eq!(
        (first.pass.scanned, first.pass.done, first.pass.complete),
        (2, 2, false)
    );
    assert!(!first.resumed && !first.closed);
    let run = store.only_run();
    assert_eq!(run.finished_at, None, "bound-stopped run stays open");
    assert_eq!(run.cursor.as_deref(), Some("sched-02"));
    assert_eq!(run.rows_done, 2);

    let later = ScheduledRewrapConfig {
        now_secs: PIN + 10,
        ..bounded
    };
    let second = invoke(&store, &keks, &later).expect("second invocation resumes");
    assert_eq!(
        (second.pass.scanned, second.pass.done, second.pass.complete),
        (1, 1, true)
    );
    assert!(second.resumed, "the durable run is resumed, not replaced");
    assert!(second.closed);
    assert_eq!(store.runs_created(), 1, "both passes share one run");
    let run = store.only_run();
    assert_eq!(run.finished_at, Some(PIN + 10));
    assert_eq!(run.rows_done, 3, "counts accumulate durably");

    for (user, payload, stamp) in [
        ("sched-01", b"first payload".as_slice(), PIN),
        ("sched-02", b"second payload".as_slice(), PIN),
        ("sched-03", b"third payload".as_slice(), PIN + 10),
    ] {
        let (blob, _, rewrapped_at) = store.row(user);
        assert_eq!(blob.kek_version, TARGET, "{user} reaches the target");
        assert_eq!(rewrapped_at, Some(stamp as i64), "{user} pass stamp");
        assert_eq!(
            decrypt_vault(&new_kek, user, &blob).expect("decrypt after scheduled pass"),
            payload
        );
    }
}

/// Overlap race: this invocation's first load sees no open run, but by
/// the time it opens, a concurrent invocation's run exists. The unique
/// index rejects the second open; the orchestrator must reload and
/// resume that same run rather than fail or fork progress.
///
/// Mutants killed: propagating `PassAlreadyRunning` to the caller;
/// treating the open conflict as a store failure; opening anyway under
/// a different identity.
#[test]
fn open_race_reloads_and_resumes_the_same_run() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("race-01", &old_kek, V1, b"one");
    store.seed("race-02", &old_kek, V1, b"two");
    let bounded = ScheduledRewrapConfig {
        batch_limit: 1,
        max_batches: 1,
        ..config()
    };
    let first = invoke(&store, &keks, &bounded).expect("first invocation stops at the bound");
    assert!(!first.pass.complete && !first.closed);
    let created = store.only_run();

    // The overlapping invocation races past its load before it can see
    // the open run, then loses the open. It is granted a second batch
    // so the pass can observe the empty claim that proves completion.
    store.hide_open_loads.set(1);
    let finisher = ScheduledRewrapConfig {
        max_batches: 2,
        ..bounded
    };
    let second = invoke(&store, &keks, &finisher).expect("race reloads instead of failing");
    assert!(second.resumed, "the conflict reload resumes the same run");
    assert!(second.pass.complete && second.closed);
    assert_eq!(store.runs_created(), 1, "no second run was created");
    assert_eq!(store.only_run().id, created.id);
    assert_eq!(store.only_run().rows_done, 2);
}

/// Duplicate scheduled delivery after a run has closed changes no vault
/// row. A fresh empty run may open/close; the vault is untouched.
///
/// Mutants killed: re-claiming at-target rows (`<=` claim) on the
/// duplicate pass; re-stamping `rewrapped_at` on a no-op pass.
#[test]
fn duplicate_delivery_after_close_does_not_touch_vault_rows() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("dup-01", &old_kek, V1, b"payload");
    let first = invoke(&store, &keks, &config()).expect("first delivery completes");
    assert!(first.closed);
    let before = store.row("dup-01");

    let duplicate = invoke(
        &store,
        &keks,
        &ScheduledRewrapConfig {
            now_secs: PIN + 99,
            ..config()
        },
    )
    .expect("duplicate delivery is harmless");
    assert_eq!(
        (
            duplicate.pass.scanned,
            duplicate.pass.done,
            duplicate.pass.complete,
            duplicate.closed
        ),
        (0, 0, true, true)
    );
    let after = store.row("dup-01");
    assert_eq!(after.0.dek_wrapped_b64url, before.0.dek_wrapped_b64url);
    assert_eq!(after.0.payload_b64url, before.0.payload_b64url);
    assert_eq!(after.0.payload_nonce_b64url, before.0.payload_nonce_b64url);
    assert_eq!(after.1, before.1, "updated_at untouched");
    assert_eq!(after.2, before.2, "rewrapped_at not re-stamped");
}

/// A `save_progress` failure after a row was CAS-rewrapped aborts the
/// pass without closing. The next invocation resumes the same durable
/// run, never rewraps the already-done row twice, and finishes the
/// rest. (Ledger counts may under-report the interrupted batch; the
/// vault state is authoritative and the run still terminates closed.)
///
/// Mutants killed: closing on error; reporting success on a failed
/// save; opening a replacement run on recovery.
#[test]
fn save_progress_failure_keeps_run_open_for_recovery() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("save-01", &old_kek, V1, b"one");
    store.seed("save-02", &old_kek, V1, b"two");
    let one_at_a_time = ScheduledRewrapConfig {
        batch_limit: 1,
        max_batches: 5,
        ..config()
    };
    store.fail_saves.set(1);

    let failed = invoke(&store, &keks, &one_at_a_time);
    assert!(
        matches!(failed, Err(JobError::Store)),
        "save failure propagates opaquely, got {failed:?}"
    );
    assert_eq!(store.close_calls.get(), 0, "a failed pass must not close");
    let run = store.only_run();
    assert_eq!(run.finished_at, None);
    let (done_before_recovery, _, _) = store.row("save-01");
    assert_eq!(done_before_recovery.kek_version, TARGET, "the CAS landed");
    assert_eq!(
        store.row("save-02").0.kek_version,
        V1,
        "the pass stopped at the failed save"
    );

    let recovered = invoke(&store, &keks, &one_at_a_time).expect("next invocation recovers");
    assert!(recovered.resumed && recovered.closed);
    assert_eq!(store.runs_created(), 1, "recovery reuses the durable run");
    assert_eq!(
        store.row("save-01").0.dek_wrapped_b64url,
        done_before_recovery.dek_wrapped_b64url,
        "the already-rewrapped row is never rewrapped twice"
    );
    assert_eq!(store.row("save-02").0.kek_version, TARGET);
    assert_eq!(store.only_run().finished_at, Some(PIN));
}

/// A failed `close_run` propagates and leaves the finished pass's run
/// open; the retry resumes that run (scanning nothing) and closes it.
///
/// Mutants killed: swallowing the close error and reporting `closed`;
/// opening a new run because the close was forgotten.
#[test]
fn close_failure_keeps_run_open_and_retry_closes_it() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("close-01", &old_kek, V1, b"payload");
    store.fail_closes.set(1);

    let failed = invoke(&store, &keks, &config());
    assert!(
        matches!(failed, Err(JobError::Store)),
        "close failure propagates, got {failed:?}"
    );
    assert_eq!(store.row("close-01").0.kek_version, TARGET);
    assert_eq!(
        store.only_run().finished_at,
        None,
        "run stays open after a failed close"
    );

    let retried = invoke(&store, &keks, &config()).expect("retry closes the same run");
    assert!(retried.resumed && retried.closed);
    assert_eq!(retried.pass.scanned, 0);
    assert_eq!(store.runs_created(), 1);
    assert_eq!(store.only_run().finished_at, Some(PIN));
}

/// Dry-run reports would-rewrap counts and writes NOTHING: no vault
/// CAS, no ledger open/save/close.
///
/// Mutants killed: the orchestrator opening a run before honoring
/// `dry_run`; closing the transient record after the pass.
#[test]
fn dry_run_writes_nothing_including_the_ledger() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("dry-01", &old_kek, V1, b"one");
    store.seed("dry-02", &old_kek, V1, b"two");
    let before_one = store.row("dry-01");
    let before_two = store.row("dry-02");

    let report = invoke(
        &store,
        &keks,
        &ScheduledRewrapConfig {
            dry_run: true,
            ..config()
        },
    )
    .expect("dry run reports");
    assert_eq!((report.pass.scanned, report.pass.done), (2, 2));
    assert!(report.pass.complete && !report.resumed && !report.closed);
    assert_eq!(
        store.mutation_calls(),
        (0, 0, 0, 0),
        "no open/cas/save/close calls in a dry run"
    );
    assert_eq!(store.runs_created(), 0, "no ledger row exists");
    assert_eq!(
        store.row("dry-01").0.dek_wrapped_b64url,
        before_one.0.dek_wrapped_b64url
    );
    assert_eq!(
        store.row("dry-02").0.dek_wrapped_b64url,
        before_two.0.dek_wrapped_b64url
    );
    assert_eq!(store.row("dry-01").2, None);
}

/// Dry-run rehearsal while a live run is open leaves that run's durable
/// record byte-identical (cursor, counts, timestamps).
///
/// Mutants killed: dry-run resuming/closing the live run to "preview"
/// its remaining rows.
#[test]
fn dry_run_leaves_an_existing_open_run_untouched() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("dry-live-01", &old_kek, V1, b"one");
    store.seed("dry-live-02", &old_kek, V1, b"two");
    let bounded = ScheduledRewrapConfig {
        batch_limit: 1,
        max_batches: 1,
        ..config()
    };
    invoke(&store, &keks, &bounded).expect("live pass opens a run");
    let snapshot = store.only_run();
    let calls_before = store.mutation_calls();

    let report = invoke(
        &store,
        &keks,
        &ScheduledRewrapConfig {
            dry_run: true,
            ..config()
        },
    )
    .expect("dry run reports");
    assert!(!report.resumed && !report.closed);
    assert_eq!(
        store.mutation_calls(),
        calls_before,
        "dry run adds no mutating calls"
    );
    assert_eq!(
        store.run_by_id(&snapshot.id),
        snapshot,
        "the open run is byte-identical after the rehearsal"
    );
}

/// An unprovisioned target key fails before `open_run` in both modes:
/// no ledger row, no CAS, no anything.
///
/// Mutants killed: naive orchestration that opens first and lets
/// `run_rewrap_pass` fail afterwards (a leaked open run per attempt).
#[test]
fn missing_target_key_fails_before_opening_a_run() {
    for dry_run in [false, true] {
        let store = FakeStore::new();
        let (keks, old_kek) = FakeKeks::without_target();
        store.seed("key-01", &old_kek, V1, b"payload");
        let outcome = invoke(
            &store,
            &keks,
            &ScheduledRewrapConfig {
                dry_run,
                ..config()
            },
        );
        assert!(
            matches!(outcome, Err(JobError::UnknownKekVersion(v)) if v == TARGET),
            "dry_run={dry_run}: got {outcome:?}"
        );
        assert_eq!(
            store.mutation_calls(),
            (0, 0, 0, 0),
            "dry_run={dry_run}: nothing mutated before the key check"
        );
        assert_eq!(store.runs_created(), 0);
        assert_eq!(store.row("key-01").0.kek_version, V1);
    }
}

/// Malformed ceremony configs are rejected before any mutating store
/// call, each with the scalar `InvalidParams` error.
///
/// Mutants killed: forwarding unchecked bounds to the pass after
/// opening a run; accepting an empty/multi-env or non-rotation shape.
#[test]
fn invalid_config_fails_before_any_mutation() {
    let cases = [
        ScheduledRewrapConfig {
            env: String::new(),
            ..config()
        },
        ScheduledRewrapConfig {
            from_version: TARGET,
            ..config()
        },
        ScheduledRewrapConfig {
            from_version: TARGET + 1,
            ..config()
        },
        ScheduledRewrapConfig {
            to_version: 0,
            ..config()
        },
        ScheduledRewrapConfig {
            batch_limit: 0,
            ..config()
        },
        ScheduledRewrapConfig {
            batch_limit: MAX_BATCH_LIMIT + 1,
            ..config()
        },
        ScheduledRewrapConfig {
            max_batches: 0,
            ..config()
        },
    ];
    for case in cases {
        let store = FakeStore::new();
        let (keks, old_kek, _) = FakeKeks::with_target();
        store.seed("cfg-01", &old_kek, V1, b"payload");
        let outcome = invoke(&store, &keks, &case);
        assert!(
            matches!(outcome, Err(JobError::InvalidParams(_))),
            "config (env={:?} from={} to={} batch={} batches={}) got {outcome:?}",
            case.env,
            case.from_version,
            case.to_version,
            case.batch_limit,
            case.max_batches
        );
        assert_eq!(store.mutation_calls(), (0, 0, 0, 0), "nothing mutated");
        assert_eq!(store.row("cfg-01").2, None, "row never marked rewrapped");
    }
}

/// A durable run whose ceremony differs from the configured one is not
/// silently continued under new management: mismatch fails closed with
/// the ledger untouched.
///
/// Mutants killed: resuming by `(env, to_version)` alone while the
/// operator-supplied `from_version` changed mid-ceremony.
#[test]
fn resumed_run_must_match_the_configured_ceremony() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("mismatch-01", &old_kek, V1, b"payload");
    let existing = drive(store.open_run(ENV, V1, TARGET, PIN)).expect("pre-opened run");
    let snapshot = store.run_by_id(&existing.id);

    let outcome = invoke(
        &store,
        &keks,
        &ScheduledRewrapConfig {
            from_version: TARGET - 1,
            ..config()
        },
    );
    assert!(
        matches!(outcome, Err(JobError::InvalidParams(_))),
        "got {outcome:?}"
    );
    assert_eq!(store.cas_calls.get(), 0);
    assert_eq!(store.save_calls.get(), 0);
    assert_eq!(store.close_calls.get(), 0);
    assert_eq!(store.run_by_id(&existing.id), snapshot, "ledger untouched");
}

/// Captured-envelope CAS through the scheduled path: a same-version
/// PUT loses the first claim and is retried against the replacement
/// envelope; a row raced on every attempt defers with its newest
/// payload intact; the good row behind both still progresses and the
/// completed pass closes (deferred rows belong to a later run).
///
/// Mutants killed: version-only CAS in the orchestration path counting
/// a lost claim as done; a hot row wedging or failing the whole pass.
#[test]
fn stale_put_and_hot_row_via_scheduled_path() {
    let store = FakeStore::new();
    let (keks, old_kek, new_kek) = FakeKeks::with_target();
    store.seed("cas-a-stale", &old_kek, V1, b"captured payload");
    store.seed("cas-b-hot", &old_kek, V1, b"hot captured");
    store.seed("cas-c-good", &old_kek, V1, b"good payload");

    // One replacement for the stale row: lose once, retry, succeed.
    let stale_replacement =
        encrypt_vault(&old_kek, V1, "cas-a-stale", b"replacement payload").expect("replacement");
    store.queue_race("cas-a-stale", stale_replacement);
    // A fresh replacement under every attempt for the hot row.
    let mut last_hot: Option<VaultBlob> = None;
    for index in 0..MAX_ROW_ATTEMPTS {
        let replacement = encrypt_vault(
            &old_kek,
            V1,
            "cas-b-hot",
            format!("hot replacement {index}").as_bytes(),
        )
        .expect("hot replacement");
        last_hot = Some(replacement.clone());
        store.queue_race("cas-b-hot", replacement);
    }
    let last_hot = last_hot.expect("hot replacements queued");

    let report = invoke(&store, &keks, &config()).expect("scheduled pass runs");
    assert_eq!(
        (
            report.pass.scanned,
            report.pass.done,
            report.pass.failed,
            report.pass.deferred
        ),
        (3, 2, 0, 1),
        "lost claims are never counted as done"
    );
    assert_eq!(
        report.pass.retried,
        1 + u64::from(MAX_ROW_ATTEMPTS),
        "one reload for the stale row, one per lost hot-row attempt"
    );
    assert!(report.pass.complete && report.closed);

    let (stale_after, _, _) = store.row("cas-a-stale");
    assert_eq!(stale_after.kek_version, TARGET);
    assert_eq!(
        decrypt_vault(&new_kek, "cas-a-stale", &stale_after).expect("decrypt retried row"),
        b"replacement payload".as_slice(),
        "the replacement envelope was rewrapped, never overwritten"
    );
    let (hot_after, _, hot_mark) = store.row("cas-b-hot");
    assert_eq!(hot_after.kek_version, V1, "deferred row keeps its version");
    assert_eq!(hot_after.payload_b64url, last_hot.payload_b64url);
    assert_eq!(
        hot_after.payload_nonce_b64url,
        last_hot.payload_nonce_b64url
    );
    assert_eq!(hot_after.dek_wrapped_b64url, last_hot.dek_wrapped_b64url);
    assert_eq!(hot_mark, None, "deferred row stays unmarked");
    assert_eq!(
        decrypt_vault(&old_kek, "cas-b-hot", &hot_after).expect("hot row still readable"),
        format!("hot replacement {}", MAX_ROW_ATTEMPTS - 1).as_bytes()
    );
    assert_eq!(store.row("cas-c-good").0.kek_version, TARGET);
}

/// An open run for ANOTHER environment is invisible to this ceremony:
/// every ledger load names the configured env, and the foreign run's
/// record is untouched. Scheduling never crosses environments.
///
/// Mutants killed: loading "the" open run without an env predicate;
/// closing whatever run a pass happened to load.
#[test]
fn other_env_open_run_is_never_touched() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    store.seed("xenv-01", &old_kek, V1, b"payload");
    let foreign = drive(store.open_run("other-env", V1, TARGET, PIN)).expect("foreign run");
    let foreign_snapshot = store.run_by_id(&foreign.id);

    let report = invoke(&store, &keks, &config()).expect("own-env pass runs");
    assert!(report.closed);
    assert!(
        store
            .load_envs
            .borrow()
            .iter()
            .all(|env| env.as_str() == ENV),
        "every load names only the configured env: {:?}",
        store.load_envs.borrow()
    );
    assert_eq!(
        store.run_by_id(&foreign.id),
        foreign_snapshot,
        "the foreign run is byte-identical"
    );
    assert_eq!(store.runs_created(), 2, "each env has its own run");
}

/// The value a scheduled invocation returns (and the errors it raises)
/// can be debug-formatted into telemetry without exposing a user id, a
/// resume cursor, or any wrap/payload ciphertext.
///
/// Mutants killed: a report carrying the run id or cursor; an error
/// variant that echoes row material.
#[test]
fn scheduled_report_and_errors_are_count_only() {
    let store = FakeStore::new();
    let (keks, old_kek, _) = FakeKeks::with_target();
    let sentinel_user = "count-only-sentinel-user";
    let seeded = store.seed(sentinel_user, &old_kek, V1, b"sentinel payload");
    let report = invoke(&store, &keks, &config()).expect("pass runs");
    let dumped = format!("{report:?} {report:#?}");
    for secret in [
        sentinel_user,
        seeded.dek_wrapped_b64url.as_str(),
        seeded.dek_wrapped_nonce_b64url.as_str(),
        seeded.payload_b64url.as_str(),
        seeded.payload_nonce_b64url.as_str(),
    ] {
        assert!(
            !dumped.contains(secret),
            "report must not contain row material"
        );
    }

    let (no_target, old_only) = FakeKeks::without_target();
    let err_store = FakeStore::new();
    let err_seeded = err_store.seed(sentinel_user, &old_only, V1, b"sentinel payload");
    let error = invoke(&err_store, &no_target, &config()).expect_err("missing key fails");
    let dumped = format!("{error:?} {error:#?} {error}");
    for secret in [
        sentinel_user,
        err_seeded.dek_wrapped_b64url.as_str(),
        err_seeded.payload_b64url.as_str(),
    ] {
        assert!(
            !dumped.contains(secret),
            "error must not contain row material"
        );
    }
}

fn arming(pairs: &[(&str, &str)]) -> Result<ScheduledRewrapArming, JobError> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    scheduled_rewrap_arming_from_lookup(|name| map.get(name).cloned(), PIN)
}

fn armed_config(pairs: &[(&str, &str)]) -> ScheduledRewrapConfig {
    match arming(pairs).expect("ceremony parses") {
        ScheduledRewrapArming::Armed(config) => config,
        ScheduledRewrapArming::Disabled => panic!("expected an armed ceremony"),
    }
}

const FULL_CEREMONY: &[(&str, &str)] = &[
    ("VAULT_REWRAP_ENABLED", "1"),
    ("VAULT_REWRAP_ENV", "staging-like"),
    ("VAULT_REWRAP_FROM_VERSION", "1"),
    ("VAULT_REWRAP_TO_VERSION", "3"),
    ("VAULT_REWRAP_BATCH_LIMIT", "25"),
    ("VAULT_REWRAP_MAX_BATCHES", "4"),
    ("VAULT_REWRAP_DRY_RUN", "true"),
];

/// Without an explicit opt-in the scheduled event is inert, however
/// complete the rest of the ceremony variables look.
///
/// Mutants killed: treating ceremony vars alone (or a cron fire alone)
/// as authorization to rotate.
#[test]
fn arming_is_disabled_without_explicit_opt_in() {
    assert!(matches!(
        arming(&[]).expect("empty lookup parses"),
        ScheduledRewrapArming::Disabled
    ));
    for off in ["", "0", "false", "FALSE"] {
        let mut pairs: Vec<(&str, &str)> = FULL_CEREMONY
            .iter()
            .filter(|(name, _)| *name != "VAULT_REWRAP_ENABLED")
            .copied()
            .collect();
        pairs.push(("VAULT_REWRAP_ENABLED", off));
        assert!(
            matches!(
                arming(&pairs).expect("disabled ceremony parses"),
                ScheduledRewrapArming::Disabled
            ),
            "ENABLED={off:?} must stay disabled"
        );
    }
}

/// An armed-but-incomplete or ambiguous ceremony is an error, never a
/// best-effort pass: missing pieces, cross-environment names,
/// non-rotations, out-of-range bounds, and unparsable flags all fail.
///
/// Mutants killed: defaulting a missing target to the current write
/// version; splitting a comma env into several passes; clamping an
/// over-limit batch instead of refusing the ceremony.
#[test]
fn arming_rejects_incomplete_or_ambiguous_ceremony() {
    let without = |drop_name: &'static str| -> Vec<(&'static str, &'static str)> {
        FULL_CEREMONY
            .iter()
            .filter(|(name, _)| *name != drop_name)
            .copied()
            .collect::<Vec<_>>()
    };
    let replaced = |name: &'static str, value: &'static str| -> Vec<(&'static str, &'static str)> {
        let mut pairs = without(name);
        pairs.push((name, value));
        pairs
    };
    let cases: Vec<Vec<(&str, &str)>> = vec![
        vec![("VAULT_REWRAP_ENABLED", "1")],
        without("VAULT_REWRAP_ENV"),
        without("VAULT_REWRAP_FROM_VERSION"),
        without("VAULT_REWRAP_TO_VERSION"),
        replaced("VAULT_REWRAP_ENABLED", "yes"),
        replaced("VAULT_REWRAP_ENV", ""),
        replaced("VAULT_REWRAP_ENV", "staging-like,prod-like"),
        replaced("VAULT_REWRAP_ENV", "staging-like prod-like"),
        replaced("VAULT_REWRAP_ENV", "staging-like;prod-like"),
        replaced("VAULT_REWRAP_FROM_VERSION", "three"),
        replaced("VAULT_REWRAP_TO_VERSION", "0"),
        replaced("VAULT_REWRAP_FROM_VERSION", "3"),
        replaced("VAULT_REWRAP_FROM_VERSION", "4"),
        replaced("VAULT_REWRAP_BATCH_LIMIT", "0"),
        replaced("VAULT_REWRAP_BATCH_LIMIT", "101"),
        replaced("VAULT_REWRAP_BATCH_LIMIT", "many"),
        replaced("VAULT_REWRAP_MAX_BATCHES", "0"),
        replaced("VAULT_REWRAP_DRY_RUN", "maybe"),
    ];
    for case in &cases {
        assert!(
            matches!(arming(case), Err(JobError::InvalidParams(_))),
            "ceremony {case:?} must be rejected, got {:?}",
            arming(case).map(|_| "armed/disabled")
        );
    }
}

/// A complete explicit ceremony parses into exactly the configured
/// pass; omitted optional bounds fall back to in-range defaults and an
/// omitted dry-run flag means live.
///
/// Mutants killed: trimming or remapping the env; swapping from/to;
/// ignoring the operator's explicit bounds or dry-run rehearsal.
#[test]
fn arming_parses_an_explicit_ceremony() {
    let config = armed_config(FULL_CEREMONY);
    assert_eq!(config.env, "staging-like");
    assert_eq!((config.from_version, config.to_version), (V1, TARGET));
    assert_eq!((config.batch_limit, config.max_batches), (25, 4));
    assert!(config.dry_run);
    assert_eq!(config.now_secs, PIN, "the invocation time passes through");

    let minimal = armed_config(&[
        ("VAULT_REWRAP_ENABLED", "true"),
        ("VAULT_REWRAP_ENV", "staging-like"),
        ("VAULT_REWRAP_FROM_VERSION", "1"),
        ("VAULT_REWRAP_TO_VERSION", "3"),
    ]);
    assert!(
        (1..=MAX_BATCH_LIMIT).contains(&minimal.batch_limit),
        "default batch limit stays within the validated bound"
    );
    assert!(minimal.max_batches >= 1, "default pass bound is positive");
    assert!(!minimal.dry_run, "absent dry-run flag means live");
}

/// The Postgres-runnable CAS is the normative CAS with named
/// placeholders rendered positionally in first-appearance order, `$now`
/// bound once. The normative const itself is untouched (the sibling
/// job tests pin it byte-for-byte against the runbook).
///
/// Mutants killed: executing the named form against Postgres (driver
/// rejects unknown parameters); reordering binds so the captured
/// envelope compares against the wrong columns; "fixing" the normative
/// const instead of adding the positional rendering.
#[test]
fn postgres_cas_sql_binds_the_normative_cas_positionally() {
    // Replace longest names first so `$n` can never eat `$new`/`$now`
    // and `$captured_wrapped` never eats its `_nonce` sibling.
    let expected = CAS_REWRAP_SQL
        .replace("$captured_wrapped_nonce", "$8")
        .replace("$captured_wrapped", "$7")
        .replace("$new", "$1")
        .replace("$now", "$4")
        .replace("$old", "$6")
        .replace("$w", "$2")
        .replace("$n", "$3")
        .replace("$u", "$5");
    assert_eq!(
        CAS_REWRAP_POSTGRES_SQL, expected,
        "positional CAS must be the normative statement, re-bound only"
    );
    for named in [
        "$new",
        "$now",
        "$old",
        "$captured_wrapped",
        "$captured_wrapped_nonce",
    ] {
        assert!(
            !CAS_REWRAP_POSTGRES_SQL.contains(named),
            "named placeholder {named} survives into Postgres SQL"
        );
    }
    assert_eq!(
        CAS_REWRAP_POSTGRES_SQL.matches("$4").count(),
        2,
        "both timestamp columns bind the single pass time"
    );
    for positional in ["$1", "$2", "$3", "$5", "$6", "$7", "$8"] {
        assert_eq!(
            CAS_REWRAP_POSTGRES_SQL.matches(positional).count(),
            1,
            "{positional} binds exactly once"
        );
    }
}
