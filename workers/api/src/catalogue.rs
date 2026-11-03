//! Catalogue slice 1: rich Artist/Release/Track reads over the MusicBrainz
//! mirror plus the per-user ingest library ledger.
//!
//! Native core for the `/v1/library`, `/v1/tracks`, `/v1/releases`,
//! `/v1/artists` collections, the `/v1/releases/{mbid}` detail read, and
//! the legacy `/api/library` alias (which serves the same entries as
//! `/v1/library`; the M1 ingest→play→meter journey owns that path's
//! contract in `e2e/helpers/contract.ts`, so this slice is additive and
//! never reshapes it). Pure row assembly plus the parameterized statements
//! the Worker entry binds against PlanetScale Postgres via Hyperdrive —
//! never string-interpolated SQL.
//!
//! Sources (ADR 0002 rule 6: the mirror is the ONLY authority for shared
//! metadata):
//! - Per-user rows come from the ingest `library` ledger (migration 0026):
//!   one row per ingest job carrying the matched release MBID plus the
//!   release's recording MBIDs in album order. An unmatched ingest writes
//!   the empty array by design (`recording_mbids_or_fallback` in
//!   workers/ingest/src/matching.rs).
//! - Rich metadata (titles, durations, positions, artist) is NOT duplicated
//!   in the ledger: it stays authoritative in the MusicBrainz mirror
//!   (`mb_artist`, `mb_release`, `mb_recording`, `mb_track` from migrations
//!   0001/0016) and the reads below join to it. A stored MBID that is
//!   malformed, uppercase, or absent from the mirror degrades that row's
//!   enrichment to NULL — never the listing, never an error, and never a
//!   fill from tenant file tags. The `CASE ... THEN ...::uuid END` guards
//!   force this: a bare `text::uuid` cast on a poisoned value would abort
//!   the whole listing with a 500, and the `CASE` evaluates its guard
//!   before the cast (an `AND`-chained guard would not — the planner may
//!   evaluate the cheap cast first).
//! - Cover art comes from our own origin only: `art_url` names the reserved
//!   `/v1/releases/{mbid}/art` path and is always `Some` when the release
//!   MBID is known. The art-serving slice has not landed, so the path 404s
//!   today (uniform posture, like every unbuilt surface); clients adopt the
//!   field without reshaping later. `mb_release.caa_url` is NEVER selected
//!   or serialized — caching Cover Archive art in a paid product is a
//!   licence risk (W4 checklist), so the mirror's third-party URL stays in
//!   the database.
//!
//! Rules (the HTTP layer maps store/auth failures; this module never sees
//! them):
//! - Authentication first: the dispatch owns session-or-bearer identity and
//!   answers 401 before any store read. Owner ids never leave SQL: every
//!   statement filters `WHERE ... user_id = $1`, and no owner column is
//!   selected.
//! - Lists are bounded first pages, not cursors: slice 1 has no query
//!   transport (`handlers::Request` carries no query field), so every list
//!   statement ends in `LIMIT $N` bound to [`CATALOGUE_MAX_PAGE_SIZE`]
//!   (collections) or [`CATALOGUE_RELEASE_TRACKS_MAX`] (release detail,
//!   mirroring the mirror worker's own recordings cap). Cursor paging needs
//!   the `Request` query-field churn and is an explicit slice-2 follow-up,
//!   as are `/v1/search` (W14) and per-track/per-artist item reads (each
//!   needs its own ADR 0010 scope-map entry first).
//! - Library order is matched-first, then `job_id DESC`, then album
//!   position: playable content surfaces first, which is also what the M1
//!   `recording_mbids` poll reads at `items[0]`. The order is stable for
//!   fixed data, NOT recency — `job_id`s are pipeline ids, not timestamps.
//! - Library entries are per recording occurrence (one entry per array
//!   element in album order), because the web library renders one row per
//!   track and M1 streams `items[0].id`. `id` is the recording MBID for
//!   matched rows and the `job_id` for unmatched rows (both non-empty
//!   strings, satisfying the M1 item filter); `recording_mbids` mirrors the
//!   job row's FULL array (empty by design when unmatched), and
//!   `release_mbid` mirrors the job row's release. Stream-id resolution
//!   against these ids lands on a later play-path card.
//! - Unknown releases read 404: the detail handler runs the release read
//!   first (which doubles as the existence probe) and only then the tracks
//!   read. A malformed `:mbid` never reaches SQL — the handler answers 404
//!   after [`is_catalogue_mbid`] rejects it, so malformed and unknown ids
//!   are indistinguishable (no oracle).

