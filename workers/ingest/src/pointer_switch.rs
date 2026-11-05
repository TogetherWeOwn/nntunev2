//! B2.6 pointer switch: per-track stream-pointer move to the verified re-post.
//!
//! Pipeline position: after two-spool verification (the verify/re-post
//! service from the TWO-1768 slice) proves the new post, the track's stream
//! pointer moves from the original NZB's envelope to the new post's
//! envelope. Until that moment the original NZB/stream path is untouched;
//! staging copies are deleted only after the switch, and the original
//! retires only after verification — never before.
//!
//! This module is pure and order-agnostic, in the [`crate::track_writers`]
//! slice pattern: it pins the per-track state machine, the real-catalogue
//! gate, the resolver helpers, the statements and the zero-row
//! classification the wire-up uses. Zero execution here — the switch
//! wire-up (a TWO-1768 follow-up) owns the `LIBRARY_DB` calls, the R2
//! staging delete, and the sequencing behind the rows they update.
//!
//! Per-track isolation: an album may be partially switched with no playback
//! regression. The resolver ([`served_pointer`]) serves the original
//! envelope for every state except [`TrackPointerState::Switched`], so a
//! track that is still reposting (or blocked) plays exactly what it played
//! before; the flip to the new envelope is atomic per track at the switch.
//! Rollback ([`PointerEvent::Rollback`]) restores the original pointer and
//! is possible only while the original is not yet retired
//! ([`can_retire_original`]).
//!
//! Envelope boundary (spec §7: KEK never in ingest): the switch binds only
//! sealed envelopes ([`crate::track_writers::TrackSourceParams`], validated
//! by [`crate::track_writers::validate_track_source_params`]) — never
//! plaintext NZB bytes, message-IDs, subjects, file names or segment lists.
//! The `Debug` impl on the switch params reuses the redacting
//! `TrackSourceParams` impl, which prints lengths only, never bytes.
//!
//! Real-catalogue gate (O7): synthetic/staging switching (fixtures, the B7
//! staging run) may proceed with the default gate, but pointer-switching
//! the real catalogue is forbidden until the O7 staging-leg card (TWO-1956:
//! staging-leg restore-drill record + offsite-copy existence, split out of
//! TWO-1772 whose code leg merges separately) completes. The gate is an
//! explicit [`PointerSwitchGate`] value plus the
//! [`REAL_CATALOGUE_SWITCH_ENV`] admin flag — default off, enforced by
//! [`check_gate`], never a comment. [`O7_GATE_CARD`] pins the card the
//! `o7_drill_complete` half keys on.
//!
//! Dashboard surfacing: [`TrackPointerState::as_dashboard_str`] renders the
//! per-track state the health work reads (`original` / `reposting` /
//! `switched` / `repost_blocked`); the literals are pinned below so the
//! retention/health card (TWO-1771) can rely on them.
//!
//! Intended ledger shape (DDL lands in the switch wire-up's migration —
//! this slice pins statements only, the [`crate::library`] precedent for a
//! not-yet-migrated table): one row per track in
//! `track_pointer_switches`, keyed by `track_id` (global, like the
//! post-`0027_track_sources_global` shape): `state` (one of the four
//! dashboard literals), `original_source_job_id` (the pre-switch job, kept
//! for rollback), `switched_source_job_id` (NULL until switched),
//! `original_retired` (FALSE until [`PointerEvent::RetireOriginal`]),
//! `updated_at` (BIGINT epoch seconds). The stream pointer itself stays in
//! `track_sources` (the CAS [`switch_track_source_sql`] below moves it);
//! the ledger keeps the state, the rollback job id and the retire flag.

use crate::dispatch::validate_user_id;
use crate::jobs::validate_job_id;
use crate::queue::validate_mbid;
use crate::track_writers::{validate_track_source_params, TrackSourceParams};
use crate::verify::VerifyProgress;

/// Dashboard literal for [`TrackPointerState::Original`].
pub const STATE_ORIGINAL: &str = "original";
/// Dashboard literal for [`TrackPointerState::Reposting`].
pub const STATE_REPOSTING: &str = "reposting";
/// Dashboard literal for [`TrackPointerState::Switched`].
pub const STATE_SWITCHED: &str = "switched";
/// Dashboard literal for [`TrackPointerState::RepostBlocked`].
pub const STATE_REPOST_BLOCKED: &str = "repost_blocked";

/// Every dashboard-visible per-track pointer state, in a fixed order.
pub const POINTER_STATES: [&str; 4] = [
    STATE_ORIGINAL,
    STATE_REPOSTING,
    STATE_SWITCHED,
    STATE_REPOST_BLOCKED,
];

/// Admin/config flag enabling real-catalogue pointer switching. Read from
/// the process environment by the wire-up (never inside this module, so
/// host tests pin the parsing without env access); unset/anything-else is
/// deny. Stays `false` until the O7 staging-leg card ([`O7_GATE_CARD`])
/// completes AND an operator sets it — see [`PointerSwitchGate`].
pub const REAL_CATALOGUE_SWITCH_ENV: &str = "M2_ALLOW_REAL_CATALOGUE_SWITCH";

/// Paperclip card the O7 real-catalogue gate keys on: the O7 staging leg
/// (staging-leg restore-drill record + offsite-copy existence evidence).
/// The real catalogue must not switch until this card is done; the code
/// leg on TWO-1772 merging does NOT open the gate. The wire-up sets
/// [`PointerSwitchGate::o7_drill_complete`] from this card's completion
/// evidence only.
pub const O7_GATE_CARD: &str = "TWO-1956";

/// Latest bindable `updated_at`: mirrors the ledger epoch convention
/// (`accepted_at <= 253402300799` in migration 0021), so a caller clock
/// fault fails here instead of as a 23514 at the database.
pub const MAX_SWITCH_AT_SECS: i64 = 253402300799;

