//! Offsite backup bundle format + proven-restore drill tooling.
//!
//! Native-only: this crate must never become a Worker dependency (it
//! exists for the operator backup path and CI, not the data plane).
//! See `docs/offsite-backup-runbook.md` for the scope, retention and
//! drill procedures this code supports.
//!
//! - [`bundle`]: bundle format v1 (manifest + SHA-256 content files).
//! - [`envelope`]: drill-local v2 field envelope for per-file media
//!   keys, byte-compatible in AAD framing with the production
//!   `track_sources.nzb_ref` seal path.
//! - [`drill`]: synthetic backup → wipe → isolated-restore → ranged
//!   decrypt drill emitting secrets-free [`drill::DrillEvidence`].

pub mod bundle;
pub mod drill;
pub mod envelope;
pub mod verify;