use serde::Serialize;

/// Maximum rows one catalogue collection page returns. The Worker MUST bind
/// this as the list `LIMIT` (`$1`/`$2`); a single response can never grow
/// with the total mirror. Slice 1 always serves the first page — there is
/// no cursor yet (see the module docs).
pub const CATALOGUE_MAX_PAGE_SIZE: usize = 50;

/// Maximum tracks one release-detail read returns. Long compilations are
/// real (the mirror worker caps its own recordings listing at the same
/// bound), so the detail read gets its own cap rather than silently
/// truncating box sets at the collection page size.
pub const CATALOGUE_RELEASE_TRACKS_MAX: u32 = 1000;

/// Validate an MBID string (UUID shape `8-4-4-4-12` hex). Keeps malformed
/// `:mbid` values out of Hyperdrive SQL: the detail handler answers 404
/// before binding. Mirrors the ingest `validate_mbid`
/// (`workers/ingest/src/queue.rs`) and the mirror worker's `is_valid_mbid`,
/// kept local so this crate never depends on sibling workers.
pub fn is_catalogue_mbid(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &byte) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Own-origin art path for a release. Reserved by (not served in) this
/// slice: the art-serving card owns the reader, and until then the path
/// 404s like every unbuilt surface. The scope map already serves
/// `GET /v1/releases/*` under `library:read`, so no map change is needed
/// when the reader lands.
pub fn release_art_path(release_mbid: &str) -> String {
    format!("/v1/releases/{release_mbid}/art")
}

/// One `mb_artist` row as the store returns it. The HTTP layer maps this
/// through [`artist_response`]; stores never serialize it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistDbRow {
    pub mbid: String,
    pub name: String,
    pub sort_name: String,
    pub artist_type: Option<String>,
    pub country: Option<String>,
}

/// One `mb_release` row (joined to its artist) as the store returns it.
/// Feeds [`release_response`] and [`assemble_release_detail`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDbRow {
    pub mbid: String,
    pub title: String,
    pub release_type: Option<String>,
    pub year: Option<i16>,
    pub artist_mbid: Option<String>,
    pub artist_name: Option<String>,
}

/// One `mb_track` occurrence (joined through to the artist) as the store
/// returns it. Feeds [`track_response`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDbRow {
    pub release_mbid: String,
    pub recording_mbid: String,
    pub title: String,
    pub duration_ms: Option<i32>,
    pub medium_no: Option<i16>,
    pub track_no: Option<i16>,
    pub release_title: String,
    pub artist_mbid: Option<String>,
    pub artist_name: Option<String>,
}

/// One track of a release-detail read as the store returns it: the release
/// context comes from the sibling [`ReleaseDbRow`], so the statement only
/// selects recording columns. Feeds [`assemble_release_detail`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTrackDbRow {
    pub recording_mbid: String,
    pub title: String,
    pub duration_ms: Option<i32>,
    pub medium_no: Option<i16>,
    pub track_no: Option<i16>,
}

/// One joined library row as the store returns it: the ingest ledger row
/// (`job_id`, `release_mbid`, the full `recording_mbids` array) crossed
/// with one unnested recording plus its mirror enrichment. `recording_mbid`
/// is `None` exactly for the unmatched-union rows (empty array by design);
/// every other `Option` is `None` when the mirror has no row (unknown stays
/// NULL, never filled). Feeds [`library_entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryDbRow {
    pub job_id: String,
    pub release_mbid: Option<String>,
    pub recording_mbids: Vec<String>,
    pub recording_mbid: Option<String>,
    pub recording_ord: i64,
    pub recording_title: Option<String>,
    pub duration_ms: Option<i32>,
    pub medium_no: Option<i16>,
    pub track_no: Option<i16>,
    pub release_title: Option<String>,
    pub artist_mbid: Option<String>,
    pub artist_name: Option<String>,
}

/// Caller-visible artist. Mirrors the mirror worker's `Artist` shape
/// (`type` renames the reserved word) so the two workers never fork the
/// artist wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtistResponse {
    pub mbid: String,
    pub name: String,
    pub sort_name: String,
    #[serde(rename = "type")]
    pub artist_type: Option<String>,
    pub country: Option<String>,
}

