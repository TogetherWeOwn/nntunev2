//! Bounded/resumable KEK re-wrap job (`docs/security/kek-rotation.md` §6).
//!
//! This is the executable runner core for routine rotation phase 3: it
//! re-wraps every `provider_vaults` row below a target KEK version to that
//! version — DEK only, payload and nonces preserved — through the §6
//! claim/CAS contract. What runs today, over any [`RewrapStore`]:
//!
//! - cursor-based batch claims (`kek_version < to`, `user_id > cursor`,
//!   never `OFFSET`), at most [`MAX_BATCH_LIMIT`] rows per batch;
//! - per-row re-wrap via [`vault::rewrap_dek`](crate::vault::rewrap_dek)
//!   with version-pinned keys from a [`KekSource`] (production: [`Config`]);
//! - envelope-conditional writes: zero affected rows is a lost claim, which
//!   reloads and retries against the current envelope within the bounded
//!   pass, never overwriting a newer envelope;
//! - poison-row isolation: per-row failures are counted and skipped, never
//!   aborting the pass; contention-exhausted rows defer to a later run;
//! - resumability: cursor + counts persist to the `rotation_runs` record
//!   after every batch, so a Worker eviction resumes mid-run with no
//!   in-memory state;
//! - dry-run mode: claims and re-wraps compute, nothing mutates.
//!
//! [`run_scheduled_rewrap`] orchestrates load/open/pass/close. The Worker
//! scheduled handler and fenced Hyperdrive [`RewrapStore`] live in
//! `worker_entry`; migration `0024_rotation_runs.sql` carries this ledger
//! plus the database-scoped invocation fence. The normative CAS retains
//! named placeholders; [`CAS_REWRAP_POSTGRES_SQL`] is its positional
//! rendering for the production driver. Source/native/SQL proof is not a
//! live ceremony: see the runbook §7 for deployment and drill limitations.
//!
//! Redaction: claims and envelopes carry wrap ciphertext, so their `Debug`
//! impls are manual and redacted; [`RunRecord`] and [`PassReport`] are
//! counts plus a single resume cursor (never ids-in-bulk, never key
//! material); [`JobError`] variants are scalar-only. This module emits no
//! log calls at all — there is no path for ciphertext to reach a log.

use crate::handlers::{Config, StoreError};
use crate::vault::{self, VaultBlob, VaultError};
use std::fmt;
use std::future::Future;

/// Largest batch a pass may claim. Runbook §6: batch ≤ 100 rows.
pub const MAX_BATCH_LIMIT: u32 = 100;

/// Claim/CAS attempts per row per pass before a still-contended row defers
/// to a later run. Lost claims reload and retry against the current
/// envelope; only persistent contention (every attempt loses) defers.
pub const MAX_ROW_ATTEMPTS: u32 = 3;

/// The normative §6 claim SELECT: rows below the target version after the
/// cursor, in `user_id` order, with a bound batch size (`LIMIT $3`).
/// Byte-identical to the runbook text; drift fails the job tests.
pub const CLAIM_BATCH_SQL: &str = "SELECT user_id, kek_version, dek_wrapped, dek_wrapped_nonce
  FROM provider_vaults
 WHERE kek_version < $1 AND user_id > $2
 ORDER BY user_id LIMIT $3;";

/// The normative §6 conditional UPDATE: a compare-and-swap on the captured
/// envelope (user + captured version + captured wrapped DEK and nonce), so
/// a same-version vault PUT between claim and write loses the claim instead
/// of being overwritten. Byte-identical to the runbook text.
pub const CAS_REWRAP_SQL: &str = "UPDATE provider_vaults
      SET kek_version = $new, dek_wrapped = $w,
          dek_wrapped_nonce = $n, updated_at = $now,
          rewrapped_at = $now
    WHERE user_id = $u AND kek_version = $old
      AND dek_wrapped = $captured_wrapped
      AND dek_wrapped_nonce = $captured_wrapped_nonce;";

/// PostgreSQL rendering of the normative named-parameter CAS above.
/// Bind order: new version, wrap, nonce, timestamp, user, captured version,
/// captured wrap, captured nonce. The normative statement stays unchanged.
pub const CAS_REWRAP_POSTGRES_SQL: &str = "UPDATE provider_vaults
      SET kek_version = $1, dek_wrapped = $2,
          dek_wrapped_nonce = $3, updated_at = $4,
          rewrapped_at = $4
    WHERE user_id = $5 AND kek_version = $6
      AND dek_wrapped = $7
      AND dek_wrapped_nonce = $8;";

