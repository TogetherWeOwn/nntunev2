//! Pure durable-run model for ListenBrainz KEK rotation. No I/O.
//!
//! A run is one row of the dedicated `listenbrainz_rotation_runs` ledger (not
//! the vault `rotation_runs` ledger). Each committed batch advances an opaque
//! resume cursor, a checked `checkpoint_seq` and six checked counters. These
//! methods validate a row read back from storage and compute the next
//! checkpoint; the store applies it under the run-row lock.
//!
//! `finished` means only that the keyset scan was exhausted. Failed and
//! deferred rows stay visible in the counters and are never hidden by
//! finishing. A finished run is NOT proof that every link is rotated and must
//! not be used alone to retire a key.

use super::{OwnerCursor, RotationBatchReport};
use crate::handlers::StoreError;

/// Longest run id (bytes).
pub(crate) const MAX_RUN_ID_BYTES: usize = 128;
/// Longest environment label (bytes).
pub(crate) const MAX_ENV_BYTES: usize = 64;

fn valid_label(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Caller-chosen stable run id: 1..=128 ASCII alphanumeric, `-` or `_` (matches the table CHECK).
pub(crate) fn valid_run_id(id: &str) -> bool {
    valid_label(id, MAX_RUN_ID_BYTES)
}

/// Environment label: 1..=64 ASCII alphanumeric, `-` or `_` (matches the table CHECK).
pub(crate) fn valid_env(env: &str) -> bool {
    valid_label(env, MAX_ENV_BYTES)
}

/// Target versions must fit the INT column and be positive.
pub(crate) fn valid_target(to_version: u32) -> bool {
    to_version != 0 && i32::try_from(to_version).is_ok()
}

/// One durable rotation run. Scalars and one opaque cursor only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RotationRun {
    pub(crate) id: String,
    pub(crate) env: String,
    pub(crate) to_version: u32,
    pub(crate) owner_cursor: Option<OwnerCursor>,
    pub(crate) checkpoint_seq: i64,
    pub(crate) scanned: i64,
    pub(crate) rewrapped: i64,
    pub(crate) no_link: i64,
    pub(crate) already_current: i64,
    pub(crate) deferred: i64,
    pub(crate) failed: i64,
    pub(crate) finished: bool,
}

impl RotationRun {
    /// A fresh, unfinished run with no progress.
    pub(crate) fn new(id: &str, env: &str, to_version: u32) -> Result<Self, StoreError> {
        let run = Self {
            id: id.to_owned(),
            env: env.to_owned(),
            to_version,
            owner_cursor: None,
            checkpoint_seq: 0,
            scanned: 0,
            rewrapped: 0,
            no_link: 0,
            already_current: 0,
            deferred: 0,
            failed: 0,
            finished: false,
        };
        run.validate()?;
        Ok(run)
    }

    /// Sum of the five outcome counters, or `None` on overflow.
    fn outcome_sum(&self) -> Option<i64> {
        self.rewrapped
            .checked_add(self.no_link)?
            .checked_add(self.already_current)?
            .checked_add(self.deferred)?
            .checked_add(self.failed)
    }