/// Caller-visible release. `artist` is the display name (`artist_mbid` the
/// identity); `art_url` is the reserved own-origin path, always `Some`
/// here because the MBID comes from the mirror itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseResponse {
    pub mbid: String,
    pub title: String,
    pub release_type: Option<String>,
    pub year: Option<i16>,
    pub artist_mbid: Option<String>,
    pub artist: Option<String>,
    pub art_url: Option<String>,
}

/// Caller-visible track occurrence: the canonical `(release, medium, track)`
/// key (ADR 0002 §6) with both MBIDs, both positions, and the release's
/// own-origin art path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackResponse {
    pub mbid: String,
    pub title: String,
    pub release_mbid: String,
    pub release: String,
    pub artist_mbid: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<i32>,
    pub track_no: Option<i16>,
    pub medium_no: Option<i16>,
    pub art_url: Option<String>,
}

/// Caller-visible library entry: the M1 row (`id`, `recording_mbids`,
/// `release_mbid`) plus the rich enrichment the web library renders. `id`
/// is the recording MBID for matched rows, the ingest `job_id` for
/// unmatched rows; `title`/`artist`/`release` are `None` exactly when the
/// mirror has nothing (unmatched ingest, or a match the mirror does not
/// carry). `art_url` is `Some` only for well-formed release MBIDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryEntry {
    pub id: String,
    pub recording_mbids: Vec<String>,
    pub release_mbid: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artist_mbid: Option<String>,
    pub release: Option<String>,
    pub duration_ms: Option<i32>,
    pub track_no: Option<i16>,
    pub medium_no: Option<i16>,
    pub art_url: Option<String>,
}

/// Caller-visible release detail: the release plus its tracks in album
/// order (`medium_no`, `track_no`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseDetail {
    pub release: ReleaseResponse,
    pub tracks: Vec<TrackResponse>,
}

/// Map one artist row to its response. Total: every column has a field.
pub fn artist_response(row: &ArtistDbRow) -> ArtistResponse {
    ArtistResponse {
        mbid: row.mbid.clone(),
        name: row.name.clone(),
        sort_name: row.sort_name.clone(),
        artist_type: row.artist_type.clone(),
        country: row.country.clone(),
    }
}

/// Map one release row to its response. The MBID comes from the mirror, so
/// the own-origin art path is always well-formed here.
pub fn release_response(row: &ReleaseDbRow) -> ReleaseResponse {
    ReleaseResponse {
        mbid: row.mbid.clone(),
        title: row.title.clone(),
        release_type: row.release_type.clone(),
        year: row.year,
        artist_mbid: row.artist_mbid.clone(),
        artist: row.artist_name.clone(),
        art_url: Some(release_art_path(&row.mbid)),
    }
}

/// Map one track occurrence row to its response. Both MBIDs come from the
/// mirror, so the art path is always well-formed here.
pub fn track_response(row: &TrackDbRow) -> TrackResponse {
    TrackResponse {
        mbid: row.recording_mbid.clone(),
        title: row.title.clone(),
        release_mbid: row.release_mbid.clone(),
        release: row.release_title.clone(),
        artist_mbid: row.artist_mbid.clone(),
        artist: row.artist_name.clone(),
        duration_ms: row.duration_ms,
        track_no: row.track_no,
        medium_no: row.medium_no,
        art_url: Some(release_art_path(&row.release_mbid)),
    }
}

/// Map one joined library row to its entry. The `recording_mbids` array and
/// `release_mbid` mirror the ingest write verbatim (the M1 contract reads
/// them off `items[0]`); enrichment degrades to `None` per field, never to
/// an error. The art path is gated on the stored release MBID's shape: a
/// poisoned value yields `None`, never a crafted URL.
pub fn library_entry(row: &LibraryDbRow) -> LibraryEntry {
    LibraryEntry {
        id: row
            .recording_mbid
            .clone()
            .unwrap_or_else(|| row.job_id.clone()),
        recording_mbids: row.recording_mbids.clone(),
        release_mbid: row.release_mbid.clone(),
        title: row.recording_title.clone(),
        artist: row.artist_name.clone(),
        artist_mbid: row.artist_mbid.clone(),
        release: row.release_title.clone(),
        duration_ms: row.duration_ms,
        track_no: row.track_no,
        medium_no: row.medium_no,
        art_url: row
            .release_mbid
            .as_deref()
            .and_then(|mbid| is_catalogue_mbid(mbid).then(|| release_art_path(mbid))),
    }
}