/// Designed `rotation_runs` progress record (runbook §6: id, env,
/// from/to versions, cursor, counts, started/finished), reconciled with two
/// drill counts the design implies but does not name — `rows_deferred`
/// (contention-exhausted, retried by a later run) and `claims_retried`
/// (lost-claim reloads) — plus `updated_at` for stuck-pass detection. The
/// partial-unique open-run index is the ≤1-concurrent-pass guard: opening a
/// second run for the same `(env, to_version)` while one is unfinished must
/// fail, and the store maps that conflict to
/// [`JobError::PassAlreadyRunning`].
///
/// This DDL executes verbatim on the SQLite stand-in and is mirrored by
/// root migration `0024_rotation_runs.sql`. The migration adds a separate
/// `rotation_state` fence; the progress-record columns stay unchanged.
pub const ROTATION_RUNS_DDL: &str = "CREATE TABLE IF NOT EXISTS rotation_runs (
  id             TEXT PRIMARY KEY,
  env            TEXT NOT NULL,
  from_version   INTEGER NOT NULL,
  to_version     INTEGER NOT NULL,
  cursor         TEXT NULL,
  rows_done      BIGINT NOT NULL DEFAULT 0,
  rows_failed    BIGINT NOT NULL DEFAULT 0,
  rows_deferred  BIGINT NOT NULL DEFAULT 0,
  claims_retried BIGINT NOT NULL DEFAULT 0,
  started_at     BIGINT NOT NULL,
  updated_at     BIGINT NOT NULL,
  finished_at    BIGINT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_rotation_runs_open
  ON rotation_runs (env, to_version) WHERE finished_at IS NULL;";

/// One claimed re-wrap candidate: the captured envelope identity the CAS
/// binds. Wrap-only by construction — the payload ciphertext is never
/// loaded, because re-wrap never touches it.
///
/// `Debug` is manual and redacted: wrap ciphertext must never reach a log.
pub struct RewrapClaim {
    pub user_id: String,
    pub kek_version: u32,
    pub dek_wrapped: String,
    pub dek_wrapped_nonce: String,
}

impl fmt::Debug for RewrapClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RewrapClaim")
            .field("user_id", &self.user_id)
            .field("kek_version", &self.kek_version)
            .field("dek_wrapped", &"<redacted>")
            .field("dek_wrapped_nonce", &"<redacted>")
            .finish()
    }
}

impl RewrapClaim {
    /// View the captured wrap as the blob shape
    /// [`vault::rewrap_dek`](crate::vault::rewrap_dek) consumes. The payload
    /// fields stay empty: re-wrap reads only the wrap, and an empty payload
    /// round-trips back empty, so wrap-only claims cannot smuggle payload
    /// ciphertext through the job.
    fn as_vault_blob(&self) -> VaultBlob {
        VaultBlob {
            kek_version: self.kek_version,
            dek_wrapped_nonce_b64url: self.dek_wrapped_nonce.clone(),
            dek_wrapped_b64url: self.dek_wrapped.clone(),
            payload_nonce_b64url: String::new(),
            payload_b64url: String::new(),
        }
    }
}

/// A freshly re-wrapped envelope, ready for the CAS write. Redacted `Debug`
/// like [`RewrapClaim`].
pub struct RewrappedEnvelope {
    pub kek_version: u32,
    pub dek_wrapped: String,
    pub dek_wrapped_nonce: String,
}

impl fmt::Debug for RewrappedEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RewrappedEnvelope")
            .field("kek_version", &self.kek_version)
            .field("dek_wrapped", &"<redacted>")
            .field("dek_wrapped_nonce", &"<redacted>")
            .finish()
    }
}

/// Outcome of one [`RewrapStore::cas_rewrap`] write: exactly one affected
/// row rewrapped the captured envelope; zero is a lost claim (a concurrent
/// vault PUT or another pass replaced the envelope first), never progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasRewrapOutcome {
    Rewrapped,
    ClaimLost,
}