    /// Shape and invariant check for a row read back from storage or built
    /// here: identifiers, positive target, non-negative counters, and
    /// `scanned` equal to the checked sum of the other five.
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        let counters = [
            self.scanned,
            self.rewrapped,
            self.no_link,
            self.already_current,
            self.deferred,
            self.failed,
        ];
        if !valid_run_id(&self.id)
            || !valid_env(&self.env)
            || !valid_target(self.to_version)
            || self.checkpoint_seq < 0
            || counters.iter().any(|c| *c < 0)
            || self.outcome_sum() != Some(self.scanned)
        {
            return Err(StoreError);
        }
        Ok(())
    }

    /// The run must be exactly this env and target, at the caller's expected
    /// sequence, and not finished.
    pub(crate) fn fence(
        &self,
        env: &str,
        to_version: u32,
        expected_seq: i64,
    ) -> Result<(), StoreError> {
        if self.env != env
            || self.to_version != to_version
            || self.checkpoint_seq != expected_seq
            || expected_seq < 0
            || self.finished
        {
            return Err(StoreError);
        }
        Ok(())
    }

    /// The run after applying one batch report. Checked arithmetic
    /// throughout: sequence exhaustion or counter overflow is an error and
    /// nothing is partially applied. `finished` follows scan exhaustion only.
    pub(crate) fn checkpoint(
        &self,
        expected_seq: i64,
        report: &RotationBatchReport,
    ) -> Result<Self, StoreError> {
        self.fence(&self.env, self.to_version, expected_seq)?;
        self.validate()?;
        let parts = [
            report.rewrapped,
            report.no_link,
            report.already_current,
            report.deferred,
            report.failed,
        ];
        let mut sum: u32 = 0;
        for p in parts {
            sum = sum.checked_add(p).ok_or(StoreError)?;
        }
        if sum != report.scanned {
            return Err(StoreError);
        }
        let add = |a: i64, b: u32| a.checked_add(i64::from(b)).ok_or(StoreError);
        let next = Self {
            id: self.id.clone(),
            env: self.env.clone(),
            to_version: self.to_version,
            owner_cursor: report.resume_after.clone(),
            checkpoint_seq: self.checkpoint_seq.checked_add(1).ok_or(StoreError)?,
            scanned: add(self.scanned, report.scanned)?,
            rewrapped: add(self.rewrapped, report.rewrapped)?,
            no_link: add(self.no_link, report.no_link)?,
            already_current: add(self.already_current, report.already_current)?,
            deferred: add(self.deferred, report.deferred)?,
            failed: add(self.failed, report.failed)?,
            finished: report.end_of_keyset,
        };
        next.validate()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(scanned: u32, rewrapped: u32, failed: u32, end: bool) -> RotationBatchReport {
        RotationBatchReport {
            scanned,
            rewrapped,
            no_link: 0,
            already_current: 0,
            deferred: 0,
            failed,
            resume_after: OwnerCursor::parse("acct-9"),
            end_of_keyset: end,
        }
    }

    #[test]
    fn labels_are_bounded_ascii() {
        assert!(valid_run_id("run-1_a"));
        assert!(valid_run_id(&"a".repeat(128)));
        assert!(!valid_run_id(&"a".repeat(129)));
        assert!(valid_env(&"e".repeat(64)));
        assert!(!valid_env(&"e".repeat(65)));
        for bad in ["", "a b", "a/b", "\u{e9}", "a;b", "a.b", "a:b"] {
            assert!(!valid_run_id(bad) && !valid_env(bad), "{bad}");
        }
        assert!(!valid_target(0) && !valid_target(i32::MAX as u32 + 1));
        assert!(valid_target(1) && valid_target(i32::MAX as u32));
    }

    #[test]
    fn checkpoint_accumulates_and_keeps_failures_visible() {
        let run = RotationRun::new("r1", "prod", 2).unwrap();
        let next = run.checkpoint(0, &report(3, 2, 1, true)).unwrap();
        assert_eq!(
            (
                next.checkpoint_seq,
                next.scanned,
                next.rewrapped,
                next.failed
            ),
            (1, 3, 2, 1)
        );
        assert!(next.finished);
        assert_eq!(next.owner_cursor.as_ref().unwrap().as_str(), "acct-9");
        // A finished run is fenced out of further batches.
        assert!(next.checkpoint(1, &report(0, 0, 0, true)).is_err());
    }

    #[test]
    fn checkpoint_is_fenced_and_checked() {
        let run = RotationRun::new("r1", "prod", 2).unwrap();
        assert!(run.checkpoint(1, &report(1, 1, 0, false)).is_err());
        assert!(run.fence("stage", 2, 0).is_err());
        assert!(run.fence("prod", 3, 0).is_err());
        assert!(run.checkpoint(0, &report(2, 1, 0, false)).is_err());
        let mut max = run.clone();
        max.checkpoint_seq = i64::MAX;
        assert!(max.checkpoint(i64::MAX, &report(0, 0, 0, true)).is_err());
        let mut full = run.clone();
        full.scanned = i64::MAX;
        full.rewrapped = i64::MAX;
        assert!(full.checkpoint(0, &report(1, 1, 0, false)).is_err());
    }

    #[test]
    fn validate_rejects_inconsistent_rows() {
        let mut run = RotationRun::new("r1", "prod", 2).unwrap();
        run.scanned = 1;
        assert!(run.validate().is_err());
        run.scanned = 0;
        run.failed = -1;
        assert!(run.validate().is_err());
        assert!(RotationRun::new("", "prod", 2).is_err());
        assert!(RotationRun::new("r1", "prod", 0).is_err());
    }
}