/// Per-track pointer state. Dashboard-visible via [`Self::as_dashboard_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackPointerState {
    /// Serving the original NZB/stream path. The only state a track is in
    /// before the first [`PointerEvent::BeginRepost`], and the state a
    /// rollback returns to.
    Original,
    /// A re-post is in flight (posting, propagation delay, verification).
    /// The resolver still serves the original envelope.
    Reposting,
    /// Two-spool verification passed and the stream pointer now serves the
    /// new post's envelope. The original may still be retained for
    /// rollback until [`PointerEvent::RetireOriginal`].
    Switched,
    /// The re-post failed beyond the re-post budget. The track stays on
    /// its original NZB; a future repost begins from this state.
    RepostBlocked,
}

impl TrackPointerState {
    /// Dashboard literal for this state (`original` / `reposting` /
    /// `switched` / `repost_blocked`). Also the `state` value the ledger
    /// statements bind — one literal, never two spellings.
    pub const fn as_dashboard_str(self) -> &'static str {
        match self {
            Self::Original => STATE_ORIGINAL,
            Self::Reposting => STATE_REPOSTING,
            Self::Switched => STATE_SWITCHED,
            Self::RepostBlocked => STATE_REPOST_BLOCKED,
        }
    }
}

/// Parse a dashboard/`state` literal. `None` for anything else — the
/// wire-up fails unknown stored states closed instead of guessing.
pub fn parse_pointer_state(raw: &str) -> Option<TrackPointerState> {
    match raw {
        STATE_ORIGINAL => Some(TrackPointerState::Original),
        STATE_REPOSTING => Some(TrackPointerState::Reposting),
        STATE_SWITCHED => Some(TrackPointerState::Switched),
        STATE_REPOST_BLOCKED => Some(TrackPointerState::RepostBlocked),
        _ => None,
    }
}

/// Which catalogue a switch targets. Synthetic covers fixtures and the B7
/// staging run; Real is the user-serving catalogue guarded by O7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetCatalogue {
    Synthetic,
    Real,
}

impl TargetCatalogue {
    /// Parse the wire-up's target marker (`synthetic` / `real`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "synthetic" => Some(Self::Synthetic),
            "real" => Some(Self::Real),
            _ => None,
        }
    }

    /// The marker the ledger statements bind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::Real => "real",
        }
    }
}

/// The O7 real-catalogue gate, keyed on [`O7_GATE_CARD`]. Both halves must
/// hold before the real catalogue may switch: the admin flag (an operator
/// sets [`REAL_CATALOGUE_SWITCH_ENV`] after the drill) AND the drill
/// evidence itself. Default denies everything real.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PointerSwitchGate {
    /// Operator-set allow flag (from [`REAL_CATALOGUE_SWITCH_ENV`]).
    pub allow_real_catalogue_switch: bool,
    /// Whether the O7 staging-leg card ([`O7_GATE_CARD`]: keyring/KEK
    /// backup + proven restore drill) completed with evidence on its card.
    pub o7_drill_complete: bool,
}

impl PointerSwitchGate {
    /// True only when the admin flag is set AND the O7 drill completed.
    pub const fn real_catalogue_allowed(self) -> bool {
        self.allow_real_catalogue_switch && self.o7_drill_complete
    }
}

/// Parse the admin allow flag: `1`/`true`/`yes`/`on` (any case) allow;
/// anything else — including unset/empty — denies. Pure so the truth
/// table is pinned without env access; the wire-up reads
/// [`REAL_CATALOGUE_SWITCH_ENV`] and forwards the raw value here.
pub fn parse_allow_flag(raw: Option<&str>) -> bool {
    match raw.map(str::trim) {
        Some("1") => true,
        Some(word)
            if word.eq_ignore_ascii_case("true")
                || word.eq_ignore_ascii_case("yes")
                || word.eq_ignore_ascii_case("on") =>
        {
            true
        }
        _ => false,
    }
}

/// Why a switch was refused by [`check_gate`]. Reasons are stable strings
/// the wire-up surfaces to the dashboard/audit — never NZB bytes,
/// message-IDs, subjects, file names or segment lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateRefusal {
    pub reason: &'static str,
}

/// Enforce the O7 gate: synthetic targets always pass (staging switching
/// may proceed); real targets pass only when [`PointerSwitchGate`]
/// allows, otherwise fail with the precise half that is missing.
pub fn check_gate(target: TargetCatalogue, gate: PointerSwitchGate) -> Result<(), GateRefusal> {
    match target {
        TargetCatalogue::Synthetic => Ok(()),
        TargetCatalogue::Real => {
            if !gate.o7_drill_complete {
                return Err(GateRefusal {
                    reason: "real-catalogue pointer switching refused: O7 keyring backup + restore drill incomplete",
                });
            }
            if !gate.allow_real_catalogue_switch {
                return Err(GateRefusal {
                    reason: "real-catalogue pointer switching refused: admin allow flag off",
                });
            }
            Ok(())
        }
    }
}

/// Events driving the per-track state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerEvent {
    /// A re-post starts for an [`TrackPointerState::Original`] (or a
    /// retried [`TrackPointerState::RepostBlocked`]) track. Serves the
    /// original until verification.
    BeginRepost,
    /// Two-spool verification passed for the new post: the pointer flips.
    VerifiedTwoSpool,
    /// The re-post budget is exhausted (or verification failed terminally):
    /// the track stays on its original NZB.
    RepostFailed,
    /// Restore the original pointer. Possible only from
    /// [`TrackPointerState::Switched`] while the original is not retired.
    Rollback,
    /// Mark the original retired. Possible only from
    /// [`TrackPointerState::Switched`] with verification evidence; the
    /// dashboard state stays `switched`.
    RetireOriginal,
}

/// Outcome of one [`transition`]: the new state plus whether the original
/// is retired afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub state: TrackPointerState,
    pub original_retired: bool,
}