/// The redacted run/progress record: counts plus a single resume cursor.
/// No user lists, no ciphertext, no key material — by construction there is
/// nowhere to put them. `cursor` is the last fully processed `user_id`
/// (`None` before the first row); a resumed pass claims after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub id: String,
    pub env: String,
    pub from_version: u32,
    pub to_version: u32,
    pub cursor: Option<String>,
    pub rows_done: u64,
    pub rows_failed: u64,
    pub rows_deferred: u64,
    pub claims_retried: u64,
    pub started_at: u64,
    pub updated_at: u64,
    pub finished_at: Option<u64>,
}

impl RunRecord {
    /// A never-persisted record for dry-run rehearsal: the job reads its
    /// cursor as the start point and reports into the returned
    /// [`PassReport`] without touching the store. A live pass over a
    /// transient record is a caller bug and is rejected, because rows the
    /// pass rewraps could never be recorded.
    pub fn transient(env: &str, from_version: u32, to_version: u32, now_secs: u64) -> Self {
        Self {
            id: String::new(),
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
        }
    }
}

/// Bounded-pass parameters. `batch_limit` is rows processed per batch
/// (1–[`MAX_BATCH_LIMIT`]); `max_batches` caps batches per pass (≥ 1), so a
/// pass touches at most `batch_limit × max_batches` rows and reports
/// `complete: false` when the bound stops it early. `now_secs` stamps the
/// CAS writes and progress saves of this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewrapParams {
    pub to_version: u32,
    pub batch_limit: u32,
    pub max_batches: u32,
    pub dry_run: bool,
    pub now_secs: u64,
}

impl RewrapParams {
    fn validate(&self, run: &RunRecord) -> Result<(), JobError> {
        if self.batch_limit == 0 || self.batch_limit > MAX_BATCH_LIMIT {
            return Err(JobError::InvalidParams(
                "batch_limit must be within 1..=MAX_BATCH_LIMIT",
            ));
        }
        if self.max_batches == 0 {
            return Err(JobError::InvalidParams("max_batches must be at least 1"));
        }
        if self.to_version == 0 {
            return Err(JobError::InvalidParams("to_version must not be zero"));
        }
        if self.to_version != run.to_version {
            return Err(JobError::InvalidParams(
                "pass target must match its run target",
            ));
        }
        validate_new_run(&run.env, run.from_version, run.to_version)?;
        if !self.dry_run && run.id.is_empty() {
            return Err(JobError::InvalidParams(
                "live pass requires a persisted run",
            ));
        }
        Ok(())
    }
}

/// Per-pass counts. `complete` is true only when a claim came back short
/// (no rows remain below the target after the cursor); a pass stopped by
/// `max_batches` reports `complete: false` for the trigger to resume.
/// Counter semantics: `done` rows reached the target version this pass (or,
/// in dry-run, would have); `failed` rows errored per-row (unknown KEK
/// version, corrupt/tampered wrap, per-row store failure) and were skipped
/// past; `deferred` rows lost every CAS attempt to concurrent PUTs and keep
/// their version for a later run; `retried` counts lost-claim reloads.
/// Rows deleted mid-pass count in `scanned` only — there is nothing left to
/// rewrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PassReport {
    pub scanned: u64,
    pub done: u64,
    pub failed: u64,
    pub deferred: u64,
    pub retried: u64,
    pub complete: bool,
}

/// Job failures. Scalar-only by construction: no variant can format a blob,
/// a cursor, or key material. `Display` stays generic (versions are
/// operator-visible in row counts, never user-facing through this type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    InvalidParams(&'static str),
    UnknownKekVersion(u32),
    PassAlreadyRunning,
    Store,
    RandomnessUnavailable,
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParams(reason) => write!(f, "invalid rewrap params: {reason}"),
            Self::UnknownKekVersion(_) => write!(f, "unknown KEK version"),
            Self::PassAlreadyRunning => write!(f, "rewrap pass already running"),
            Self::Store => write!(f, "rewrap store unavailable"),
            Self::RandomnessUnavailable => write!(f, "secure randomness unavailable"),
        }
    }
}

impl From<StoreError> for JobError {
    fn from(_: StoreError) -> Self {
        Self::Store
    }
}

impl std::error::Error for JobError {}

