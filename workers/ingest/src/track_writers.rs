//! `tracks` + `track_sources` writers: pure statements (migration 0022).
//!
//! Pipeline position: the `Writing` step runs after `Matching` (and after
//! `Transcoding` in the B6 pipeline — `track_media` rows key on `track_id`,
//! which only exists once the match assigns track identity). This module is
//! pure and order-agnostic: it pins the derivation, the statements, the
//! parameter orders, and the zero-row classification the wire-up uses. Zero
//! execution here — the writers-wiring follow-up owns the `LIBRARY_DB`
//! calls, sequencing the writes behind the `tracks` rows they FK to.
//!
//! Derivation ([`derive_track_rows`]) pairs content-role NZB files with the
//! matched release's recordings, positionally: content files (the #658
//! `File::role` contract — only `role == "content"` files are audio) in
//! document order against recordings in album order (the wire-up passes
//! [`crate::matching::parse_mb_recordings_detailed`] output, which yields
//! that order). A row exists only for a backed pair — an extra recording
//! has no bytes and an extra file has no match position, so neither
//! writes. Every column traces to match metadata: `release_id` is the
//! matched release MBID as UUID text, `title`/`medium_no`/`track_no`/`mbid`
//! come from the recording. The title is NEVER a filename fallback: `tracks`
//! rows are cross-user shared, so a per-job filename title would churn (or
//! poison, under first-wins) every user's catalog row. An empty recording
//! title means a stale MB cache and fails that position closed with reason;
//! the library upsert converges, so a replay with fresh match data lands
//! the row. The file side contributes pairing + routing (the row carries
//! the backing file's ordinal for the `nzb_ref` seal), not columns.
//!
//! Unmatched jobs (`release_mbid` is `None`) derive zero rows
//! ([`TrackDerivation::matched`] is false): `release_id` is NOT NULL, so
//! there is no row to write, and therefore zero `track_sources` rows
//! either (they FK to the track). The job itself still lands its
//! `ingest_jobs`/library rows — skipping tracks is a legitimate outcome,
//! never a NULL smuggle and never a job failure.
//!
//! `releases`/`indexers` FK deferral is FINAL for this slice (no new
//! migration): the 0022 header shapes `tracks.release_id` /
//! `track_sources.indexer_id` final but FK-deferred, and the tables
//! themselves are a local-catalog concern (ADR 0002 `artists`/`releases`
//! rows with `local_key`). This slice stores MB-derived UUIDs in those
//! columns; the tables' migrations add the constraints with no backfill.
//! `indexer_id` binds `None` until then (the pipeline's `indexer_ref` is
//! an opaque vault handle, not a UUID — there is no id to store).
//!
//! Envelope boundary (spec §7: KEK never in ingest): this module pins the
//! `nzb_ref_*` companion contract — v2 as a SQL literal, lengths, versions —
//! and validates bound values, but it never mints. Minting reuses the vault
//! envelope code (`workers/api/src/vault.rs::encrypt_vault`, fresh DEK per
//! row: 32-byte DEK + 16-byte tag = the 48-byte `*_dek_wrapped` the schema
//! CHECKs) and lands with the providers-minting sibling, which assigns the
//! `nzb_ref_payload_format` number alongside the sealed payload shape. The
//! wire-up binds this module's [`TrackSourceParams`] from that mint output.
//!
//! Both statements select through the standard `writer_fence` (the owner's
//! `users` row `FOR SHARE` while `deleting_at IS NULL`, ADR 0002 rule 3),
//! so no track row appears after deletion is scheduled. `tracks` carries no
//! user identity, so its fence owner (`$6`) is bound but not inserted — it
//! gates the write without becoming a column. `tracks.id` is likewise not
//! bound: the surrogate is DB-owned (`gen_random_uuid()`), the writer
//! declares the natural key `(release_id, medium_no, track_no)`, and the
//! wire-up re-selects the id ([`select_track_id_by_position_sql`]).
//! `duration_ms` stays NULL — no probe stage measures audio length yet.
//!
//! Both conflict targets are `DO NOTHING` (first writer wins): the rows are
//! cross-user shared (`tracks`) or per-user single-slot (`track_sources`
//! PK `(user_id, track_id)`), MB-derived values are stable so honest
//! replays are identical either way, and a divergent later write is a
//! pipeline bug that must not flap a shared row. A zero-row write is either
//! a benign replay or a fenced account; the consumer separates them with
//! the PK selects plus [`classify_track_zero_rows`], never by assuming.

use nntune_core::nzb::File;

use crate::dispatch::validate_user_id;
use crate::jobs::validate_job_id;
use crate::matching::MbRecording;
use crate::queue::validate_mbid;

/// GCM nonce length (bytes) for both envelope nonces. Mirrors the
/// migration `CHECK`s (`octet_length(...) = 12`).
pub const ENVELOPE_NONCE_BYTES: usize = 12;

/// Wrapped-DEK length (bytes): the fresh 32-byte DEK plus the 16-byte GCM
/// tag `encrypt_vault` seals it with. Mirrors the migration `CHECK`s
/// (`octet_length(...) = 48`).
pub const ENVELOPE_WRAPPED_DEK_BYTES: usize = 48;