/// Advance the per-track state machine. `original_retired` is the stored
/// retire flag (a retired original can never come back). Every illegal
/// move fails with a stable reason; transitions never touch storage —
/// the wire-up persists the returned [`Transition`] via
/// [`upsert_pointer_switch_sql`].
pub fn transition(
    from: TrackPointerState,
    event: PointerEvent,
    original_retired: bool,
) -> Result<Transition, &'static str> {
    // A retired original pins the track: no rollback target exists, and no
    // new repost may start from a state whose history is gone. `Switched`
    // stays `Switched` (the pointer already serves the new post);
    // `RetireOriginal` re-applied converges instead of failing.
    if original_retired {
        return match (from, event) {
            (TrackPointerState::Switched, PointerEvent::RetireOriginal) => Ok(Transition {
                state: TrackPointerState::Switched,
                original_retired: true,
            }),
            (TrackPointerState::Switched, _) => Err("original retired: pointer is final"),
            _ => Err("original retired: no transitions out of a retired state"),
        };
    }
    match (from, event) {
        (TrackPointerState::Original, PointerEvent::BeginRepost) => Ok(Transition {
            state: TrackPointerState::Reposting,
            original_retired: false,
        }),
        (TrackPointerState::RepostBlocked, PointerEvent::BeginRepost) => Ok(Transition {
            state: TrackPointerState::Reposting,
            original_retired: false,
        }),
        (TrackPointerState::Reposting, PointerEvent::VerifiedTwoSpool) => Ok(Transition {
            state: TrackPointerState::Switched,
            original_retired: false,
        }),
        (TrackPointerState::Reposting, PointerEvent::RepostFailed) => Ok(Transition {
            state: TrackPointerState::RepostBlocked,
            original_retired: false,
        }),
        (TrackPointerState::Switched, PointerEvent::Rollback) => Ok(Transition {
            state: TrackPointerState::Original,
            original_retired: false,
        }),
        (TrackPointerState::Switched, PointerEvent::RetireOriginal) => Ok(Transition {
            state: TrackPointerState::Switched,
            original_retired: true,
        }),
        (TrackPointerState::Original, PointerEvent::VerifiedTwoSpool) => {
            Err("nothing to switch: no re-post is in flight")
        }
        (TrackPointerState::Original, PointerEvent::RepostFailed) => {
            Err("nothing failed: no re-post is in flight")
        }
        (TrackPointerState::Original, PointerEvent::Rollback) => {
            Err("nothing to roll back: pointer already serves the original")
        }
        (TrackPointerState::Original, PointerEvent::RetireOriginal) => {
            Err("nothing to retire: pointer never switched")
        }
        (TrackPointerState::Reposting, PointerEvent::BeginRepost) => {
            Err("re-post already in flight")
        }
        (TrackPointerState::Reposting, PointerEvent::Rollback) => {
            Err("nothing to roll back: pointer still serves the original")
        }
        (TrackPointerState::Reposting, PointerEvent::RetireOriginal) => {
            Err("original retires only after verification")
        }
        (TrackPointerState::Switched, PointerEvent::BeginRepost) => {
            Err("already switched: retire or roll back first")
        }
        (TrackPointerState::Switched, PointerEvent::VerifiedTwoSpool) => {
            Err("already switched: verification already applied")
        }
        (TrackPointerState::Switched, PointerEvent::RepostFailed) => {
            Err("already switched: no re-post in flight to fail")
        }
        (TrackPointerState::RepostBlocked, PointerEvent::VerifiedTwoSpool) => {
            Err("blocked: begin a new re-post before applying verification")
        }
        (TrackPointerState::RepostBlocked, PointerEvent::RepostFailed) => {
            Err("already blocked: begin a new re-post first")
        }
        (TrackPointerState::RepostBlocked, PointerEvent::Rollback) => {
            Err("nothing to roll back: pointer still serves the original")
        }
        (TrackPointerState::RepostBlocked, PointerEvent::RetireOriginal) => {
            Err("nothing to retire: pointer never switched")
        }
    }
}

/// Whether the original may retire in `state` with two-spool verification
/// evidence. Only [`TrackPointerState::Switched`] + verified evidence —
/// the original NZB/stream path is untouched until the switch, and the
/// retire step never precedes it.
pub const fn can_retire_original(state: TrackPointerState, two_spool_verified: bool) -> bool {
    matches!(state, TrackPointerState::Switched) && two_spool_verified
}

/// Which envelope the stream path serves in `state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedPointer {
    Original,
    Switched,
}

/// Resolve which envelope serves playback. Every state except
/// [`TrackPointerState::Switched`] serves the original — that is the whole
/// no-regression argument for partial album switches: reposting, blocked
/// and never-switched tracks play exactly what they played before, and
/// only a verified track flips, atomically, to the new envelope.
pub const fn served_pointer(state: TrackPointerState) -> ServedPointer {
    match state {
        TrackPointerState::Switched => ServedPointer::Switched,
        TrackPointerState::Original
        | TrackPointerState::Reposting
        | TrackPointerState::RepostBlocked => ServedPointer::Original,
    }
}

/// Whether the job's R2 staging copies may be deleted: only once the track
/// is [`TrackPointerState::Switched`] with two-spool verification
/// evidence. No master copy is kept, so early deletion would destroy the
/// only bytes a rollback could fall back to.
pub const fn staging_cleanup_due(state: TrackPointerState, two_spool_verified: bool) -> bool {
    matches!(state, TrackPointerState::Switched) && two_spool_verified
}

/// R2 staging prefix to delete once [`staging_cleanup_due`] holds: the
/// round's scratch prefix (`ingest-scratch/{job_id}/r{round}/`), covering
/// spooled segments, staged inputs and the manifest. Delegates to
/// [`VerifyProgress::scratch_prefix`] so the delete set can never drift
/// from the verify step's key namespace.
pub fn staging_delete_prefix(job_id: &str, round: u32) -> String {
    VerifyProgress::scratch_prefix(job_id, round)
}

/// One pointer-switch write: the track, the compare-and-swap guard on the
/// current stream pointer, and the new post's sealed envelope.
pub struct SwitchParams<'a> {
    /// Track whose pointer moves (canonical MBID hex; binds `::text::uuid`).
    pub track_id: &'a str,
    /// Source job the pointer is expected to serve now (CAS guard — the
    /// pre-switch job id, or the currently-serving job on a re-switch
    /// attempt). A mismatch means another delivery already switched (or
    /// rolled back) and this write must converge without overwriting.
    pub expected_source_job_id: &'a str,
    /// The new post's sealed envelope. Must pass
    /// [`validate_track_source_params`]: encrypted DB envelopes only.
    pub new: TrackSourceParams<'a>,
    /// Ledger `updated_at` (BIGINT epoch seconds, bound by the caller).
    pub updated_at: i64,
}

