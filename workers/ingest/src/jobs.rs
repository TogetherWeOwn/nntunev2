//! Ingest job state machine: idempotent and resumable.
//!
//! Legacy jobs move `Queued -> Searching|Parsing -> Probing -> Verifying
//! -> (Repairing -> Verifying)? -> Matching -> Transcoding -> Writing -> Done`.
//! Admin uploads instead move `Queued -> Parsing -> Matching -> Probing
//! -> Writing -> Done` (published), with explicit review/archive parking.
//! `Done`/`Failed`/`Rejected` are terminal. Workers persist the [`JobCheckpoint`] after every step, so a
//! crashed or evicted run resumes from the last checkpoint instead of
//! restarting (Queue redelivery + Workflow step replay both converge here).

use serde::{Deserialize, Serialize};

use crate::probe::ProbeTally;

/// How many times a single job may attempt a step before it is `Failed`.
pub const MAX_ATTEMPTS: u32 = 5;

/// How many `Verifying -> Repairing` rounds one job may start before a
/// still-failing verify fails the job instead of dispatching another paid
/// Container repair (see [`advance_checkpoint`]).
pub const MAX_REPAIR_ROUNDS: u32 = 3;

/// All ingest stages. Serialized snake_case for Queue/Workflow payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Searching,
    Parsing,
    Probing,
    Verifying,
    Repairing,
    Matching,
    Transcoding,
    Writing,
    NeedsReview,
    NeedsArchiveRead,
    Rejected,
    Done,
    Failed,
}

/// How the job's NZB entered the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobSource {
    IndexerSearch,
    NzbUpload,
    /// Content-derived admin identity; never admitted through the legacy
    /// self-asserted user upload or indexer-search routes.
    AdminUpload,
}

/// Durable resume point written after every completed step.
///
/// `attempt` is the Queue-delivery attempt (1-based), persisted by the
/// consumer in `INGEST_STATE`; Queue message bodies are immutable on redelivery.
/// [`advance`] never touches it, so per-state retries cannot silently reset the
/// job-level budget. `repair_rounds` counts started `Verifying -> Repairing` rounds and
/// bounds the verify↔repair loop (see [`advance_checkpoint`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCheckpoint {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub source: JobSource,
    pub state: JobState,
    pub attempt: u32,
    /// Repair rounds started so far. `#[serde(default)]` keeps checkpoints
    /// written before this field existed readable (they resume with 0).
    #[serde(default)]
    pub repair_rounds: u32,
    /// Probe answers collected while `Probing`, keyed by batch index (see
    /// [`ProbeTally`](crate::probe::ProbeTally)). The completed tally is
    /// retained into `Verifying` (its missing-segment total is that step's
    /// input) and cleared on redrive; `None` before `Probing` is first
    /// entered. `#[serde(default)]` keeps pre-tally checkpoints readable
    /// (a crash before this field existed resumes probing from batch zero).
    #[serde(default)]
    pub probe_tally: Option<ProbeTally>,
    /// Catalogue-only match identity for admin execution. No parser content
    /// is retained. Old admin checkpoints lack a match and fail closed before
    /// probing/publication; legacy uploads do not use this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_release_mbid: Option<String>,
    /// PAR2 set covering the damaged files, recorded by `Verifying`
    /// before it hands off to `Repairing`. `None` until verify runs;
    /// `#[serde(default)]` keeps pre-existing checkpoints readable.
    #[serde(default)]
    pub par2_set_id: Option<String>,
    /// Blocks verify found missing, recorded alongside `par2_set_id`;
    /// the `Repairing` step dispatches exactly this repair request.
    #[serde(default)]
    pub missing_blocks: u32,
    /// Stable machine-readable failure category, retained for DLQ redelivery.
    #[serde(default)]
    pub failure_code: Option<String>,
    pub failure_reason: Option<String>,
    /// Pool-fetch attribution (see [`QueueMsg::provider_id`](crate::queue::QueueMsg::provider_id)).
    /// Projected from the message at admission and carried as the live
    /// copy thereafter: Queue bodies are immutable on redelivery, so the
    /// checkpoint — not the redelivered message — is what the grant
    /// publication reads. `#[serde(default)]` keeps pre-attribution
    /// checkpoints readable (they resume unattributed).
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Verified audio inputs awaiting `Transcoding`, recorded by
    /// `Verifying` on `Complete` (see
    /// [`TranscodeInput`](crate::transcode::TranscodeInput)). Empty
    /// until verify runs; `#[serde(default)]` keeps pre-existing
    /// checkpoints readable (they record on their next verify).
    #[serde(default)]
    pub transcode_inputs: Vec<crate::transcode::TranscodeInput>,
    /// Index of the next input `Transcoding` must encode. Advances
    /// past each published artifact; `#[serde(default)]` restarts
    /// pre-existing checkpoints at the first input.
    #[serde(default)]
    pub transcode_next: usize,
}