/// Assemble one release detail from its release row plus its track rows
/// (already in album order from [`release_tracks_statement`]). The release
/// context (titles, artist, art path) fills every track: the tracks
/// statement selects recording columns only.
pub fn assemble_release_detail(
    release: &ReleaseDbRow,
    tracks: &[ReleaseTrackDbRow],
) -> ReleaseDetail {
    let detail = release_response(release);
    let art_url = Some(release_art_path(&release.mbid));
    ReleaseDetail {
        tracks: tracks
            .iter()
            .map(|track| TrackResponse {
                mbid: track.recording_mbid.clone(),
                title: track.title.clone(),
                release_mbid: release.mbid.clone(),
                release: release.title.clone(),
                artist_mbid: release.artist_mbid.clone(),
                artist: release.artist_name.clone(),
                duration_ms: track.duration_ms,
                track_no: track.track_no,
                medium_no: track.medium_no,
                art_url: art_url.clone(),
            })
            .collect(),
        release: detail,
    }
}

/// Per-user library page: the ingest ledger crossed with one unnested
/// recording per row plus its mirror enrichment, matched content first.
/// Parameters: owner user id ($1), row bound ($2, [`CATALOGUE_MAX_PAGE_SIZE`]).
///
/// - The `LATERAL` subselect unnests the job's `recording_mbids` with
///   ordinality (album order) and pre-guards each element: `recording_uuid`
///   is NULL for malformed values, so the `mb_recording` join degrades to
///   NULL enrichment instead of aborting the listing (see the module docs
///   on `CASE` evaluation order). Joins compare `uuid = uuid` so every
///   mirror probe hits its primary-key index.
/// - Rows whose array is empty (the by-design unmatched write) produce no
///   unnest rows, so the `UNION ALL` branch re-adds them as single entries
///   (NULL recording, `0` ordinal): a user's upload is never silently
///   hidden. Both branches carry the release/artist enrichment so an
///   unmatched-but-released job still shows its release.
/// - `ORDER BY release_mbid NULLS LAST, job_id DESC, recording_ord ASC`
///   applies to the union: matched ingests first, stable within. The
///   ordering names the bare `release_mbid` output column because Postgres
///   rejects expressions in a `UNION` `ORDER BY` (`(x IS NULL)` is an
///   expression, not a result column).
///
pub fn library_list_statement() -> &'static str {
    "SELECT l.job_id, l.release_mbid, l.recording_mbids, u.recording_mbid, u.recording_ord, \
     rec.title AS recording_title, rec.duration_ms, t.medium_no, t.track_no, \
     r.title AS release_title, r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM library l \
     LEFT JOIN LATERAL (SELECT value AS recording_mbid, ord AS recording_ord, \
     CASE WHEN value ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' \
     THEN value::uuid END AS recording_uuid \
     FROM unnest(l.recording_mbids) WITH ORDINALITY AS u2(value, ord)) AS u ON true \
     LEFT JOIN mb_recording rec ON rec.mbid = u.recording_uuid \
     LEFT JOIN mb_track t ON t.release_mbid = CASE WHEN l.release_mbid ~ \
     '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' \
     THEN l.release_mbid::uuid END AND t.recording_mbid = rec.mbid \
     LEFT JOIN mb_release r ON r.mbid = CASE WHEN l.release_mbid ~ \
     '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' \
     THEN l.release_mbid::uuid END \
     LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     WHERE l.user_id = $1 AND coalesce(cardinality(l.recording_mbids), 0) > 0 \
     UNION ALL \
     SELECT l.job_id, l.release_mbid, l.recording_mbids, NULL::text AS recording_mbid, \
     0::bigint AS recording_ord, NULL::text AS recording_title, NULL::integer AS duration_ms, \
     NULL::smallint AS medium_no, NULL::smallint AS track_no, r.title AS release_title, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM library l \
     LEFT JOIN mb_release r ON r.mbid = CASE WHEN l.release_mbid ~ \
     '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' \
     THEN l.release_mbid::uuid END \
     LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     WHERE l.user_id = $1 AND coalesce(cardinality(l.recording_mbids), 0) = 0 \
     ORDER BY release_mbid NULLS LAST, job_id DESC, recording_ord ASC LIMIT $2"
}