// Manual Debug: sealed bytes must never appear in debug output (threat
// model §3: CI logs are attacker-readable). Ids only — the envelope bytes
// stay behind the redacting `TrackSourceParams` impl.
impl std::fmt::Debug for SwitchParams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwitchParams")
            .field("track_id", &self.track_id)
            .field("expected_source_job_id", &self.expected_source_job_id)
            .field("new", &self.new)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// Check [`SwitchParams`] before they touch the database. Bounds mirror
/// the ledger conventions (`track_id` UUID text shape, job-id alphabet,
/// `updated_at` epoch range); the envelope itself reuses
/// [`validate_track_source_params`] unchanged, so the "encrypted DB
/// envelopes only" contract is one shared check, never a copy.
pub fn validate_switch_params(params: &SwitchParams) -> Result<(), &'static str> {
    if !validate_mbid(params.track_id) {
        return Err("track id must be canonical 8-4-4-4-12 hex");
    }
    if !validate_job_id(params.expected_source_job_id) {
        return Err("expected job id must be 1..=64 url-safe chars");
    }
    if !(0..=MAX_SWITCH_AT_SECS).contains(&params.updated_at) {
        return Err("updated_at must be 0..=253402300799 unix seconds");
    }
    validate_track_source_params(&params.new)
        .map_err(|_| "new envelope must be a sealed v2 envelope")?;
    if params.new.track_id != params.track_id {
        return Err("new envelope must address the switched track");
    }
    Ok(())
}

/// `$n` parameter order for [`switch_track_source_sql`]. The envelope
/// version is a SQL literal (`2`), not a parameter.
pub const SWITCH_SOURCE_PARAM_ORDER: [&str; 9] = [
    "track_id",
    "expected_source_job_id",
    "new_source_job_id",
    "nzb_ref_encrypted",
    "nzb_ref_kek_version",
    "nzb_ref_dek_wrapped_nonce",
    "nzb_ref_dek_wrapped",
    "nzb_ref_payload_nonce",
    "nzb_ref_payload_format",
];

/// Move the stream pointer to the new post's envelope: a
/// compare-and-swap `UPDATE` on `track_sources` (global post-`0027` shape:
/// `PRIMARY KEY (track_id)`, no `user_id`).
///
/// `SET` writes the new job id plus the sealed v2 envelope (the version is
/// the `2` literal: only v2 mint output is bindable, never a
/// caller-supplied version). The `WHERE` pins the CAS guard
/// (`source_job_id = $2`): a concurrent switch/rollback that already moved
/// the pointer matches 0 rows instead of overwriting it, and the caller
/// separates that from a missing row with
/// [`select_track_source_envelope_sql`] plus
/// [`classify_switch_zero_rows`].
///
/// Both sides select through the standard `writer_fence` (the owner's
/// `users` row `FOR SHARE` while `deleting_at IS NULL`, ADR 0002 rule 3),
/// so no pointer moves after deletion is scheduled. `track_sources`
/// carries no user identity post-`0027`, so the fence owner is bound but
/// not stored — the [`crate::track_writers`] `tracks` precedent.
pub fn switch_track_source_sql() -> &'static str {
    "UPDATE track_sources SET source_job_id = $3, nzb_ref_encrypted = $4, nzb_ref_envelope_version = 2, nzb_ref_kek_version = $5, nzb_ref_dek_wrapped_nonce = $6, nzb_ref_dek_wrapped = $7, nzb_ref_payload_nonce = $8, nzb_ref_payload_format = $9 FROM (SELECT id FROM users WHERE id = $10 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE track_sources.track_id = $1::text::uuid AND track_sources.source_job_id = $2 AND track_sources.user_id = writer_fence.id"
}

/// Fetch the serving pointer for CAS classification and rollback reads.
/// Parameters: `track_id` ($1).
pub fn select_track_source_envelope_sql() -> &'static str {
    "SELECT track_id, source_job_id FROM track_sources WHERE track_id = $1::text::uuid"
}

/// Outcome of one CAS [`switch_track_source_sql`] write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchWriteOutcome {
    /// The pointer moved to the new envelope while the fence was open.
    Switched,
    /// Zero rows but the pointer already serves the new job: a redelivery
    /// converged on the already-switched pointer.
    Converged,
    /// Zero rows and the pointer serves a different job (a concurrent
    /// switch/rollback won) or no row exists (missing track, or the fence
    /// refused a deleting account). Permanent either way: never retried
    /// as a blind overwrite.
    DivergedOrMissing,
}

/// Classify a zero-row CAS switch write by the currently stored
/// `source_job_id` (or its absence). Pure; the consumer owns the select.
/// `current` is `None` when the track has no `track_sources` row.
pub fn classify_switch_zero_rows(
    current_source_job_id: Option<&str>,
    expected_source_job_id: &str,
    new_source_job_id: &str,
) -> SwitchWriteOutcome {
    match current_source_job_id {
        Some(current) if current == new_source_job_id => SwitchWriteOutcome::Converged,
        Some(current) if current == expected_source_job_id => {
            // The guard still matches but the fenced update wrote nothing:
            // the fence refused (account deleting). Retrying the same CAS
            // can never land it.
            SwitchWriteOutcome::DivergedOrMissing
        }
        _ => SwitchWriteOutcome::DivergedOrMissing,
    }
}

/// `$n` parameter order for [`upsert_pointer_switch_sql`].
pub const POINTER_SWITCH_PARAM_ORDER: [&str; 7] = [
    "track_id",
    "state",
    "target_catalogue",
    "original_source_job_id",
    "switched_source_job_id",
    "original_retired",
    "updated_at",
];