/// Advance one step on success. `repair_needed` is only read in `Verifying`.
pub fn advance(state: JobState, source: JobSource, repair_needed: bool) -> JobState {
    if source == JobSource::AdminUpload {
        return match state {
            JobState::Queued => JobState::Parsing,
            JobState::Parsing => JobState::Matching,
            JobState::Matching => JobState::Probing,
            JobState::Probing => JobState::Writing,
            JobState::Writing => JobState::Done,
            // These stages require the retired user-artifact pipeline.
            // A stale/corrupt admin checkpoint must not dispatch them.
            JobState::Searching
            | JobState::Verifying
            | JobState::Repairing
            | JobState::Transcoding => JobState::NeedsReview,
            _ => state,
        };
    }
    match state {
        JobState::Queued => match source {
            JobSource::IndexerSearch => JobState::Searching,
            JobSource::NzbUpload | JobSource::AdminUpload => JobState::Parsing,
        },
        JobState::Searching => JobState::Parsing,
        JobState::Parsing => JobState::Probing,
        JobState::Probing => JobState::Verifying,
        JobState::Verifying => {
            if repair_needed {
                JobState::Repairing
            } else {
                JobState::Matching
            }
        }
        JobState::Repairing => JobState::Verifying,
        JobState::Matching => JobState::Transcoding,
        JobState::Transcoding => JobState::Writing,
        JobState::Writing => JobState::Done,
        JobState::NeedsReview
        | JobState::NeedsArchiveRead
        | JobState::Rejected
        | JobState::Done
        | JobState::Failed => state,
    }
}

/// Advance a checkpointed job one step, bounding the verify↔repair loop.
///
/// Admin match/probe/write transitions require the retained match and probe
/// evidence (see [`crate::admin_gates`]); other transitions delegate to
/// [`advance`] except `Verifying` with `repair_needed`. A repair start
/// increments `repair_rounds` and moves to
/// `Repairing` while `repair_rounds < MAX_REPAIR_ROUNDS`; once the budget is
/// spent the job moves to `Failed` with `failure_reason` set instead of
/// dispatching another Container repair. A par2 set with too few recovery
/// blocks therefore terminates instead of cycling forever. `attempt` is left
/// alone (it belongs to Queue redelivery, not to in-job loops).
pub fn advance_checkpoint(cp: &mut JobCheckpoint, repair_needed: bool) -> JobState {
    if cp.source == JobSource::AdminUpload {
        match cp.state {
            // Match completion must explicitly record the selected release
            // and start a fresh probe plan, never reuse an earlier tally.
            JobState::Matching => {
                cp.state = JobState::NeedsReview;
                return cp.state;
            }
            JobState::Probing => return crate::admin_gates::advance_probe(cp),
            JobState::Writing if !crate::admin_gates::publish_ready(cp) => {
                cp.state = JobState::NeedsReview;
                return cp.state;
            }
            _ => {}
        }
    }
    match cp.state {
        JobState::Verifying if repair_needed && cp.source != JobSource::AdminUpload => {
            if cp.repair_rounds >= MAX_REPAIR_ROUNDS {
                cp.failure_code = Some("repair_budget_exhausted".to_string());
                cp.failure_reason = Some("repair budget exhausted".to_string());
                cp.state = JobState::Failed;
            } else {
                cp.repair_rounds += 1;
                cp.state = JobState::Repairing;
            }
        }
        _ => {
            cp.state = advance(cp.state, cp.source, repair_needed);
        }
    }
    cp.state
}