/// Validate a would-be run before it opens: one environment, a real
/// rotation (`from < to`, nonzero target). Store implementations call this
/// from `open_run` so every backend enforces the same shape.
pub fn validate_new_run(env: &str, from_version: u32, to_version: u32) -> Result<(), JobError> {
    if env.is_empty() {
        return Err(JobError::InvalidParams("run env must name one environment"));
    }
    if to_version == 0 {
        return Err(JobError::InvalidParams("to_version must not be zero"));
    }
    if from_version >= to_version {
        return Err(JobError::InvalidParams(
            "from_version must be below to_version",
        ));
    }
    Ok(())
}

/// Version-pinned KEK lookup for the job: exactly one candidate per
/// version, `None` when unprovisioned. Production serves this through
/// [`Config::kek_by_version`](crate::handlers::Config::kek_by_version), so
/// rows at mixed versions resolve during phases 1–2 and unknown versions
/// fail their row (never the pass, never another version's key).
pub trait KekSource {
    fn kek(&self, version: u32) -> Option<&[u8; 32]>;
}

impl KekSource for Config {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        self.kek_by_version(version)
    }
}

/// Persistence the pass needs: wrap-only vault claims + the CAS write, and
/// the `rotation_runs` ledger. Claim/CAS SQL is [`CLAIM_BATCH_SQL`] /
/// [`CAS_REWRAP_SQL`]; ledger statements follow [`ROTATION_RUNS_DDL`].
/// Parameterized statements only, like every other store in this crate.
pub trait RewrapStore {
    /// Claim up to `limit` rows below `to_version` after `after_user_id`
    /// (`None` starts from the beginning), in `user_id` order.
    fn claim_batch(
        &self,
        to_version: u32,
        after_user_id: Option<&str>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<RewrapClaim>, StoreError>>;
    /// Reload one row's current envelope (lost-claim retry).
    fn reload_row(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<RewrapClaim>, StoreError>>;
    /// Conditional re-wrap write: exactly one affected row is success, zero
    /// is a lost claim. Must touch only the wrap columns (+ `updated_at` /
    /// `rewrapped_at`); `payload` and `payload_nonce` stay untouched.
    fn cas_rewrap(
        &self,
        user_id: &str,
        captured: &RewrapClaim,
        rewrapped: &RewrappedEnvelope,
        now_secs: u64,
    ) -> impl Future<Output = Result<CasRewrapOutcome, StoreError>>;
    /// Open a run, enforcing the ≤1-concurrent-pass guard: a second open
    /// for the same `(env, to_version)` while one is unfinished returns
    /// [`JobError::PassAlreadyRunning`]. Validates via [`validate_new_run`].
    fn open_run(
        &self,
        env: &str,
        from_version: u32,
        to_version: u32,
        now_secs: u64,
    ) -> impl Future<Output = Result<RunRecord, JobError>>;
    /// The unfinished run for `(env, to_version)`, if any (resume path).
    fn load_open_run(
        &self,
        env: &str,
        to_version: u32,
    ) -> impl Future<Output = Result<Option<RunRecord>, StoreError>>;
    /// Persist cursor + counts + `updated_at` for an unfinished run. Zero
    /// affected rows (closed or never opened under this pass) fails closed.
    fn save_progress(&self, run: &RunRecord) -> impl Future<Output = Result<(), StoreError>>;
    /// Mark a run finished. Idempotent: double close is a no-op.
    fn close_run(
        &self,
        run_id: &str,
        now_secs: u64,
    ) -> impl Future<Output = Result<(), StoreError>>;
}

/// An explicitly armed ceremony; cron never derives or changes a key version.
#[derive(Debug, Clone)]
pub struct ScheduledRewrapConfig {
    pub env: String,
    pub from_version: u32,
    pub to_version: u32,
    pub batch_limit: u32,
    pub max_batches: u32,
    pub dry_run: bool,
    pub now_secs: u64,
}

impl ScheduledRewrapConfig {
    pub fn params(&self) -> RewrapParams {
        RewrapParams {
            to_version: self.to_version,
            batch_limit: self.batch_limit,
            max_batches: self.max_batches,
            dry_run: self.dry_run,
            now_secs: self.now_secs,
        }
    }

    /// Shared preflight, also used before production acquires its lease.
    pub fn validate(&self, keks: &impl KekSource) -> Result<(), JobError> {
        if self
            .env
            .chars()
            .any(|c| c.is_whitespace() || c == ',' || c == ';')
        {
            return Err(JobError::InvalidParams("run env must name one environment"));
        }
        let transient =
            RunRecord::transient(&self.env, self.from_version, self.to_version, self.now_secs);
        // A live pass requires persistence, but preflight must precede that
        // very mutation. Validate the remaining shape as a transient rehearsal.
        let mut params = self.params();
        params.dry_run = true;
        params.validate(&transient)?;
        keks.kek(self.to_version)
            .ok_or(JobError::UnknownKekVersion(self.to_version))?;
        Ok(())
    }
}

/// Safe to emit: counts and flags only, never run IDs or resume cursors.
#[derive(Debug)]
pub struct ScheduledRewrapReport {
    pub pass: PassReport,
    pub resumed: bool,
    pub closed: bool,
}

pub enum ScheduledRewrapArming {
    Disabled,
    Armed(ScheduledRewrapConfig),
}

fn rewrap_flag(raw: Option<String>) -> Result<bool, JobError> {
    match raw.as_deref() {
        None | Some("") | Some("0") | Some("false") | Some("FALSE") => Ok(false),
        Some("1") | Some("true") | Some("TRUE") => Ok(true),
        _ => Err(JobError::InvalidParams("invalid rewrap flag")),
    }
}

/// Presence-preserving parsing; bad or partial arming is never a silent no-op.
pub fn scheduled_rewrap_arming_from_lookup(
    get: impl Fn(&str) -> Option<String>,
    now_secs: u64,
) -> Result<ScheduledRewrapArming, JobError> {
    if !rewrap_flag(get("VAULT_REWRAP_ENABLED"))? {
        return Ok(ScheduledRewrapArming::Disabled);
    }
    let number = |name: &str, default: Option<u32>| -> Result<u32, JobError> {
        match get(name) {
            Some(raw) => raw
                .parse()
                .map_err(|_| JobError::InvalidParams("invalid rewrap bound or version")),
            None => default.ok_or(JobError::InvalidParams("missing rewrap version")),
        }
    };
    let config = ScheduledRewrapConfig {
        env: get("VAULT_REWRAP_ENV").ok_or(JobError::InvalidParams("missing rewrap env"))?,
        from_version: number("VAULT_REWRAP_FROM_VERSION", None)?,
        to_version: number("VAULT_REWRAP_TO_VERSION", None)?,
        batch_limit: number("VAULT_REWRAP_BATCH_LIMIT", Some(20))?,
        max_batches: number("VAULT_REWRAP_MAX_BATCHES", Some(1))?,
        dry_run: rewrap_flag(get("VAULT_REWRAP_DRY_RUN"))?,
        now_secs,
    };
    if config
        .env
        .chars()
        .any(|c| c.is_whitespace() || c == ',' || c == ';')
    {
        return Err(JobError::InvalidParams("run env must name one environment"));
    }
    let mut params = config.params();
    params.dry_run = true;
    params.validate(&RunRecord::transient(
        &config.env,
        config.from_version,
        config.to_version,
        now_secs,
    ))?;
    Ok(ScheduledRewrapArming::Armed(config))
}

/// One invocation, one bounded pass. Production must fence overlapping
/// invocations for the entire ceremony; the partial-unique ledger index
/// prevents duplicate open records, not concurrent writes to one cursor.
/// The Hyperdrive implementation additionally fences every transaction.
pub async fn run_scheduled_rewrap(
    store: &impl RewrapStore,
    keks: &impl KekSource,
    config: &ScheduledRewrapConfig,
) -> Result<ScheduledRewrapReport, JobError> {
    config.validate(keks)?;
    let mut run = RunRecord::transient(
        &config.env,
        config.from_version,
        config.to_version,
        config.now_secs,
    );
    let mut resumed = false;
    if !config.dry_run {
        let existing = store.load_open_run(&config.env, config.to_version).await?;
        run = match existing {
            Some(existing) => {
                resumed = true;
                existing
            }
            None => match store
                .open_run(
                    &config.env,
                    config.from_version,
                    config.to_version,
                    config.now_secs,
                )
                .await
            {
                Ok(opened) => opened,
                Err(JobError::PassAlreadyRunning) => {
                    resumed = true;
                    store
                        .load_open_run(&config.env, config.to_version)
                        .await?
                        .ok_or(JobError::PassAlreadyRunning)?
                }
                Err(error) => return Err(error),
            },
        };
        if run.env != config.env
            || run.from_version != config.from_version
            || run.to_version != config.to_version
            || run.finished_at.is_some()
        {
            return Err(JobError::InvalidParams(
                "persisted run differs from ceremony",
            ));
        }
    }
    let pass = run_rewrap_pass(store, keks, &mut run, &config.params()).await?;
    let closed = pass.complete && !config.dry_run;
    if closed {
        store.close_run(&run.id, config.now_secs).await?;
    }
    Ok(ScheduledRewrapReport {
        pass,
        resumed,
        closed,
    })
}

/// Outcome of one row's bounded attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowOutcome {
    Done,
    Failed,
    Deferred,
    Gone,
}

/// Execute one bounded pass of `run`: claim batches from the run cursor,
/// rewrap each row, persist progress per batch. Resuming after eviction is
/// just calling this again with the reloaded run — already-rewrapped rows
/// no longer claim (their version reached the target), so a resumed pass
/// never rewraps twice.
///
/// Fail-closed ordering: params validate and the target key resolves before
/// the first claim, so a misconfigured pass mutates nothing. Claim and
/// progress-save failures abort the pass (resume later); per-row failures
/// count and continue. [`VaultError::RandomnessUnavailable`] aborts the
/// pass: broken RNG is environmental, not per-row, and every further row
/// would fail the same way.
pub async fn run_rewrap_pass(
    store: &impl RewrapStore,
    keks: &impl KekSource,
    run: &mut RunRecord,
    params: &RewrapParams,
) -> Result<PassReport, JobError> {
    params.validate(run)?;
    let new_kek = keks
        .kek(params.to_version)
        .ok_or(JobError::UnknownKekVersion(params.to_version))?;

    let mut report = PassReport::default();
    // Pagination cursor: advances past every visited row in both modes so
    // a multi-batch dry-run pages forward instead of re-claiming the first
    // batch. Only live passes write it back to the run record — a rehearsal
    // leaves `run` (cursor, counts, timestamps) untouched.
    let mut cursor = run.cursor.clone();
    for _ in 0..params.max_batches {
        let batch = store
            .claim_batch(params.to_version, cursor.as_deref(), params.batch_limit)
            .await?;
        if batch.is_empty() {
            report.complete = true;
            break;
        }
        let exhausted = batch.len() < params.batch_limit as usize;
        let batch_start = (report.done, report.failed, report.deferred, report.retried);
        for claimed in batch {
            report.scanned += 1;
            let (outcome, last_user, lost_claims) =
                rewrap_one_row(store, keks, new_kek, claimed, params).await?;
            report.retried += lost_claims;
            match outcome {
                RowOutcome::Done => report.done += 1,
                RowOutcome::Failed => report.failed += 1,
                RowOutcome::Deferred => report.deferred += 1,
                RowOutcome::Gone => {}
            }
            cursor = Some(last_user.clone());
            if !params.dry_run {
                run.cursor = Some(last_user);
            }
        }
        if !params.dry_run {
            run.rows_done += report.done - batch_start.0;
            run.rows_failed += report.failed - batch_start.1;
            run.rows_deferred += report.deferred - batch_start.2;
            run.claims_retried += report.retried - batch_start.3;
            run.updated_at = params.now_secs;
            store.save_progress(run).await?;
        }
        if exhausted {
            report.complete = true;
            break;
        }
    }
    Ok(report)
}

/// Rewrap one claimed row within [`MAX_ROW_ATTEMPTS`] attempts. Returns the
/// row outcome, the user id the cursor must advance past (the user id never
/// changes across reloads — reloads are by that same id), and the lost
/// claims encountered (each reloaded and, attempts permitting, retried).
async fn rewrap_one_row(
    store: &impl RewrapStore,
    keks: &impl KekSource,
    new_kek: &[u8; 32],
    claimed: RewrapClaim,
    params: &RewrapParams,
) -> Result<(RowOutcome, String, u64), JobError> {
    let mut current = claimed;
    let mut lost_claims = 0;
    for _ in 0..MAX_ROW_ATTEMPTS {
        let old_kek = match keks.kek(current.kek_version) {
            Some(key) => key,
            // Unknown KEK version: poison, not contention — no retry.
            None => return Ok((RowOutcome::Failed, current.user_id, lost_claims)),
        };
        let rewrapped = match vault::rewrap_dek(
            old_kek,
            new_kek,
            params.to_version,
            &current.user_id,
            &current.as_vault_blob(),
        ) {
            Ok(blob) => RewrappedEnvelope {
                kek_version: params.to_version,
                dek_wrapped: blob.dek_wrapped_b64url,
                dek_wrapped_nonce: blob.dek_wrapped_nonce_b64url,
            },
            Err(VaultError::RandomnessUnavailable) => {
                return Err(JobError::RandomnessUnavailable);
            }
            // Bad tag, malformed wrap, or any other per-row crypto failure:
            // poison, not contention — no retry.
            Err(_) => return Ok((RowOutcome::Failed, current.user_id, lost_claims)),
        };
        if params.dry_run {
            return Ok((RowOutcome::Done, current.user_id, lost_claims));
        }
        match store
            .cas_rewrap(&current.user_id, &current, &rewrapped, params.now_secs)
            .await?
        {
            CasRewrapOutcome::Rewrapped => {
                return Ok((RowOutcome::Done, current.user_id, lost_claims));
            }
            CasRewrapOutcome::ClaimLost => {
                // The stale rewrap is discarded; the row reloads and — while
                // attempts remain — retries against its current envelope,
                // never overwriting the newer one.
                lost_claims += 1;
                match store.reload_row(&current.user_id).await? {
                    None => return Ok((RowOutcome::Gone, current.user_id, lost_claims)),
                    Some(reloaded) if reloaded.kek_version >= params.to_version => {
                        return Ok((RowOutcome::Gone, current.user_id, lost_claims));
                    }
                    Some(reloaded) => {
                        current = reloaded;
                    }
                }
            }
        }
    }
    // Every attempt lost its claim to a concurrent PUT: the row keeps its
    // version and a later run retries it. Deferring (not failing, not
    // wedging) is what keeps a hot row from blocking the pass.
    Ok((RowOutcome::Deferred, current.user_id, lost_claims))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_run() -> RunRecord {
        RunRecord {
            id: "run-1".to_string(),
            env: "staging".to_string(),
            from_version: 1,
            to_version: 2,
            cursor: None,
            rows_done: 0,
            rows_failed: 0,
            rows_deferred: 0,
            claims_retried: 0,
            started_at: NOW,
            updated_at: NOW,
            finished_at: None,
        }
    }

    fn live_params() -> RewrapParams {
        RewrapParams {
            to_version: 2,
            batch_limit: MAX_BATCH_LIMIT,
            max_batches: 1,
            dry_run: false,
            now_secs: NOW,
        }
    }

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn params_validate_before_any_claim() {
        assert!(live_params().validate(&live_run()).is_ok());
        for params in [
            RewrapParams {
                batch_limit: 0,
                ..live_params()
            },
            RewrapParams {
                batch_limit: MAX_BATCH_LIMIT + 1,
                ..live_params()
            },
            RewrapParams {
                max_batches: 0,
                ..live_params()
            },
            RewrapParams {
                to_version: 0,
                ..live_params()
            },
            RewrapParams {
                to_version: 3,
                ..live_params()
            },
        ] {
            assert!(
                matches!(
                    params.validate(&live_run()),
                    Err(JobError::InvalidParams(_))
                ),
                "params must be rejected: {params:?}"
            );
        }
        // A live pass over a transient record is refused: rewrapped rows
        // could never be recorded.
        let transient = RunRecord::transient("staging", 1, 2, NOW);
        assert!(matches!(
            live_params().validate(&transient),
            Err(JobError::InvalidParams(_))
        ));
        // Dry-run accepts the transient record (it persists nothing).
        assert!(RewrapParams {
            dry_run: true,
            ..live_params()
        }
        .validate(&transient)
        .is_ok());
        // Non-rotations and empty envs are refused at the run, not mid-pass.
        for run in [
            RunRecord {
                from_version: 2,
                ..live_run()
            },
            RunRecord {
                env: String::new(),
                ..live_run()
            },
        ] {
            assert!(
                matches!(
                    live_params().validate(&run),
                    Err(JobError::InvalidParams(_))
                ),
                "run must be rejected: {run:?}"
            );
        }
    }

    #[test]
    fn new_run_shape_is_shared_with_stores() {
        assert!(validate_new_run("staging", 1, 2).is_ok());
        assert!(validate_new_run("", 1, 2).is_err());
        assert!(validate_new_run("staging", 2, 2).is_err());
        assert!(validate_new_run("staging", 3, 2).is_err());
        assert!(validate_new_run("staging", 1, 0).is_err());
    }
}