/// Record the state-machine move in the `track_pointer_switches` ledger
/// (one row per track; DDL lands with the wire-up migration). `INSERT ...
/// ON CONFLICT (track_id) DO UPDATE` converges redeliveries onto the
/// latest move; the transition legality itself is enforced by
/// [`transition`], which the wire-up must call before binding — the
/// statement carries values, never the matrix. `switched_source_job_id`
/// binds NULL until the first [`PointerEvent::VerifiedTwoSpool`].
/// `original_retired` binds as a boolean; `updated_at` is the caller clock
/// (bounded by [`validate_switch_ledger_params`]).
///
/// Fenced like every ingest writer: the owner's `users` row `FOR SHARE`
/// while `deleting_at IS NULL` gates the write without becoming a column
/// (the ledger is global per track).
pub fn upsert_pointer_switch_sql() -> &'static str {
    "INSERT INTO track_pointer_switches (track_id, state, target_catalogue, original_source_job_id, switched_source_job_id, original_retired, updated_at) SELECT $1::text::uuid, $2, $3, $4, $5, $6, $7 FROM (SELECT id FROM users WHERE id = $8 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (track_id) DO UPDATE SET state = EXCLUDED.state, target_catalogue = EXCLUDED.target_catalogue, switched_source_job_id = EXCLUDED.switched_source_job_id, original_retired = EXCLUDED.original_retired, updated_at = EXCLUDED.updated_at"
}

/// Fetch the ledger row for rollback/retire guards and dashboard reads.
/// Parameters: `track_id` ($1).
pub fn select_pointer_switch_sql() -> &'static str {
    "SELECT track_id, state, original_source_job_id, switched_source_job_id, original_retired FROM track_pointer_switches WHERE track_id = $1::text::uuid"
}

/// Mark the original retired: only from `state = 'switched'` while not
/// already retired. A guarded `UPDATE` (not a blind flag set): retiring
/// from any other state, or re-retiring, matches 0 rows, and the caller
/// separates "already retired" from "never switched" with
/// [`select_pointer_switch_sql`]. The dashboard state is untouched by this
/// statement — retirement never changes what [`served_pointer`] serves.
///
/// Parameters: `track_id` ($1), `updated_at` ($2), fence owner ($3).
pub fn retire_original_sql() -> &'static str {
    "UPDATE track_pointer_switches SET original_retired = TRUE, updated_at = $2 FROM (SELECT id FROM users WHERE id = $3 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE track_pointer_switches.track_id = $1::text::uuid AND track_pointer_switches.state = 'switched' AND track_pointer_switches.original_retired = FALSE AND writer_fence.id IS NOT NULL"
}

/// One ledger row's bound values, in [`POINTER_SWITCH_PARAM_ORDER`]
/// (`$1`..=`$7`) order, plus the fence owner (`$8`). The edge executor
/// binds exactly these values in field order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerSwitchLedger<'a> {
    pub track_id: &'a str,
    pub state: TrackPointerState,
    pub target: TargetCatalogue,
    pub original_source_job_id: &'a str,
    pub switched_source_job_id: Option<&'a str>,
    pub original_retired: bool,
    pub updated_at: i64,
    /// Fence owner (bound, never stored): the switching job's owner.
    pub owner_id: &'a str,
}