/// Complete an admin parse using the shared parser's classification, never
/// a scan of subject prose. Call only after loading and validating the
/// retained original. Any archive parks the whole job before matching or
/// provider reads, including an archive mixed with loose audio. Only the
/// state changes: no parser content enters the checkpoint.
pub fn complete_admin_parse(cp: &mut JobCheckpoint, parsed: &nntune_core::nzb::Document) -> bool {
    if cp.source != JobSource::AdminUpload || cp.state != JobState::Parsing {
        return false;
    }
    if parsed
        .files
        .iter()
        .any(|file| file.role == "archive" || matches!(file.container.as_str(), "rar" | "7z"))
    {
        park_archive(cp)
    } else {
        advance_checkpoint(cp, false);
        true
    }
}

/// Park a parsed admin archive before matching or spending provider reads.
/// The original and identity remain available for an explicit later resume.
pub fn park_archive(cp: &mut JobCheckpoint) -> bool {
    if cp.source != JobSource::AdminUpload || cp.state != JobState::Parsing {
        return false;
    }
    cp.state = JobState::NeedsArchiveRead;
    true
}

/// Resume only after the archive reader is available. Re-enter parse using
/// the retained original; queue replay alone never invokes this transition.
pub fn resume_archive(cp: &mut JobCheckpoint) -> bool {
    if cp.source != JobSource::AdminUpload || cp.state != JobState::NeedsArchiveRead {
        return false;
    }
    cp.state = JobState::Parsing;
    cp.admin_release_mbid = None;
    cp.probe_tally = None;
    true
}

/// Move to `Failed`, unless already terminal (terminal states stick).
pub fn fail(state: JobState) -> JobState {
    if is_terminal(state) {
        state
    } else {
        JobState::Failed
    }
}

/// Terminal states need no further work and free the job slot.
pub fn is_terminal(state: JobState) -> bool {
    matches!(
        state,
        JobState::Done | JobState::Failed | JobState::Rejected
    )
}

/// Parked jobs retain their original but await an explicit admin action or
/// archive capability, not another automatic queue delivery.
pub fn is_parked(state: JobState) -> bool {
    matches!(state, JobState::NeedsReview | JobState::NeedsArchiveRead)
}

/// True when another attempt is allowed (`attempt` is 1-based).
pub fn can_retry(attempt: u32) -> bool {
    (1..MAX_ATTEMPTS).contains(&attempt)
}

/// Build the idempotency key for a job: `ingest:{user}:{sha}:{ref}`.
///
/// - `content_sha256_hex`: sha256 of the canonical NZB bytes (upload) or of
///   the indexer's GUID URL (search); 64 hex chars, case-insensitive on input
///   and normalised to lowercase in the key.
/// - `source_ref`: `"upload"` or the indexer-bound search ref, binding the key
///   to the exact release so a retry of the same content reuses the job row.
///
/// `:` is the field delimiter, so `user_id` and `source_ref` must not contain
/// it (nor NUL): without that check two different
/// `(user, sha, ref)` triples can format to the same key and two unrelated
/// jobs would converge on one library row.
pub fn derive_idempotency_key(
    user_id: &str,
    content_sha256_hex: &str,
    source_ref: &str,
) -> Result<String, &'static str> {
    if user_id.is_empty() || user_id.len() > 128 {
        return Err("user id must be 1..=128 chars");
    }
    if user_id.contains(':') || user_id.contains('\0') {
        return Err("user id must not contain ':'");
    }
    let hex_ok =
        content_sha256_hex.len() == 64 && content_sha256_hex.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex_ok {
        return Err("content sha256 must be 64 hex chars");
    }
    if source_ref.is_empty() || source_ref.len() > 512 {
        return Err("source ref must be 1..=512 chars");
    }
    if source_ref.contains(':') || source_ref.contains('\0') {
        return Err("source ref must not contain ':'");
    }
    Ok(format!(
        "ingest:{user_id}:{}:{source_ref}",
        content_sha256_hex.to_ascii_lowercase()
    ))
}