/// Artist collection page, ordered by MBID (stable under renames, no false
/// "ranked" implication). Parameters: row bound ($1, [`CATALOGUE_MAX_PAGE_SIZE`]).
pub fn artists_list_statement() -> &'static str {
    "SELECT mbid::text AS mbid, name, sort_name, type, country::text AS country \
     FROM mb_artist ORDER BY mbid ASC LIMIT $1"
}

/// Release collection page with each release's artist, ordered by MBID.
/// Parameters: row bound ($1, [`CATALOGUE_MAX_PAGE_SIZE`]).
pub fn releases_list_statement() -> &'static str {
    "SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     ORDER BY r.mbid ASC LIMIT $1"
}

/// Track-occurrence collection page: every `mb_track` appearance with its
/// recording, release, and artist (inner joins — `mb_track` foreign keys
/// guarantee the parents). Ordered by release, then true album position.
/// Parameters: row bound ($1, [`CATALOGUE_MAX_PAGE_SIZE`]).
pub fn tracks_list_statement() -> &'static str {
    "SELECT t.release_mbid::text AS release_mbid, t.recording_mbid::text AS recording_mbid, \
     rec.title, rec.duration_ms, t.medium_no, t.track_no, r.title AS release_title, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid \
     JOIN mb_release r ON r.mbid = t.release_mbid \
     LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     ORDER BY t.release_mbid ASC, t.medium_no ASC, t.track_no ASC LIMIT $1"
}

/// One release with its artist by MBID. Parameters: release MBID ($1, text
/// cast through `$1::text::uuid` so the driver binds a string while the
/// final cast still hits the primary-key index — the mirror worker's
/// `lookup_sql` idiom). The handler validates the shape first
/// ([`is_catalogue_mbid`]); zero rows means unknown (404), which doubles as
/// the detail read's existence probe.
pub fn release_read_statement() -> &'static str {
    "SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     WHERE r.mbid = $1::text::uuid"
}