/// One derived `tracks` row: inserted columns plus the backing file's
/// ordinal. The ordinal is routing — it tells the wire-up which file's
/// segment manifest to seal into `nzb_ref` — not an inserted column:
/// `tracks` has no file identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRow<'a> {
    /// Matched release MBID as UUID text (canonical 8-4-4-4-12 hex); SQL
    /// casts the text binding to Postgres `uuid`.
    pub release_id: &'a str,
    /// Recording title from the match (trimmed), never a filename.
    pub title: &'a str,
    /// Positions bind as Postgres `INTEGER`, not the match parser's `i16`.
    pub medium_no: i32,
    pub track_no: i32,
    /// Recording MBID as UUID text. NULL-able in the schema, always bound
    /// here: a row derives only from a validated recording.
    pub mbid: &'a str,
    /// Ordinal of the backing content file in the NZB document.
    pub file_ordinal: usize,
}

/// What one [`TrackSkip`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSkipKind {
    /// A recording (index into the caller's slice, album order).
    Recording,
    /// A content file (NZB document ordinal).
    File,
}

/// One position that produced no row. Stable reasons for the wire-up's
/// accounting, pinned below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackSkip {
    pub index: usize,
    pub kind: TrackSkipKind,
    pub reason: &'static str,
}

/// The tracks derivation for one job's match outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDerivation<'a> {
    /// False only when `release_mbid` was `None`: unmatched jobs write
    /// zero `tracks` rows (and therefore zero `track_sources` rows).
    /// `rows` and `skips` are then both empty.
    pub matched: bool,
    pub rows: Vec<TrackRow<'a>>,
    pub skips: Vec<TrackSkip>,
}

/// Derive `tracks` rows for one job: content-role files in document order
/// paired positionally with recordings in album order. `recordings` must
/// already be in album order (see
/// [`crate::matching::parse_mb_recordings_detailed`]); files are sorted by
/// ordinal here so pairing stays deterministic even if a caller passes
/// them out of order. Non-content files (`par2`, `nfo`, `archive`,
/// `unknown` roles) are silently excluded — they are not audio, so their
/// absence from the derivation is not a skip. Only the first valid row
/// for a `(medium_no, track_no)` position wins (matching the SQL conflict
/// target); later duplicates skip their paired file without shifting any
/// subsequent pair.
///
/// `Ok` with `matched: false` is the unmatched outcome (zero rows, zero
/// skips). `Err` is a caller bug only: a `Some` release MBID that is not
/// canonical hex (`pick_release_mbid` only returns validated MBIDs, so the
/// wire-up never hits this on the honest path).
pub fn derive_track_rows<'a>(
    release_mbid: Option<&'a str>,
    files: &'a [File],
    recordings: &'a [MbRecording],
) -> Result<TrackDerivation<'a>, &'static str> {
    let Some(release_mbid) = release_mbid else {
        return Ok(TrackDerivation {
            matched: false,
            rows: Vec::new(),
            skips: Vec::new(),
        });
    };
    if !validate_mbid(release_mbid) {
        return Err("release mbid must be canonical 8-4-4-4-12 hex");
    }
    let mut audio: Vec<&File> = files
        .iter()
        .filter(|file| file.role.as_str() == "content")
        .collect();
    audio.sort_by_key(|file| file.ordinal);
    let mut rows = Vec::new();
    let mut skips = Vec::new();
    let mut seen_positions = std::collections::HashSet::new();
    let paired = audio.len().min(recordings.len());
    for (index, (file, recording)) in audio.iter().zip(recordings.iter()).enumerate() {
        match track_row_for(release_mbid, recording, file.ordinal) {
            Ok(row) if seen_positions.insert((row.medium_no, row.track_no)) => rows.push(row),
            Ok(_) => skips.push(TrackSkip {
                index,
                kind: TrackSkipKind::Recording,
                reason: "duplicate recording medium/track position",
            }),
            Err(reason) => skips.push(TrackSkip {
                index,
                kind: TrackSkipKind::Recording,
                reason,
            }),
        }
    }
    // Unpaired tail: extra recordings have no backing bytes, extra files
    // have no match position — neither writes a row.
    for index in paired..recordings.len() {
        skips.push(TrackSkip {
            index,
            kind: TrackSkipKind::Recording,
            reason: "no content file backs this recording",
        });
    }
    for file in audio.iter().skip(paired) {
        skips.push(TrackSkip {
            index: file.ordinal,
            kind: TrackSkipKind::File,
            reason: "no recording backs this content file",
        });
    }
    Ok(TrackDerivation {
        matched: true,
        rows,
        skips,
    })
}

/// Derive one row for a backed (file, recording) pair. Private: callers go
/// through [`derive_track_rows`] so pairing, unmatched handling and skip
/// accounting stay in one place.
fn track_row_for<'a>(
    release_mbid: &'a str,
    recording: &'a MbRecording,
    file_ordinal: usize,
) -> Result<TrackRow<'a>, &'static str> {
    let (Some(medium_no), Some(track_no)) = (recording.medium_no, recording.track_no) else {
        return Err("recording medium/track position missing");
    };
    let row = TrackRow {
        release_id: release_mbid,
        title: recording.title.trim(),
        medium_no: i32::from(medium_no),
        track_no: i32::from(track_no),
        mbid: recording.mbid.as_str(),
        file_ordinal,
    };
    validate_track_row(&row)?;
    Ok(row)
}