/// Check [`PointerSwitchLedger`] before it touches the database. Bounds
/// mirror the intended DDL (UUID text shape, job-id alphabet, epoch
/// range); the state/target enums are exhaustive by construction, and the
/// cross-field rules pin the machine: a switched job id exists exactly in
/// [`TrackPointerState::Switched`], and a retired original exists only
/// there too (retirement without a switch is a caller bug).
pub fn validate_switch_ledger_params(ledger: &PointerSwitchLedger) -> Result<(), &'static str> {
    if !validate_mbid(ledger.track_id) {
        return Err("track id must be canonical 8-4-4-4-12 hex");
    }
    if !validate_user_id(ledger.owner_id) {
        return Err("owner id must be 1..=128 chars without ':' or NUL");
    }
    if !validate_job_id(ledger.original_source_job_id) {
        return Err("original job id must be 1..=64 url-safe chars");
    }
    if let Some(switched) = ledger.switched_source_job_id {
        if !validate_job_id(switched) {
            return Err("switched job id must be 1..=64 url-safe chars");
        }
    }
    if !(0..=MAX_SWITCH_AT_SECS).contains(&ledger.updated_at) {
        return Err("updated_at must be 0..=253402300799 unix seconds");
    }
    match ledger.state {
        TrackPointerState::Switched => {
            if ledger.switched_source_job_id.is_none() {
                return Err("switched state must carry the switched job id");
            }
        }
        TrackPointerState::Original
        | TrackPointerState::Reposting
        | TrackPointerState::RepostBlocked => {
            if ledger.switched_source_job_id.is_some() {
                return Err("unswitched state must not carry a switched job id");
            }
            if ledger.original_retired {
                return Err("unswitched state must not retire the original");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track_writers::{ENVELOPE_NONCE_BYTES, ENVELOPE_WRAPPED_DEK_BYTES};

    const TRACK: &str = "11111111-1111-4111-8111-111111111111";
    const JOB_ORIG: &str = "job_original_0123456789abcdef";
    const JOB_NEW: &str = "job_repost_0123456789abcdef";

    fn new_params<'a>(
        encrypted: &'a [u8],
        wrap_nonce: &'a [u8],
        wrapped: &'a [u8],
        payload_nonce: &'a [u8],
    ) -> TrackSourceParams<'a> {
        TrackSourceParams {
            track_id: TRACK,
            source_job_id: JOB_NEW,
            indexer_id: None,
            nzb_ref_encrypted: encrypted,
            nzb_ref_kek_version: 1,
            nzb_ref_dek_wrapped_nonce: wrap_nonce,
            nzb_ref_dek_wrapped: wrapped,
            nzb_ref_payload_nonce: payload_nonce,
            nzb_ref_payload_format: 2,
        }
    }

    #[test]
    fn dashboard_states_are_pinned_for_health_surfacing() {
        assert_eq!(
            POINTER_STATES,
            ["original", "reposting", "switched", "repost_blocked"]
        );
        assert_eq!(TrackPointerState::Original.as_dashboard_str(), "original");
        assert_eq!(TrackPointerState::Reposting.as_dashboard_str(), "reposting");
        assert_eq!(TrackPointerState::Switched.as_dashboard_str(), "switched");
        assert_eq!(
            TrackPointerState::RepostBlocked.as_dashboard_str(),
            "repost_blocked"
        );
        for state in POINTER_STATES {
            assert!(parse_pointer_state(state).is_some(), "{state}");
        }
        assert_eq!(parse_pointer_state("Original"), None);
        assert_eq!(parse_pointer_state("switched "), None);
        assert_eq!(parse_pointer_state(""), None);
        assert_eq!(parse_pointer_state("deleted"), None);
    }

    #[test]
    fn happy_path_moves_original_to_switched_then_retires() {
        let t = transition(
            TrackPointerState::Original,
            PointerEvent::BeginRepost,
            false,
        )
        .unwrap();
        assert_eq!(
            t,
            Transition {
                state: TrackPointerState::Reposting,
                original_retired: false
            }
        );
        let t = transition(
            TrackPointerState::Reposting,
            PointerEvent::VerifiedTwoSpool,
            false,
        )
        .unwrap();
        assert_eq!(
            t,
            Transition {
                state: TrackPointerState::Switched,
                original_retired: false
            }
        );
        assert!(can_retire_original(TrackPointerState::Switched, true));
        let t = transition(
            TrackPointerState::Switched,
            PointerEvent::RetireOriginal,
            false,
        )
        .unwrap();
        assert_eq!(
            t,
            Transition {
                state: TrackPointerState::Switched,
                original_retired: true
            }
        );
        // Re-applying the retire converges instead of failing.
        let t = transition(
            TrackPointerState::Switched,
            PointerEvent::RetireOriginal,
            true,
        )
        .unwrap();
        assert!(t.original_retired);
    }

    #[test]
    fn failed_repost_blocks_but_keeps_the_original_and_can_retry() {
        let t = transition(
            TrackPointerState::Reposting,
            PointerEvent::RepostFailed,
            false,
        )
        .unwrap();
        assert_eq!(t.state, TrackPointerState::RepostBlocked);
        assert!(!t.original_retired);
        // The blocked track serves the original (no playback regression).
        assert_eq!(
            served_pointer(TrackPointerState::RepostBlocked),
            ServedPointer::Original
        );
        // A retry begins a new repost from the blocked state.
        let t = transition(
            TrackPointerState::RepostBlocked,
            PointerEvent::BeginRepost,
            false,
        )
        .unwrap();
        assert_eq!(t.state, TrackPointerState::Reposting);
    }

    #[test]
    fn rollback_restores_the_original_only_before_retire() {
        let t = transition(TrackPointerState::Switched, PointerEvent::Rollback, false).unwrap();
        assert_eq!(t.state, TrackPointerState::Original);
        assert!(!t.original_retired);
        // After retire the pointer is final: rollback is refused.
        assert!(transition(TrackPointerState::Switched, PointerEvent::Rollback, true).is_err());
        // Retiring twice converges; nothing else moves once retired.
        assert!(transition(
            TrackPointerState::Switched,
            PointerEvent::RetireOriginal,
            true
        )
        .is_ok());
        assert!(transition(
            TrackPointerState::Switched,
            PointerEvent::VerifiedTwoSpool,
            true
        )
        .is_err());
    }

    #[test]
    fn original_is_untouchable_until_the_switch() {
        // Retire/verify/rollback/fail out of Original never move the
        // pointer: the original NZB/stream path stays live.
        for event in [
            PointerEvent::VerifiedTwoSpool,
            PointerEvent::RepostFailed,
            PointerEvent::Rollback,
            PointerEvent::RetireOriginal,
        ] {
            assert!(
                transition(TrackPointerState::Original, event, false).is_err(),
                "{event:?}"
            );
        }
        assert!(transition(
            TrackPointerState::Reposting,
            PointerEvent::RetireOriginal,
            false
        )
        .is_err());
        assert!(!can_retire_original(TrackPointerState::Original, true));
        assert!(!can_retire_original(TrackPointerState::Reposting, true));
        assert!(!can_retire_original(TrackPointerState::RepostBlocked, true));
        // Verification evidence is required even once switched.
        assert!(!can_retire_original(TrackPointerState::Switched, false));
    }

    #[test]
    fn resolver_serves_the_original_until_the_verified_switch() {
        assert_eq!(
            served_pointer(TrackPointerState::Original),
            ServedPointer::Original
        );
        assert_eq!(
            served_pointer(TrackPointerState::Reposting),
            ServedPointer::Original
        );
        assert_eq!(
            served_pointer(TrackPointerState::RepostBlocked),
            ServedPointer::Original
        );
        assert_eq!(
            served_pointer(TrackPointerState::Switched),
            ServedPointer::Switched
        );
    }

    #[test]
    fn partial_album_switch_has_no_playback_regression() {
        // Track A verified and switched; track B still reposting. The
        // resolver serves the new envelope only for A.
        let served = [
            (TrackPointerState::Switched, ServedPointer::Switched),
            (TrackPointerState::Reposting, ServedPointer::Original),
        ];
        for (state, want) in served {
            assert_eq!(served_pointer(state), want, "{state:?}");
        }
    }

    #[test]
    fn real_catalogue_gate_defaults_off_and_refuses_precisely() {
        // The gate keys on the O7 staging-leg card (TWO-1956), not the O7
        // code leg (TWO-1772): the code leg merging must not open the gate.
        assert_eq!(O7_GATE_CARD, "TWO-1956");
        let gate = PointerSwitchGate::default();
        assert!(!gate.real_catalogue_allowed());
        // Staging/fixture switching proceeds under the default gate.
        assert!(check_gate(TargetCatalogue::Synthetic, gate).is_ok());
        // Real switching refuses while O7 is incomplete — naming the O7
        // half, not the admin flag.
        let err = check_gate(TargetCatalogue::Real, gate).expect_err("default denies real");
        assert!(err.reason.contains("O7"), "{}", err.reason);
        // Admin flag alone does not open the real catalogue.
        let err = check_gate(
            TargetCatalogue::Real,
            PointerSwitchGate {
                allow_real_catalogue_switch: true,
                o7_drill_complete: false,
            },
        )
        .expect_err("O7 incomplete still refuses");
        assert!(err.reason.contains("O7"), "{}", err.reason);
        // Drill alone does not open it either — the operator must opt in.
        let err = check_gate(
            TargetCatalogue::Real,
            PointerSwitchGate {
                allow_real_catalogue_switch: false,
                o7_drill_complete: true,
            },
        )
        .expect_err("flag off still refuses");
        assert!(err.reason.contains("allow flag"), "{}", err.reason);
        // Both halves open the gate.
        assert!(check_gate(
            TargetCatalogue::Real,
            PointerSwitchGate {
                allow_real_catalogue_switch: true,
                o7_drill_complete: true,
            },
        )
        .is_ok());
    }

    #[test]
    fn allow_flag_parsing_is_deny_by_default() {
        assert!(!parse_allow_flag(None));
        assert!(!parse_allow_flag(Some("")));
        assert!(!parse_allow_flag(Some("0")));
        assert!(!parse_allow_flag(Some("false")));
        assert!(!parse_allow_flag(Some("no")));
        assert!(!parse_allow_flag(Some("off")));
        for raw in ["1", "true", "TRUE", "yes", "Yes", "on", "ON", "  true  "] {
            assert!(parse_allow_flag(Some(raw)), "{raw}");
        }
        assert_eq!(REAL_CATALOGUE_SWITCH_ENV, "M2_ALLOW_REAL_CATALOGUE_SWITCH");
    }

    #[test]
    fn target_catalogue_markers_are_pinned() {
        assert_eq!(
            TargetCatalogue::parse("synthetic"),
            Some(TargetCatalogue::Synthetic)
        );
        assert_eq!(TargetCatalogue::parse("real"), Some(TargetCatalogue::Real));
        assert_eq!(TargetCatalogue::parse("Real"), None);
        assert_eq!(TargetCatalogue::parse(""), None);
        assert_eq!(TargetCatalogue::Synthetic.as_str(), "synthetic");
        assert_eq!(TargetCatalogue::Real.as_str(), "real");
    }

    #[test]
    fn switch_params_require_a_sealed_envelope_for_the_track() {
        let encrypted = vec![7u8; 32];
        let wrap_nonce = vec![8u8; ENVELOPE_NONCE_BYTES];
        let wrapped = vec![9u8; ENVELOPE_WRAPPED_DEK_BYTES];
        let payload_nonce = vec![10u8; ENVELOPE_NONCE_BYTES];
        let params = SwitchParams {
            track_id: TRACK,
            expected_source_job_id: JOB_ORIG,
            new: new_params(&encrypted, &wrap_nonce, &wrapped, &payload_nonce),
            updated_at: 1_700_000_000,
        };
        assert!(validate_switch_params(&params).is_ok());
        // Envelope for a different track is refused (never cross-bind).
        let mut other_track = new_params(&encrypted, &wrap_nonce, &wrapped, &payload_nonce);
        other_track.track_id = "22222222-2222-4222-8222-222222222222";
        let params = SwitchParams {
            track_id: TRACK,
            expected_source_job_id: JOB_ORIG,
            new: other_track,
            updated_at: 1_700_000_000,
        };
        assert_eq!(
            validate_switch_params(&params),
            Err("new envelope must address the switched track")
        );
        // Sealed-only: an empty payload is not an envelope.
        let empty: Vec<u8> = Vec::new();
        let sealed = new_params(&encrypted, &wrap_nonce, &wrapped, &payload_nonce);
        let params = SwitchParams {
            track_id: TRACK,
            expected_source_job_id: JOB_ORIG,
            new: TrackSourceParams {
                nzb_ref_encrypted: &empty,
                ..sealed
            },
            updated_at: 1_700_000_000,
        };
        assert!(validate_switch_params(&params).is_err());
        // Bad ids and clock faults fail closed.
        for (track_id, expected) in [("zzz", JOB_ORIG), (TRACK, "bad job!"), ("", JOB_ORIG)] {
            let params = SwitchParams {
                track_id,
                expected_source_job_id: expected,
                new: new_params(&encrypted, &wrap_nonce, &wrapped, &payload_nonce),
                updated_at: 1_700_000_000,
            };
            assert!(
                validate_switch_params(&params).is_err(),
                "{track_id}/{expected}"
            );
        }
        for updated_at in [-1, MAX_SWITCH_AT_SECS + 1] {
            let params = SwitchParams {
                track_id: TRACK,
                expected_source_job_id: JOB_ORIG,
                new: new_params(&encrypted, &wrap_nonce, &wrapped, &payload_nonce),
                updated_at,
            };
            assert!(validate_switch_params(&params).is_err(), "{updated_at}");
        }
    }

    #[test]
    fn switch_debug_redacts_sealed_bytes() {
        let encrypted = vec![0xABu8; 4];
        let wrap_nonce = vec![0xABu8; ENVELOPE_NONCE_BYTES];
        let wrapped = vec![0xABu8; ENVELOPE_WRAPPED_DEK_BYTES];
        let payload_nonce = vec![0xABu8; ENVELOPE_NONCE_BYTES];
        let params = SwitchParams {
            track_id: TRACK,
            expected_source_job_id: JOB_ORIG,
            new: new_params(&encrypted, &wrap_nonce, &wrapped, &payload_nonce),
            updated_at: 1_700_000_000,
        };
        let dumped = format!("{params:?}");
        assert!(dumped.contains(TRACK), "{dumped}");
        assert!(dumped.contains(JOB_ORIG), "{dumped}");
        assert!(!dumped.contains("171"), "sealed byte leaked: {dumped}");
    }

    #[test]
    fn switch_source_statement_is_a_fenced_cas_update() {
        let sql = switch_track_source_sql();
        assert_eq!(SWITCH_SOURCE_PARAM_ORDER.len(), 9);
        // CAS guard: the pointer moves only from the expected job.
        assert!(
            sql.contains(
                "WHERE track_sources.track_id = $1::text::uuid AND track_sources.source_job_id = $2"
            ),
            "{sql}"
        );
        // Only v2 mint output is bindable: the version is a literal.
        assert!(sql.contains("nzb_ref_envelope_version = 2"), "{sql}");
        assert!(!sql.contains("$11"), "{sql}");
        for i in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
            assert!(
                sql.contains(&format!("${i}")),
                "missing placeholder ${i}: {sql}"
            );
        }
        // Fenced on the owner row: dropping the marker check, the lock, or
        // the in-statement join fails here (ADR 0002 rule 3).
        assert!(
            sql.contains("FROM (SELECT id FROM users WHERE id = $10 AND deleting_at IS NULL FOR SHARE) AS writer_fence"),
            "{sql}"
        );
        assert!(
            sql.contains("track_sources.user_id = writer_fence.id"),
            "{sql}"
        );
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        assert!(!sql.contains("NOWAIT"), "{sql}");
        assert!(!sql.contains("SKIP LOCKED"), "{sql}");
        assert_eq!(
            select_track_source_envelope_sql(),
            "SELECT track_id, source_job_id FROM track_sources WHERE track_id = $1::text::uuid"
        );
    }

    #[test]
    fn cas_zero_rows_classify_converged_without_overwriting() {
        assert_eq!(
            classify_switch_zero_rows(Some(JOB_NEW), JOB_ORIG, JOB_NEW),
            SwitchWriteOutcome::Converged
        );
        // Guard still matches: the fence refused (account deleting).
        assert_eq!(
            classify_switch_zero_rows(Some(JOB_ORIG), JOB_ORIG, JOB_NEW),
            SwitchWriteOutcome::DivergedOrMissing
        );
        // A concurrent switch/rollback won: never overwrite it.
        assert_eq!(
            classify_switch_zero_rows(Some("job_other_0123456789abcdef"), JOB_ORIG, JOB_NEW),
            SwitchWriteOutcome::DivergedOrMissing
        );
        // No row at all: missing track or fenced account.
        assert_eq!(
            classify_switch_zero_rows(None, JOB_ORIG, JOB_NEW),
            SwitchWriteOutcome::DivergedOrMissing
        );
    }

    #[test]
    fn ledger_statement_pins_shape_and_cross_field_rules() {
        let sql = upsert_pointer_switch_sql();
        assert_eq!(
            POINTER_SWITCH_PARAM_ORDER,
            [
                "track_id",
                "state",
                "target_catalogue",
                "original_source_job_id",
                "switched_source_job_id",
                "original_retired",
                "updated_at"
            ]
        );
        for i in 1..=8 {
            assert!(
                sql.contains(&format!("${i}")),
                "missing placeholder ${i}: {sql}"
            );
        }
        assert!(sql.contains("ON CONFLICT (track_id) DO UPDATE"), "{sql}");
        assert!(
            sql.contains("FROM (SELECT id FROM users WHERE id = $8 AND deleting_at IS NULL FOR SHARE) AS writer_fence"),
            "{sql}"
        );
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        // A switched row must carry its job; an unswitched row must not,
        // and must never retire the original.
        let switched = PointerSwitchLedger {
            track_id: TRACK,
            state: TrackPointerState::Switched,
            target: TargetCatalogue::Synthetic,
            original_source_job_id: JOB_ORIG,
            switched_source_job_id: Some(JOB_NEW),
            original_retired: false,
            updated_at: 1_700_000_000,
            owner_id: "user_01",
        };
        assert!(validate_switch_ledger_params(&switched).is_ok());
        let retired = PointerSwitchLedger {
            original_retired: true,
            ..switched.clone()
        };
        assert!(validate_switch_ledger_params(&retired).is_ok());
        let missing_job = PointerSwitchLedger {
            switched_source_job_id: None,
            ..switched.clone()
        };
        assert_eq!(
            validate_switch_ledger_params(&missing_job),
            Err("switched state must carry the switched job id")
        );
        for state in [
            TrackPointerState::Original,
            TrackPointerState::Reposting,
            TrackPointerState::RepostBlocked,
        ] {
            let row = PointerSwitchLedger {
                state,
                switched_source_job_id: Some(JOB_NEW),
                ..switched.clone()
            };
            assert_eq!(
                validate_switch_ledger_params(&row),
                Err("unswitched state must not carry a switched job id"),
                "{state:?}"
            );
            let row = PointerSwitchLedger {
                state,
                switched_source_job_id: None,
                original_retired: true,
                ..switched.clone()
            };
            assert_eq!(
                validate_switch_ledger_params(&row),
                Err("unswitched state must not retire the original"),
                "{state:?}"
            );
        }
        assert_eq!(
            validate_switch_ledger_params(&PointerSwitchLedger {
                track_id: "zzz",
                ..switched.clone()
            }),
            Err("track id must be canonical 8-4-4-4-12 hex")
        );
        assert_eq!(
            validate_switch_ledger_params(&PointerSwitchLedger {
                updated_at: MAX_SWITCH_AT_SECS + 1,
                ..switched.clone()
            }),
            Err("updated_at must be 0..=253402300799 unix seconds")
        );
    }

    #[test]
    fn retire_statement_only_fires_from_switched() {
        let sql = retire_original_sql();
        assert!(
            sql.contains("track_pointer_switches.state = 'switched'"),
            "{sql}"
        );
        assert!(
            sql.contains("track_pointer_switches.original_retired = FALSE"),
            "{sql}"
        );
        assert!(sql.contains("SET original_retired = TRUE"), "{sql}");
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        assert_eq!(
            select_pointer_switch_sql(),
            "SELECT track_id, state, original_source_job_id, switched_source_job_id, original_retired FROM track_pointer_switches WHERE track_id = $1::text::uuid"
        );
    }

    #[test]
    fn staging_cleanup_runs_only_after_verified_switch() {
        assert!(!staging_cleanup_due(TrackPointerState::Original, true));
        assert!(!staging_cleanup_due(TrackPointerState::Reposting, true));
        assert!(!staging_cleanup_due(TrackPointerState::RepostBlocked, true));
        assert!(!staging_cleanup_due(TrackPointerState::Switched, false));
        assert!(staging_cleanup_due(TrackPointerState::Switched, true));
        // The delete set is the verify step's own key namespace — the two
        // can never drift apart.
        assert_eq!(
            staging_delete_prefix("job_0123456789abcdef", 0),
            VerifyProgress::scratch_prefix("job_0123456789abcdef", 0)
        );
        assert_eq!(
            staging_delete_prefix("job_0123456789abcdef", 3),
            "ingest-scratch/job_0123456789abcdef/r3/"
        );
    }
}