/// Job ids travel in URLs (`GET /v1/ingest/jobs/:id`): keep them narrow.
pub fn validate_job_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_job_skips_nothing_upload_skips_search() {
        assert_eq!(
            advance(JobState::Queued, JobSource::IndexerSearch, false),
            JobState::Searching
        );
        assert_eq!(
            advance(JobState::Queued, JobSource::NzbUpload, false),
            JobState::Parsing
        );
    }

    #[test]
    fn happy_path_reaches_done() {
        let mut s = advance(JobState::Queued, JobSource::IndexerSearch, false);
        for _ in 0..10 {
            s = advance(s, JobSource::IndexerSearch, false);
        }
        assert_eq!(s, JobState::Done);
        assert!(is_terminal(s));
    }

    #[test]
    fn repair_loops_back_to_verify_then_matches() {
        assert_eq!(
            advance(JobState::Verifying, JobSource::NzbUpload, true),
            JobState::Repairing
        );
        assert_eq!(
            advance(JobState::Repairing, JobSource::NzbUpload, false),
            JobState::Verifying
        );
        assert_eq!(
            advance(JobState::Verifying, JobSource::NzbUpload, false),
            JobState::Matching
        );
    }

    #[test]
    fn matching_advances_through_transcoding_to_writing() {
        assert_eq!(
            advance(JobState::Matching, JobSource::NzbUpload, false),
            JobState::Transcoding
        );
        assert_eq!(
            advance(JobState::Transcoding, JobSource::NzbUpload, false),
            JobState::Writing
        );
    }

    #[test]
    fn fail_sticks_terminal_states() {
        assert_eq!(fail(JobState::Probing), JobState::Failed);
        assert!(is_terminal(JobState::Failed));
        assert_eq!(fail(JobState::Done), JobState::Done);
        assert_eq!(fail(JobState::Failed), JobState::Failed);
    }

    #[test]
    fn retry_budget_is_bounded() {
        assert!(can_retry(1));
        assert!(can_retry(MAX_ATTEMPTS - 1));
        assert!(!can_retry(0));
        assert!(!can_retry(MAX_ATTEMPTS));
        assert!(!can_retry(MAX_ATTEMPTS + 1));
    }

    #[test]
    fn idempotency_key_binds_user_content_and_source() {
        let sha = "a".repeat(64);
        let k1 = derive_idempotency_key("u1", &sha, "upload").unwrap();
        let k2 = derive_idempotency_key("u1", &sha, "upload").unwrap();
        assert_eq!(k1, k2);
        assert!(k1.starts_with("ingest:u1:"));
        assert_ne!(k1, derive_idempotency_key("u2", &sha, "upload").unwrap());
        assert_ne!(k1, derive_idempotency_key("u1", &sha, "guid-9").unwrap());
    }

    #[test]
    fn idempotency_key_rejects_bad_inputs() {
        let sha = "b".repeat(64);
        assert!(derive_idempotency_key("", &sha, "upload").is_err());
        assert!(derive_idempotency_key("u", "xyz", "upload").is_err());
        assert!(derive_idempotency_key("u", &sha, "").is_err());
        assert!(derive_idempotency_key("u", &"A".repeat(64), "upload").is_ok());
    }

    #[test]
    fn idempotency_key_rejects_delimiter_confusion() {
        // Two different triples must never format to the same key: ':' is the
        // field delimiter, so it is rejected in both variable fields.
        let s1 = "c".repeat(64);
        let s2 = "d".repeat(64);
        assert!(derive_idempotency_key("u", &s1, "x/y/S2/r").is_ok());
        assert!(derive_idempotency_key("u", &s1, "x:y").is_err());
        assert!(derive_idempotency_key("u:y:x", &s2, "r").is_err());
        assert!(derive_idempotency_key("u\0", &s1, "upload").is_err());
        assert!(derive_idempotency_key("u", &s1, "up\0load").is_err());
    }

    fn checkpoint_at(state: JobState) -> JobCheckpoint {
        JobCheckpoint {
            job_id: "job_0123456789abcdef".to_string(),
            user_id: "user_01".to_string(),
            idempotency_key: format!("ingest:user_01:{}:upload", "a".repeat(64)),
            source: JobSource::NzbUpload,
            state,
            attempt: 1,
            repair_rounds: 0,
            probe_tally: None,
            admin_release_mbid: None,
            par2_set_id: None,
            missing_blocks: 0,
            failure_code: None,
            failure_reason: None,
            provider_id: None,
            transcode_inputs: Vec::new(),
            transcode_next: 0,
        }
    }

    #[test]
    fn admin_matches_before_probe_and_publishes_without_artifact_stages() {
        let mut cp = checkpoint_at(JobState::Queued);
        cp.source = JobSource::AdminUpload;
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::Parsing);
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::Matching);
        assert!(crate::admin_gates::complete_match(
            &mut cp,
            Some("11111111-1111-4111-8111-111111111111"),
            1,
        ));
        assert_eq!(cp.state, JobState::Probing);
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::Probing);
        cp.probe_tally
            .as_mut()
            .unwrap()
            .missing_by_batch
            .insert(0, 0);
        for expected in [JobState::Writing, JobState::Done, JobState::Done] {
            assert_eq!(advance_checkpoint(&mut cp, false), expected);
            assert_eq!(cp.attempt, 1);
            assert_eq!(cp.repair_rounds, 0);
        }
        assert!(is_terminal(cp.state));
    }

    #[test]
    fn admin_never_dispatches_legacy_stages_even_with_repair_requested() {
        for state in [
            JobState::Searching,
            JobState::Verifying,
            JobState::Repairing,
            JobState::Transcoding,
        ] {
            let mut cp = checkpoint_at(state);
            cp.source = JobSource::AdminUpload;
            assert_eq!(advance_checkpoint(&mut cp, true), JobState::NeedsReview);
            assert_eq!(cp.repair_rounds, 0);
        }
    }

    fn parsed_with_suffix(suffix: &str) -> nntune_core::nzb::Document {
        // Reuse the existing synthetic corpus; add no payload, subject,
        // filename or segment identity literals to this regression.
        let xml = crate::fixtures::SMALL_NZB.replace(".flac", suffix);
        nntune_core::nzb::parse(xml.as_bytes())
            .unwrap_or_else(|_| panic!("synthetic parse rejected"))
    }

    #[test]
    fn parsed_admin_archives_park_before_matching_without_retaining_parser_content() {
        for suffix in [".rar", ".RAR", ".part01.rar", ".r00", ".7z", ".7Z"] {
            let parsed = parsed_with_suffix(suffix);
            let mut cp = checkpoint_at(JobState::Parsing);
            cp.source = JobSource::AdminUpload;
            cp.attempt = MAX_ATTEMPTS;
            let mut expected = cp.clone();
            expected.state = JobState::NeedsArchiveRead;
            assert!(complete_admin_parse(&mut cp, &parsed));
            assert_eq!(cp, expected);
            // Replay never gets another parse/match or any provider work.
            assert!(!complete_admin_parse(&mut cp, &parsed));
            assert_eq!(cp, expected);
        }
    }

    #[test]
    fn parsed_admin_loose_audio_matches_and_mixed_archives_park() {
        for suffix in [".flac", ".FLAC", ".rar.flac", ".flac.par2"] {
            let parsed = parsed_with_suffix(suffix);
            let mut cp = checkpoint_at(JobState::Parsing);
            cp.source = JobSource::AdminUpload;
            let mut expected = cp.clone();
            expected.state = JobState::Matching;
            assert!(complete_admin_parse(&mut cp, &parsed));
            assert_eq!(cp, expected);
        }
        let mut mixed = parsed_with_suffix(".flac");
        mixed.files.extend(parsed_with_suffix(".7z").files);
        let mut cp = checkpoint_at(JobState::Parsing);
        cp.source = JobSource::AdminUpload;
        assert!(complete_admin_parse(&mut cp, &mixed));
        assert_eq!(cp.state, JobState::NeedsArchiveRead);
        assert!(resume_archive(&mut cp));
        // Re-reading an unchanged original must park again: resume is not
        // permission to treat an unread archive as loose audio.
        assert!(complete_admin_parse(&mut cp, &mixed));
        assert_eq!(cp.state, JobState::NeedsArchiveRead);
    }

    #[test]
    fn parsed_admin_completion_refuses_other_sources_and_states() {
        let parsed = parsed_with_suffix(".rar");
        for source in [
            JobSource::IndexerSearch,
            JobSource::NzbUpload,
            JobSource::AdminUpload,
        ] {
            for state in [
                JobState::Queued,
                JobState::Parsing,
                JobState::Matching,
                JobState::Probing,
                JobState::NeedsArchiveRead,
                JobState::NeedsReview,
                JobState::Rejected,
                JobState::Done,
                JobState::Failed,
            ] {
                if source == JobSource::AdminUpload && state == JobState::Parsing {
                    continue;
                }
                let mut cp = checkpoint_at(state);
                cp.source = source;
                let expected = cp.clone();
                assert!(!complete_admin_parse(&mut cp, &parsed));
                assert_eq!(cp, expected);
            }
        }
    }

    #[test]
    fn archive_parks_and_resumes_the_retained_job_without_reupload() {
        let mut cp = checkpoint_at(JobState::Parsing);
        cp.source = JobSource::AdminUpload;
        let original = cp.clone();
        assert!(park_archive(&mut cp));
        assert!(is_parked(cp.state));
        assert!(!is_terminal(cp.state));
        // A persisted/reloaded checkpoint and an ordinary queue replay
        // must leave the job parked, not probe an unread archive.
        let mut cp: JobCheckpoint =
            serde_json::from_str(&serde_json::to_string(&cp).unwrap()).unwrap();
        assert_eq!(
            advance_checkpoint(&mut cp, false),
            JobState::NeedsArchiveRead
        );
        assert!(resume_archive(&mut cp));
        assert_eq!(cp, original);
        // The reader can now parse the same original, then match first.
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::Matching);
    }

    #[test]
    fn archive_actions_refuse_other_sources_and_non_archive_states() {
        let mut legacy = checkpoint_at(JobState::Parsing);
        let before = legacy.clone();
        assert!(!park_archive(&mut legacy));
        assert!(!resume_archive(&mut legacy));
        assert_eq!(legacy, before);
        for state in [JobState::Matching, JobState::Done, JobState::Rejected] {
            let mut cp = checkpoint_at(state);
            cp.source = JobSource::AdminUpload;
            let before = cp.clone();
            assert!(!park_archive(&mut cp));
            assert!(!resume_archive(&mut cp));
            assert_eq!(cp, before);
        }
    }

    #[test]
    fn review_archive_and_rejected_states_do_not_advance_on_replay() {
        for source in [JobSource::NzbUpload, JobSource::AdminUpload] {
            for (state, wire) in [
                (JobState::NeedsReview, "needs_review"),
                (JobState::NeedsArchiveRead, "needs_archive_read"),
                (JobState::Rejected, "rejected"),
            ] {
                assert_eq!(advance(state, source, false), state);
                assert_eq!(advance(state, source, true), state);
                assert_eq!(serde_json::to_value(state).unwrap(), wire);
            }
        }
        assert!(is_terminal(JobState::Rejected));
        assert_eq!(fail(JobState::Rejected), JobState::Rejected);
    }

    #[test]
    fn perpetual_repair_need_terminates_in_failed() {
        let mut cp = checkpoint_at(JobState::Verifying);
        for _ in 0..MAX_REPAIR_ROUNDS {
            assert_eq!(advance_checkpoint(&mut cp, true), JobState::Repairing);
            assert_eq!(advance_checkpoint(&mut cp, false), JobState::Verifying);
        }
        // Budget spent: the next failing verify fails the job, and attempt is
        // untouched (it belongs to Queue redelivery, not to this loop).
        assert_eq!(advance_checkpoint(&mut cp, true), JobState::Failed);
        assert_eq!(cp.attempt, 1);
        assert_eq!(cp.repair_rounds, MAX_REPAIR_ROUNDS);
        assert!(is_terminal(cp.state));
        assert_eq!(cp.failure_code.as_deref(), Some("repair_budget_exhausted"));
        assert_eq!(
            cp.failure_reason.as_deref(),
            Some("repair budget exhausted")
        );
    }

    #[test]
    fn checkpoint_advance_matches_pure_advance_off_the_repair_path() {
        let mut cp = checkpoint_at(JobState::Queued);
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::Parsing);
        assert_eq!(cp.repair_rounds, 0);
        let mut cp = checkpoint_at(JobState::Verifying);
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::Matching);
        assert_eq!(cp.repair_rounds, 0);
    }

    #[test]
    fn checkpoint_carries_the_probe_tally_with_legacy_default() {
        // The tally must survive a crash mid-Probing: it rides the
        // checkpoint, and checkpoints written before the field existed
        // still decode (tally empty, probing resumes from batch zero).
        use crate::probe::ProbeTally;

        let mut cp = checkpoint_at(JobState::Probing);
        assert!(cp.probe_tally.is_none());
        cp.probe_tally = Some(ProbeTally::new(2));
        let back: JobCheckpoint =
            serde_json::from_str(&serde_json::to_string(&cp).unwrap()).unwrap();
        assert_eq!(back, cp);

        let legacy = serde_json::json!({
            "job_id": "job_0123456789abcdef",
            "user_id": "user_01",
            "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
            "source": "nzb_upload",
            "state": "probing",
            "attempt": 1,
            "failure_reason": null,
        });
        let legacy_cp: JobCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(legacy_cp.probe_tally.is_none());
        assert_eq!(legacy_cp.repair_rounds, 0);
    }

    #[test]
    fn checkpoint_carries_provider_attribution_with_legacy_default() {
        // Attribution rides the checkpoint as the live copy (the grant
        // publication reads it from here, not from the immutable Queue
        // body), and checkpoints written before the field existed still
        // decode (unattributed).
        let mut cp = checkpoint_at(JobState::Verifying);
        assert!(cp.provider_id.is_none());
        cp.provider_id = Some("11111111-1111-4111-8111-111111111111".to_string());
        let back: JobCheckpoint =
            serde_json::from_str(&serde_json::to_string(&cp).unwrap()).unwrap();
        assert_eq!(back, cp);
        assert_eq!(
            back.provider_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );

        let legacy = serde_json::json!({
            "job_id": "job_0123456789abcdef",
            "user_id": "user_01",
            "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
            "source": "nzb_upload",
            "state": "verifying",
            "attempt": 1,
            "failure_reason": null,
        });
        let legacy_cp: JobCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(legacy_cp.provider_id.is_none());
    }

    #[test]
    fn checkpoint_transcode_cursor_roundtrips_with_legacy_default() {
        let mut cp = checkpoint_at(JobState::Transcoding);
        cp.transcode_inputs = vec![crate::transcode::TranscodeInput {
            file_index: 0,
            key: "ingest-scratch/job_0123456789abcdef/r1/in/0000-aabb".to_string(),
            sha256: "a".repeat(64),
            size: 512,
            scratch_prefix: "ingest-scratch/job_0123456789abcdef/r1/".to_string(),
        }];
        cp.transcode_next = 1;
        let back: JobCheckpoint =
            serde_json::from_str(&serde_json::to_string(&cp).unwrap()).unwrap();
        assert_eq!(back, cp);
        assert_eq!(back.transcode_inputs.len(), 1);
        assert_eq!(back.transcode_next, 1);

        let legacy = serde_json::json!({
            "job_id": "job_0123456789abcdef",
            "user_id": "user_01",
            "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
            "source": "nzb_upload",
            "state": "transcoding",
            "attempt": 1,
            "failure_reason": null,
        });
        let legacy_cp: JobCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(legacy_cp.transcode_inputs.is_empty());
        assert_eq!(legacy_cp.transcode_next, 0);
    }

    #[test]
    fn job_id_validation_rejects_path_tricks() {
        assert!(validate_job_id("job_01-abc"));
        assert!(!validate_job_id(""));
        assert!(!validate_job_id("../../etc"));
        assert!(!validate_job_id("a/b"));
        assert!(!validate_job_id(&"x".repeat(65)));
    }
}