/// Check a [`TrackRow`] before it touches the database. [`derive_track_rows`]
/// output always passes (pinned below); the wire-up re-checks rows it did
/// not derive itself. Bounds mirror migration 0022 (`medium_no`/`track_no`
/// `>= 1`, UUID text shape); `title` is unbounded like the schema.
pub fn validate_track_row(row: &TrackRow) -> Result<(), &'static str> {
    if !validate_mbid(row.release_id) {
        return Err("release mbid must be canonical 8-4-4-4-12 hex");
    }
    if !validate_mbid(row.mbid) {
        return Err("recording mbid must be canonical 8-4-4-4-12 hex");
    }
    if row.medium_no < 1 || row.track_no < 1 {
        return Err("recording medium/track position must be >= 1");
    }
    if row.title.trim().is_empty() {
        return Err("recording title missing from match");
    }
    if row.title.contains('\0') {
        return Err("recording title must not contain NUL");
    }
    Ok(())
}

/// `$n` parameter order for [`upsert_track_sql`]. `user_id` ($6) is the
/// fence owner only — bound, never inserted.
pub const TRACK_INSERT_PARAM_ORDER: [&str; 6] = [
    "mbid",
    "release_id",
    "title",
    "medium_no",
    "track_no",
    "user_id",
];

/// Idempotent `tracks` write for one backed pair. `id` is DB-owned (the
/// `gen_random_uuid()` default) and `duration_ms` stays NULL, so neither
/// is bound. `ON CONFLICT (release_id, medium_no, track_no) DO NOTHING`:
/// the natural key is the convergence point, first writer wins, and every
/// replay converges. A zero-row write is a benign replay or a fenced
/// account; the consumer separates them with
/// [`select_track_id_by_position_sql`] plus [`classify_track_zero_rows`].
/// UUID parameters cast through text so `tokio_postgres` accepts Rust
/// strings; positions bind as `i32` to match the `INTEGER` columns.
pub fn upsert_track_sql() -> &'static str {
    "INSERT INTO tracks (mbid, release_id, title, medium_no, track_no) SELECT $1::text::uuid, $2::text::uuid, $3, $4, $5 FROM (SELECT id FROM users WHERE id = $6 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (release_id, medium_no, track_no) DO NOTHING"
}

/// Fetch the track id after an [`upsert_track_sql`] write, by the natural
/// key the writer declares. Parameters: `release_id` ($1), `medium_no`
/// ($2), `track_no` ($3). Serves both zero-row classification and the
/// downstream FKs (`track_sources`, `track_media`).
pub fn select_track_id_by_position_sql() -> &'static str {
    "SELECT id FROM tracks WHERE release_id = $1::text::uuid AND medium_no = $2 AND track_no = $3"
}

/// One `track_sources` row's bound values, in
/// [`TRACK_SOURCE_PARAM_ORDER`] (`$1`..=`$10`) order. The edge executor
/// binds exactly these values in field order; keeping the projection here
/// lets the order be unit-tested without a database. `indexer_id` is `None`
/// until the `indexers` table lands (see the module docs); the envelope
/// companions come from the providers-minting sibling's `encrypt_vault`
/// reuse, never from a local mint (spec §7: KEK never in ingest).
#[derive(Clone, PartialEq, Eq)]
pub struct TrackSourceParams<'a> {
    pub user_id: &'a str,
    pub track_id: &'a str,
    pub source_job_id: &'a str,
    pub indexer_id: Option<&'a str>,
    pub nzb_ref_encrypted: &'a [u8],
    pub nzb_ref_kek_version: i32,
    pub nzb_ref_dek_wrapped_nonce: &'a [u8],
    pub nzb_ref_dek_wrapped: &'a [u8],
    pub nzb_ref_payload_nonce: &'a [u8],
    pub nzb_ref_payload_format: i16,
}

// Manual Debug: envelope bytes are sealed material and must never appear
// in `{:#?}` output, `dbg!`, tracing fields, or assert messages (threat
// model §3: CI logs are attacker-readable). Lengths only — the vault.rs
// `ProviderConfig` redaction precedent.
impl std::fmt::Debug for TrackSourceParams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackSourceParams")
            .field("user_id", &self.user_id)
            .field("track_id", &self.track_id)
            .field("source_job_id", &self.source_job_id)
            .field("indexer_id", &self.indexer_id)
            .field("nzb_ref_encrypted_len", &self.nzb_ref_encrypted.len())
            .field("nzb_ref_kek_version", &self.nzb_ref_kek_version)
            .field(
                "nzb_ref_dek_wrapped_nonce_len",
                &self.nzb_ref_dek_wrapped_nonce.len(),
            )
            .field("nzb_ref_dek_wrapped_len", &self.nzb_ref_dek_wrapped.len())
            .field(
                "nzb_ref_payload_nonce_len",
                &self.nzb_ref_payload_nonce.len(),
            )
            .field("nzb_ref_payload_format", &self.nzb_ref_payload_format)
            .finish()
    }
}