/// One release's recordings in true album order from `mb_track` (migration
/// 0016), not from `mb_recording`: the mirror keeps only one canonical
/// release per recording there, and `mb_track` holds every appearance with
/// its medium position — the mirror worker's `RELEASE_RECORDINGS_SQL`
/// idiom. Parameters: release MBID ($1), row bound ($2,
/// [`CATALOGUE_RELEASE_TRACKS_MAX`]).
pub fn release_tracks_statement() -> &'static str {
    "SELECT t.recording_mbid::text AS recording_mbid, rec.title, rec.duration_ms, \
     t.medium_no, t.track_no \
     FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid \
     WHERE t.release_mbid = $1::text::uuid \
     ORDER BY t.medium_no ASC, t.track_no ASC, rec.mbid ASC LIMIT $2"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical MBIDs the ingest matcher, the mirror worker, and the M1
    /// contract all accept.
    const GOOD_MBID: &str = "12345678-1234-1234-1234-1234567890ab";
    const GOOD_MBID_UPPER: &str = "12345678-1234-1234-1234-1234567890AB";
    const GOOD_RELEASE: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    fn library_row() -> LibraryDbRow {
        LibraryDbRow {
            job_id: "job-1".to_string(),
            release_mbid: Some(GOOD_RELEASE.to_string()),
            recording_mbids: vec![GOOD_MBID.to_string()],
            recording_mbid: Some(GOOD_MBID.to_string()),
            recording_ord: 1,
            recording_title: Some("Blue Monday".to_string()),
            duration_ms: Some(420_000),
            medium_no: Some(1),
            track_no: Some(3),
            release_title: Some("Power, Corruption & Lies".to_string()),
            artist_mbid: Some("artist-artist-artist-artist-artist00".to_string()),
            artist_name: Some("New Order".to_string()),
        }
    }

    #[test]
    fn catalogue_page_bounds_are_pinned() {
        assert_eq!(CATALOGUE_MAX_PAGE_SIZE, 50);
        assert_eq!(CATALOGUE_RELEASE_TRACKS_MAX, 1000);
    }

    #[test]
    fn mbid_shape_accepts_canonical_forms_only() {
        assert!(is_catalogue_mbid(GOOD_MBID));
        assert!(is_catalogue_mbid(GOOD_MBID_UPPER));
        for bad in [
            "",
            "job-1",
            "12345678-1234-1234-1234-1234567890ab ",
            " 12345678-1234-1234-1234-1234567890ab",
            "123456781234123412341234567890ab",
            "12345678_1234_1234_1234_1234567890ab",
            "g2345678-1234-1234-1234-1234567890ab",
            "12345678-1234-1234-1234-1234567890abc",
            "12345678-1234-1234-1234-1234567890a",
            "/v1/releases/art",
            "..",
        ] {
            assert!(!is_catalogue_mbid(bad), "{bad:?} passed validation");
        }
    }

    #[test]
    fn art_path_is_own_origin_release_art() {
        assert_eq!(
            release_art_path(GOOD_RELEASE),
            format!("/v1/releases/{GOOD_RELEASE}/art")
        );
    }

    #[test]
    fn library_entry_carries_m1_row_plus_enrichment() {
        let entry = library_entry(&library_row());
        // M1 row: id, full recording_mbids array, release.
        assert_eq!(entry.id, GOOD_MBID);
        assert_eq!(entry.recording_mbids, vec![GOOD_MBID.to_string()]);
        assert_eq!(entry.release_mbid.as_deref(), Some(GOOD_RELEASE));
        // Rich enrichment.
        assert_eq!(entry.title.as_deref(), Some("Blue Monday"));
        assert_eq!(entry.artist.as_deref(), Some("New Order"));
        assert_eq!(entry.release.as_deref(), Some("Power, Corruption & Lies"));
        assert_eq!(entry.duration_ms, Some(420_000));
        assert_eq!((entry.medium_no, entry.track_no), (Some(1), Some(3)));
        assert_eq!(
            entry.art_url.as_deref(),
            Some(format!("/v1/releases/{GOOD_RELEASE}/art").as_str())
        );
    }

    #[test]
    fn library_entry_unmatched_falls_back_to_job_id() {
        let row = LibraryDbRow {
            job_id: "job-9".to_string(),
            release_mbid: None,
            recording_mbids: Vec::new(),
            recording_mbid: None,
            recording_ord: 0,
            recording_title: None,
            duration_ms: None,
            medium_no: None,
            track_no: None,
            release_title: None,
            artist_mbid: None,
            artist_name: None,
        };
        let entry = library_entry(&row);
        // Non-empty id keeps the M1 item filter; the empty array is the
        // by-design unmatched write the contract documents.
        assert_eq!(entry.id, "job-9");
        assert!(entry.recording_mbids.is_empty());
        assert_eq!(entry.release_mbid, None);
        assert_eq!(entry.title, None);
        assert_eq!(entry.art_url, None);
    }

    #[test]
    fn library_entry_unknown_enrichment_stays_null() {
        // Matched job, mirror carries nothing: the write shape survives,
        // every enrichment is None, never an error or a tag fill.
        let row = LibraryDbRow {
            recording_mbid: Some(GOOD_MBID.to_string()),
            recording_mbids: vec![GOOD_MBID.to_string()],
            recording_title: None,
            duration_ms: None,
            medium_no: None,
            track_no: None,
            release_title: None,
            artist_mbid: None,
            artist_name: None,
            ..library_row()
        };
        let entry = library_entry(&row);
        assert_eq!(entry.id, GOOD_MBID);
        assert_eq!(entry.recording_mbids, vec![GOOD_MBID.to_string()]);
        assert_eq!(entry.title, None);
        assert_eq!(entry.artist, None);
        assert_eq!(entry.release, None);
    }

    #[test]
    fn library_entry_poisoned_release_mbid_yields_no_art_url() {
        let row = LibraryDbRow {
            release_mbid: Some("../../etc/passwd".to_string()),
            ..library_row()
        };
        let entry = library_entry(&row);
        // The stored value still mirrors verbatim (M1 diagnostics read it),
        // but no crafted URL is ever emitted.
        assert_eq!(entry.release_mbid.as_deref(), Some("../../etc/passwd"));
        assert_eq!(entry.art_url, None);
    }

    #[test]
    fn release_detail_fills_track_context_from_release() {
        let release = ReleaseDbRow {
            mbid: GOOD_RELEASE.to_string(),
            title: "Power, Corruption & Lies".to_string(),
            release_type: Some("Album".to_string()),
            year: Some(1983),
            artist_mbid: Some("artist-mbid-here-0000-000000000000".to_string()),
            artist_name: Some("New Order".to_string()),
        };
        let tracks = vec![ReleaseTrackDbRow {
            recording_mbid: GOOD_MBID.to_string(),
            title: "Blue Monday".to_string(),
            duration_ms: Some(420_000),
            medium_no: Some(1),
            track_no: Some(3),
        }];
        let detail = assemble_release_detail(&release, &tracks);
        assert_eq!(detail.release.mbid, GOOD_RELEASE);
        assert_eq!(
            detail.release.art_url.as_deref(),
            Some(format!("/v1/releases/{GOOD_RELEASE}/art").as_str())
        );
        assert_eq!(detail.tracks.len(), 1);
        let track = &detail.tracks[0];
        assert_eq!(track.mbid, GOOD_MBID);
        assert_eq!(track.release_mbid, GOOD_RELEASE);
        assert_eq!(track.release, "Power, Corruption & Lies");
        assert_eq!(track.artist.as_deref(), Some("New Order"));
        assert_eq!(track.art_url, detail.release.art_url);
        // Empty track lists are a 200 with no tracks, never a 404.
        assert!(assemble_release_detail(&release, &[]).tracks.is_empty());
    }

    #[test]
    fn track_response_maps_occurrence_positions() {
        let row = TrackDbRow {
            release_mbid: GOOD_RELEASE.to_string(),
            recording_mbid: GOOD_MBID.to_string(),
            title: "Blue Monday".to_string(),
            duration_ms: Some(420_000),
            medium_no: Some(2),
            track_no: Some(7),
            release_title: "Substance".to_string(),
            artist_mbid: None,
            artist_name: None,
        };
        let track = track_response(&row);
        assert_eq!(track.mbid, GOOD_MBID);
        assert_eq!((track.medium_no, track.track_no), (Some(2), Some(7)));
        assert_eq!(track.artist, None);
        assert_eq!(
            track.art_url.as_deref(),
            Some(format!("/v1/releases/{GOOD_RELEASE}/art").as_str())
        );
    }

    #[test]
    fn statements_bind_limits_and_guard_every_mirror_join() {
        // Every list ends in a bound LIMIT: unbounded mirror scans can
        // never reach a response.
        assert!(artists_list_statement().ends_with("LIMIT $1"));
        assert!(releases_list_statement().ends_with("LIMIT $1"));
        assert!(tracks_list_statement().ends_with("LIMIT $1"));
        assert!(library_list_statement().ends_with("LIMIT $2"));
        assert!(release_tracks_statement().ends_with("LIMIT $2"));
        // Owner scoping never leaves SQL: only the library read takes a
        // user, and it filters on it.
        assert!(library_list_statement().contains("l.user_id = $1"));
        for statement in [
            artists_list_statement(),
            releases_list_statement(),
            tracks_list_statement(),
            release_read_statement(),
            release_tracks_statement(),
        ] {
            assert!(
                !statement.contains("user_id"),
                "mirror read must not name owners: {statement}"
            );
        }
    }

    #[test]
    fn statements_never_touch_cover_archive_urls() {
        // Licence posture: the mirror's third-party art URL stays in the
        // database. Art URLs are built in Rust from our own origin.
        for statement in [
            library_list_statement(),
            artists_list_statement(),
            releases_list_statement(),
            tracks_list_statement(),
            release_read_statement(),
            release_tracks_statement(),
        ] {
            assert!(
                !statement.contains("caa_url"),
                "catalogue read must not select CAA URLs"
            );
        }
    }

    #[test]
    fn library_statement_keeps_unmatched_rows_and_orders_matched_first() {
        let statement = library_list_statement();
        // Unmatched (empty-array) rows re-enter through the union branch.
        assert!(statement.contains("UNION ALL"));
        assert!(statement.contains("cardinality(l.recording_mbids)"));
        // Matched-first, stable within.
        assert!(
            statement.contains("ORDER BY release_mbid NULLS LAST, job_id DESC, recording_ord ASC")
        );
        // Unnest preserves album order with ordinality.
        assert!(statement.contains("WITH ORDINALITY"));
    }

    #[test]
    fn library_statement_degrades_poisoned_mbids_to_null() {
        // Every stored-MBID comparison runs through the shape-guarded CASE:
        // malformed values enrich to NULL instead of aborting the listing.
        // (A bare `text::uuid` cast would 500 the whole page.)
        let statement = library_list_statement();
        assert!(
            statement.matches("THEN value::uuid END").count() >= 1,
            "recording guard missing"
        );
        assert!(
            statement.matches("THEN l.release_mbid::uuid END").count() >= 3,
            "release guard must cover the track, release, and unmatched joins"
        );
    }
}