/// Check [`TrackSourceParams`] before they touch the database. Bounds mirror
/// migration 0022 (UUID text shapes, the 12/48-byte envelope `CHECK`s);
/// `nzb_ref_payload_format` accepts any `i16` — the minting sibling assigns
/// the format number alongside the sealed payload shape, and an invented
/// allowlist here would break that contract.
pub fn validate_track_source_params(params: &TrackSourceParams) -> Result<(), &'static str> {
    if !validate_user_id(params.user_id) {
        return Err("user id must be 1..=128 chars without ':' or NUL");
    }
    if !validate_mbid(params.track_id) {
        return Err("track id must be canonical 8-4-4-4-12 hex");
    }
    if !validate_job_id(params.source_job_id) {
        return Err("job id must be 1..=64 url-safe chars");
    }
    if let Some(indexer_id) = params.indexer_id {
        if !validate_mbid(indexer_id) {
            return Err("indexer id must be canonical 8-4-4-4-12 hex");
        }
    }
    if params.nzb_ref_encrypted.is_empty() {
        return Err("nzb_ref payload must be non-empty");
    }
    if params.nzb_ref_kek_version < 0 {
        return Err("nzb_ref kek version must be >= 0");
    }
    if params.nzb_ref_dek_wrapped_nonce.len() != ENVELOPE_NONCE_BYTES {
        return Err("nzb_ref dek wrap nonce must be 12 bytes");
    }
    if params.nzb_ref_dek_wrapped.len() != ENVELOPE_WRAPPED_DEK_BYTES {
        return Err("nzb_ref wrapped dek must be 48 bytes");
    }
    if params.nzb_ref_payload_nonce.len() != ENVELOPE_NONCE_BYTES {
        return Err("nzb_ref payload nonce must be 12 bytes");
    }
    Ok(())
}

/// `$n` parameter order for [`upsert_track_source_sql`]. The envelope
/// version is a SQL literal (`2`), not a parameter.
pub const TRACK_SOURCE_PARAM_ORDER: [&str; 10] = [
    "user_id",
    "track_id",
    "source_job_id",
    "indexer_id",
    "nzb_ref_encrypted",
    "nzb_ref_kek_version",
    "nzb_ref_dek_wrapped_nonce",
    "nzb_ref_dek_wrapped",
    "nzb_ref_payload_nonce",
    "nzb_ref_payload_format",
];

/// Idempotent `track_sources` write: one provenance slot per
/// `(user_id, track_id)`, job-bound. The envelope version is the `2`
/// literal: only v2 mint output is bindable, never a caller-supplied
/// version. `ON CONFLICT (user_id, track_id) DO NOTHING` — first
/// provenance wins, every replay converges. A zero-row write is a benign
/// replay or a fenced account; the consumer separates them with
/// [`select_track_source_by_pk_sql`] plus [`classify_track_zero_rows`].
/// UUID parameters cast through text for string bindings; the cast keeps
/// a nullable `indexer_id` as NULL.
pub fn upsert_track_source_sql() -> &'static str {
    "INSERT INTO track_sources (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted, nzb_ref_envelope_version, nzb_ref_kek_version, nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped, nzb_ref_payload_nonce, nzb_ref_payload_format) SELECT $1, $2::text::uuid, $3, $4::text::uuid, $5, 2, $6, $7, $8, $9, $10 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, track_id) DO NOTHING"
}

/// Fetch the source row after a zero-row [`upsert_track_source_sql`]
/// write so the consumer can separate a benign replay from a fenced
/// account. Parameters: `user_id` ($1), `track_id` ($2).
pub fn select_track_source_by_pk_sql() -> &'static str {
    "SELECT user_id, track_id FROM track_sources WHERE user_id = $1 AND track_id = $2::text::uuid"
}

/// Outcome of one fenced idempotent track write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackWriteOutcome {
    /// The row landed (inserted while the owner's fence was open).
    Written,
    /// Zero rows but the row exists: a redelivery converged on the
    /// already-written row.
    Duplicate,
    /// Zero rows and no row exists: the owner's fence refused the
    /// write (account deleting or erased). Permanent.
    AccountDeleting,
}

/// Classify a zero-row fenced track write by whether the PK select found
/// the row. Pure; the consumer owns the select. Serves both statements in
/// this module.
pub const fn classify_track_zero_rows(row_exists: bool) -> TrackWriteOutcome {
    if row_exists {
        TrackWriteOutcome::Duplicate
    } else {
        TrackWriteOutcome::AccountDeleting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = "aaaaaaaa-1111-4111-8111-111111111111";
    const REC1: &str = "11111111-1111-4111-8111-111111111111";
    const REC2: &str = "22222222-2222-4222-8222-222222222222";

    fn recording(mbid: &str, title: &str, medium: Option<i16>, track: Option<i16>) -> MbRecording {
        MbRecording {
            mbid: mbid.to_string(),
            title: title.to_string(),
            track_no: track,
            medium_no: medium,
        }
    }

    fn file(ordinal: usize, role: &str) -> File {
        File {
            ordinal,
            subject: format!("subject {ordinal}"),
            filename: Some(format!("{ordinal:02} track.flac")),
            role: role.to_string(),
            container: "flac".to_string(),
            posted_at_unix: 1700000000,
            segments: Vec::new(),
        }
    }

    #[test]
    fn tracks_insert_shape_is_pinned() {
        assert_eq!(
            upsert_track_sql(),
            "INSERT INTO tracks (mbid, release_id, title, medium_no, track_no) \
             SELECT $1::text::uuid, $2::text::uuid, $3, $4, $5 \
             FROM (SELECT id FROM users WHERE id = $6 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
             ON CONFLICT (release_id, medium_no, track_no) DO NOTHING"
        );
        assert_eq!(
            TRACK_INSERT_PARAM_ORDER,
            [
                "mbid",
                "release_id",
                "title",
                "medium_no",
                "track_no",
                "user_id"
            ]
        );
        let sql = upsert_track_sql();
        // The fence binds the owner ($6) even though tracks carries no user
        // identity: the owner gates the write without becoming a column.
        // Dropping the marker check, the lock, or the in-statement join
        // fails here (ADR 0002 rule 3).
        assert!(
            sql.contains(
                "SELECT $1::text::uuid, $2::text::uuid, $3, $4, $5 FROM (SELECT id FROM users WHERE id = $6 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
            ),
            "{sql}"
        );
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        assert!(!sql.contains("NOWAIT"), "{sql}");
        assert!(!sql.contains("SKIP LOCKED"), "{sql}");
        // The surrogate id is DB-owned and duration unmeasured: neither is
        // bound, so neither appears in the column list.
        assert!(
            sql.contains("INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)"),
            "{sql}"
        );
        assert!(!sql.contains("duration_ms"), "{sql}");
        // The conflict target is the natural key, not the surrogate id: a
        // replay presents a fresh default id, so only the natural key
        // converges it.
        assert!(
            sql.contains("ON CONFLICT (release_id, medium_no, track_no) DO NOTHING"),
            "{sql}"
        );
        assert!(!sql.contains("ON CONFLICT (id)"), "{sql}");
    }

    #[test]
    fn track_source_insert_shape_is_pinned() {
        assert_eq!(
            upsert_track_source_sql(),
            "INSERT INTO track_sources (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted, nzb_ref_envelope_version, nzb_ref_kek_version, nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped, nzb_ref_payload_nonce, nzb_ref_payload_format) \
             SELECT $1, $2::text::uuid, $3, $4::text::uuid, $5, 2, $6, $7, $8, $9, $10 \
             FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
             ON CONFLICT (user_id, track_id) DO NOTHING"
        );
        assert_eq!(
            TRACK_SOURCE_PARAM_ORDER,
            [
                "user_id",
                "track_id",
                "source_job_id",
                "indexer_id",
                "nzb_ref_encrypted",
                "nzb_ref_kek_version",
                "nzb_ref_dek_wrapped_nonce",
                "nzb_ref_dek_wrapped",
                "nzb_ref_payload_nonce",
                "nzb_ref_payload_format",
            ]
        );
        let sql = upsert_track_source_sql();
        assert!(
            sql.contains(
                "SELECT $1, $2::text::uuid, $3, $4::text::uuid, $5, 2, $6, $7, $8, $9, $10 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
            ),
            "{sql}"
        );
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        assert!(!sql.contains("NOWAIT"), "{sql}");
        assert!(!sql.contains("SKIP LOCKED"), "{sql}");
        // The envelope version is a literal, never a caller value: only v2
        // mint output is bindable.
        assert!(sql.contains("nzb_ref_envelope_version"), "{sql}");
        assert!(sql.contains(", $5, 2, $6,"), "{sql}");
        // One provenance slot per (user, track): first job wins, replays
        // converge.
        assert!(
            sql.contains("ON CONFLICT (user_id, track_id) DO NOTHING"),
            "{sql}"
        );
    }

    #[test]
    fn pk_selects_are_pinned() {
        assert_eq!(
            select_track_id_by_position_sql(),
            "SELECT id FROM tracks WHERE release_id = $1::text::uuid AND medium_no = $2 AND track_no = $3"
        );
        assert_eq!(
            select_track_source_by_pk_sql(),
            "SELECT user_id, track_id FROM track_sources WHERE user_id = $1 AND track_id = $2::text::uuid"
        );
    }

    #[test]
    fn zero_rows_classify_by_row_existence() {
        assert_eq!(
            classify_track_zero_rows(false),
            TrackWriteOutcome::AccountDeleting
        );
        assert_eq!(classify_track_zero_rows(true), TrackWriteOutcome::Duplicate);
    }

    #[test]
    fn derivation_pairs_content_files_with_recordings() {
        let files = vec![file(0, "content"), file(1, "content")];
        let recordings = vec![
            recording(REC1, "First", Some(1), Some(1)),
            recording(REC2, "Second", Some(1), Some(2)),
        ];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert!(derived.matched);
        assert!(derived.skips.is_empty());
        assert_eq!(
            derived.rows,
            vec![
                TrackRow {
                    release_id: RELEASE,
                    title: "First",
                    medium_no: 1,
                    track_no: 1,
                    mbid: REC1,
                    file_ordinal: 0,
                },
                TrackRow {
                    release_id: RELEASE,
                    title: "Second",
                    medium_no: 1,
                    track_no: 2,
                    mbid: REC2,
                    file_ordinal: 1,
                },
            ]
        );
        // Derivation output always validates: the wire-up can bind it.
        for row in &derived.rows {
            assert!(validate_track_row(row).is_ok());
        }
    }

    #[test]
    fn derived_positions_bind_as_postgres_integers() {
        use tokio_postgres::types::{ToSql, Type};

        let files = [file(0, "content")];
        let recordings = [recording(REC1, "First", Some(1), Some(i16::MAX))];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        let row = &derived.rows[0];
        // INTEGER columns infer INT4 parameters; binding an i16 must not
        // defer a WrongType failure to the future execution wiring.
        for (position, expected) in [
            (&row.medium_no as &(dyn ToSql + Sync), 1_i32),
            (&row.track_no as &(dyn ToSql + Sync), 32767_i32),
        ] {
            let mut encoded = Default::default();
            position.to_sql_checked(&Type::INT4, &mut encoded).unwrap();
            assert_eq!(&encoded[..], expected.to_be_bytes().as_slice());
        }
    }

    #[test]
    fn derivation_skips_non_content_files_silently() {
        // par2/nfo/archive/unknown files are not audio: excluded without a
        // skip entry, so every NZB's support files do not spam accounting.
        let files = vec![
            file(0, "par2"),
            file(1, "content"),
            file(2, "nfo"),
            file(3, "archive"),
            file(4, "unknown"),
        ];
        let recordings = vec![recording(REC1, "First", Some(1), Some(1))];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert!(derived.matched);
        assert!(derived.skips.is_empty());
        assert_eq!(derived.rows.len(), 1);
        assert_eq!(derived.rows[0].file_ordinal, 1);
        assert_eq!(derived.rows[0].mbid, REC1);
    }

    #[test]
    fn derivation_pairs_by_ordinal_not_slice_order() {
        let files = vec![file(5, "content"), file(1, "content")];
        let recordings = vec![
            recording(REC1, "First", Some(1), Some(1)),
            recording(REC2, "Second", Some(1), Some(2)),
        ];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert_eq!(derived.rows.len(), 2);
        assert_eq!(derived.rows[0].file_ordinal, 1);
        assert_eq!(derived.rows[0].mbid, REC1);
        assert_eq!(derived.rows[1].file_ordinal, 5);
        assert_eq!(derived.rows[1].mbid, REC2);
    }

    #[test]
    fn detailed_occurrences_keep_their_backing_files() {
        let body = format!(
            r#"{{"recordings":[
                {{"mbid":"{REC2}","title":"Third","medium_no":1,"track_no":3}},
                {{"mbid":"{REC1}","title":"Reprise","medium_no":1,"track_no":2}},
                {{"mbid":"{REC1}","title":"Opening","medium_no":1,"track_no":1}}
            ]}}"#
        );
        let recordings = crate::matching::parse_mb_recordings_detailed(200, &body).unwrap();
        let files = [file(0, "content"), file(1, "content"), file(2, "content")];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert!(derived.skips.is_empty());
        assert_eq!(
            derived
                .rows
                .iter()
                .map(|row| (row.mbid, row.track_no, row.file_ordinal))
                .collect::<Vec<_>>(),
            vec![(REC1, 1, 0), (REC1, 2, 1), (REC2, 3, 2)]
        );
    }

    #[test]
    fn derivation_skips_duplicate_positions_without_shifting_pairs() {
        let files: Vec<_> = (0..5).map(|ordinal| file(ordinal, "content")).collect();
        let recordings = [
            recording(REC1, "First", Some(1), Some(1)),
            recording(REC2, "Conflicting recording", Some(1), Some(1)),
            recording(REC1, "Repeated occurrence", Some(1), Some(1)),
            recording(REC2, "Next", Some(1), Some(2)),
            recording(REC1, "Disc two", Some(2), Some(1)),
        ];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert_eq!(
            derived
                .rows
                .iter()
                .map(|row| (row.mbid, row.medium_no, row.track_no, row.file_ordinal))
                .collect::<Vec<_>>(),
            vec![(REC1, 1, 1, 0), (REC2, 1, 2, 3), (REC1, 2, 1, 4)]
        );
        assert_eq!(
            derived.skips,
            vec![
                TrackSkip {
                    index: 1,
                    kind: TrackSkipKind::Recording,
                    reason: "duplicate recording medium/track position",
                },
                TrackSkip {
                    index: 2,
                    kind: TrackSkipKind::Recording,
                    reason: "duplicate recording medium/track position",
                },
            ]
        );
    }

    #[test]
    fn invalid_row_does_not_claim_a_position() {
        let files = [file(0, "content"), file(1, "content")];
        let recordings = [
            recording(REC1, "", Some(1), Some(1)),
            recording(REC2, "Valid", Some(1), Some(1)),
        ];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert_eq!(derived.rows.len(), 1);
        assert_eq!(derived.rows[0].mbid, REC2);
        assert_eq!(derived.rows[0].file_ordinal, 1);
        assert_eq!(
            derived.skips,
            vec![TrackSkip {
                index: 0,
                kind: TrackSkipKind::Recording,
                reason: "recording title missing from match",
            }]
        );
    }

    #[test]
    fn derivation_reports_unpaired_tails_with_reason() {
        // More recordings than files: the extra recording has no bytes.
        let files = vec![file(0, "content")];
        let recordings = vec![
            recording(REC1, "First", Some(1), Some(1)),
            recording(REC2, "Second", Some(1), Some(2)),
        ];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert_eq!(derived.rows.len(), 1);
        assert_eq!(
            derived.skips,
            vec![TrackSkip {
                index: 1,
                kind: TrackSkipKind::Recording,
                reason: "no content file backs this recording",
            }]
        );
        // More files than recordings: the extra file has no position.
        let files = vec![file(0, "content"), file(7, "content")];
        let recordings = vec![recording(REC1, "First", Some(1), Some(1))];
        let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
        assert_eq!(derived.rows.len(), 1);
        assert_eq!(
            derived.skips,
            vec![TrackSkip {
                index: 7,
                kind: TrackSkipKind::File,
                reason: "no recording backs this content file",
            }]
        );
    }

    #[test]
    fn derivation_fails_bad_positions_titles_and_mbids_closed() {
        let files = vec![file(0, "content")];
        for (recording, reason) in [
            (
                recording("not-an-mbid", "First", Some(1), Some(1)),
                "recording mbid must be canonical 8-4-4-4-12 hex",
            ),
            (
                recording(REC1, "First", None, Some(1)),
                "recording medium/track position missing",
            ),
            (
                recording(REC1, "First", Some(1), None),
                "recording medium/track position missing",
            ),
            (
                recording(REC1, "First", Some(0), Some(1)),
                "recording medium/track position must be >= 1",
            ),
            (
                recording(REC1, "First", Some(1), Some(0)),
                "recording medium/track position must be >= 1",
            ),
            // An empty title is a stale MB cache, never a filename fill:
            // storing the NZB filename in the shared row would poison
            // every user's catalog copy. Skip; replay converges.
            (
                recording(REC1, "", Some(1), Some(1)),
                "recording title missing from match",
            ),
            (
                recording(REC1, "   ", Some(1), Some(1)),
                "recording title missing from match",
            ),
            (
                recording(REC1, "First\0track", Some(1), Some(1)),
                "recording title must not contain NUL",
            ),
        ] {
            let recordings = [recording];
            let derived = derive_track_rows(Some(RELEASE), &files, &recordings).unwrap();
            assert!(derived.matched);
            assert!(derived.rows.is_empty(), "{reason}");
            assert_eq!(
                derived.skips,
                vec![TrackSkip {
                    index: 0,
                    kind: TrackSkipKind::Recording,
                    reason,
                }],
                "{reason}"
            );
        }
    }

    #[test]
    fn unmatched_jobs_derive_zero_rows() {
        let files = vec![file(0, "content")];
        let recordings = vec![recording(REC1, "First", Some(1), Some(1))];
        let derived = derive_track_rows(None, &files, &recordings).unwrap();
        assert!(!derived.matched);
        assert!(derived.rows.is_empty());
        assert!(derived.skips.is_empty());
    }

    #[test]
    fn malformed_release_mbid_is_a_caller_bug() {
        let files = vec![file(0, "content")];
        let recordings = vec![recording(REC1, "First", Some(1), Some(1))];
        assert_eq!(
            derive_track_rows(Some("not-an-mbid"), &files, &recordings),
            Err("release mbid must be canonical 8-4-4-4-12 hex")
        );
    }

    #[test]
    fn validate_track_row_rejects_each_bad_field() {
        let good = TrackRow {
            release_id: RELEASE,
            title: "First",
            medium_no: 1,
            track_no: 2,
            mbid: REC1,
            file_ordinal: 0,
        };
        assert!(validate_track_row(&good).is_ok());
        for (row, reason) in [
            (
                TrackRow {
                    release_id: "zzz",
                    ..good.clone()
                },
                "release mbid must be canonical 8-4-4-4-12 hex",
            ),
            (
                TrackRow {
                    mbid: "zzz",
                    ..good.clone()
                },
                "recording mbid must be canonical 8-4-4-4-12 hex",
            ),
            (
                TrackRow {
                    medium_no: 0,
                    ..good.clone()
                },
                "recording medium/track position must be >= 1",
            ),
            (
                TrackRow {
                    track_no: -1,
                    ..good.clone()
                },
                "recording medium/track position must be >= 1",
            ),
            (
                TrackRow {
                    title: "",
                    ..good.clone()
                },
                "recording title missing from match",
            ),
            (
                TrackRow {
                    title: "  ",
                    ..good.clone()
                },
                "recording title missing from match",
            ),
            (
                TrackRow {
                    title: "First\0track",
                    ..good.clone()
                },
                "recording title must not contain NUL",
            ),
        ] {
            assert_eq!(validate_track_row(&row), Err(reason), "{reason}");
        }
    }

    // No constructor helper: ten fields would trip clippy's
    // too_many_arguments, so tests build one `base` literal and mutate it
    // per case with struct-update syntax.

    #[test]
    fn track_source_params_follow_the_declared_order() {
        let encrypted = [3u8, 4];
        let wrap_nonce = [0u8; ENVELOPE_NONCE_BYTES];
        let wrapped = [0u8; ENVELOPE_WRAPPED_DEK_BYTES];
        let payload_nonce = [1u8; ENVELOPE_NONCE_BYTES];
        let params = TrackSourceParams {
            user_id: "user_01",
            track_id: REC1,
            source_job_id: "job_0123456789abcdef",
            indexer_id: None,
            nzb_ref_encrypted: &encrypted,
            nzb_ref_kek_version: 1,
            nzb_ref_dek_wrapped_nonce: &wrap_nonce,
            nzb_ref_dek_wrapped: &wrapped,
            nzb_ref_payload_nonce: &payload_nonce,
            nzb_ref_payload_format: 1,
        };
        // Field order is the `$n` binding order in TRACK_SOURCE_PARAM_ORDER.
        assert_eq!(params.user_id, "user_01");
        assert_eq!(params.track_id, REC1);
        assert_eq!(params.source_job_id, "job_0123456789abcdef");
        assert_eq!(params.indexer_id, None);
        assert_eq!(params.nzb_ref_encrypted, encrypted.as_slice());
        assert_eq!(params.nzb_ref_kek_version, 1);
        assert_eq!(params.nzb_ref_dek_wrapped_nonce, wrap_nonce.as_slice());
        assert_eq!(params.nzb_ref_dek_wrapped, wrapped.as_slice());
        assert_eq!(params.nzb_ref_payload_nonce, payload_nonce.as_slice());
        assert_eq!(params.nzb_ref_payload_format, 1);
        assert_eq!(TRACK_SOURCE_PARAM_ORDER.len(), 10);
        assert!(validate_track_source_params(&params).is_ok());
        // A UUID indexer binds once the indexers table lands; today the
        // wire-up passes None.
        let indexed = TrackSourceParams {
            indexer_id: Some(REC2),
            ..params.clone()
        };
        assert!(validate_track_source_params(&indexed).is_ok());
    }

    #[test]
    fn validate_track_source_params_rejects_each_bad_field() {
        let encrypted = [3u8, 4];
        let empty: [u8; 0] = [];
        let wrap_nonce = [0u8; ENVELOPE_NONCE_BYTES];
        let wrapped = [0u8; ENVELOPE_WRAPPED_DEK_BYTES];
        let payload_nonce = [1u8; ENVELOPE_NONCE_BYTES];
        let base = TrackSourceParams {
            user_id: "user_01",
            track_id: REC1,
            source_job_id: "job_0123456789abcdef",
            indexer_id: None,
            nzb_ref_encrypted: &encrypted,
            nzb_ref_kek_version: 1,
            nzb_ref_dek_wrapped_nonce: &wrap_nonce,
            nzb_ref_dek_wrapped: &wrapped,
            nzb_ref_payload_nonce: &payload_nonce,
            nzb_ref_payload_format: 1,
        };
        assert!(validate_track_source_params(&base).is_ok());
        // Each case mutates one field; the message names the bad field.
        let long_user = "u".repeat(129);
        for user in ["", long_user.as_str(), "has:colon"] {
            let params = TrackSourceParams {
                user_id: user,
                ..base.clone()
            };
            assert_eq!(
                validate_track_source_params(&params),
                Err("user id must be 1..=128 chars without ':' or NUL"),
                "{user:?}"
            );
        }
        for (params, reason) in [
            (
                TrackSourceParams {
                    track_id: "zzz",
                    ..base.clone()
                },
                "track id must be canonical 8-4-4-4-12 hex",
            ),
            (
                TrackSourceParams {
                    source_job_id: "bad job!",
                    ..base.clone()
                },
                "job id must be 1..=64 url-safe chars",
            ),
            (
                TrackSourceParams {
                    indexer_id: Some("zzz"),
                    ..base.clone()
                },
                "indexer id must be canonical 8-4-4-4-12 hex",
            ),
            (
                TrackSourceParams {
                    nzb_ref_encrypted: &empty,
                    ..base.clone()
                },
                "nzb_ref payload must be non-empty",
            ),
            (
                TrackSourceParams {
                    nzb_ref_kek_version: -1,
                    ..base.clone()
                },
                "nzb_ref kek version must be >= 0",
            ),
        ] {
            assert_eq!(
                validate_track_source_params(&params),
                Err(reason),
                "{reason}"
            );
        }
        for len in [0, 11, 13] {
            let short = vec![0u8; len];
            let params = TrackSourceParams {
                nzb_ref_dek_wrapped_nonce: &short,
                ..base.clone()
            };
            assert_eq!(
                validate_track_source_params(&params),
                Err("nzb_ref dek wrap nonce must be 12 bytes"),
                "wrap nonce len {len}"
            );
            let params = TrackSourceParams {
                nzb_ref_payload_nonce: &short,
                ..base.clone()
            };
            assert_eq!(
                validate_track_source_params(&params),
                Err("nzb_ref payload nonce must be 12 bytes"),
                "payload nonce len {len}"
            );
        }
        for len in [0, 47, 49] {
            let bad = vec![0u8; len];
            let params = TrackSourceParams {
                nzb_ref_dek_wrapped: &bad,
                ..base.clone()
            };
            assert_eq!(
                validate_track_source_params(&params),
                Err("nzb_ref wrapped dek must be 48 bytes"),
                "wrapped dek len {len}"
            );
        }
        // The payload format is the minting sibling's number: any i16 binds.
        for format in [i16::MIN, 0, 1, i16::MAX] {
            let params = TrackSourceParams {
                nzb_ref_payload_format: format,
                ..base.clone()
            };
            assert!(
                validate_track_source_params(&params).is_ok(),
                "format {format}"
            );
        }
    }

    #[test]
    fn debug_redacts_envelope_bytes() {
        // Threat-model §3: sealed bytes must never appear in debug output
        // (CI logs are attacker-readable). Ids print; bytes show lengths.
        let encrypted = [0xABu8; 4];
        let wrap_nonce = [0xABu8; ENVELOPE_NONCE_BYTES];
        let wrapped = [0xABu8; ENVELOPE_WRAPPED_DEK_BYTES];
        let payload_nonce = [0xABu8; ENVELOPE_NONCE_BYTES];
        let params = TrackSourceParams {
            user_id: "user_01",
            track_id: REC1,
            source_job_id: "job_0123456789abcdef",
            indexer_id: None,
            nzb_ref_encrypted: &encrypted,
            nzb_ref_kek_version: 1,
            nzb_ref_dek_wrapped_nonce: &wrap_nonce,
            nzb_ref_dek_wrapped: &wrapped,
            nzb_ref_payload_nonce: &payload_nonce,
            nzb_ref_payload_format: 1,
        };
        let dumped = format!("{params:?}");
        assert!(dumped.contains("user_01"), "{dumped}");
        assert!(dumped.contains(REC1), "{dumped}");
        assert!(!dumped.contains("171"), "sealed byte leaked: {dumped}");
        assert!(dumped.contains("nzb_ref_encrypted_len"), "{dumped}");
    }
}
