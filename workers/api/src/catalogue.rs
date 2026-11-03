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
//! - Per-user rows come from the ingest `library` ledger (migration 0035):
//!   one row per ingest job carrying the matched release MBID plus the
//!   release's recording MBIDs in album order. An unmatched ingest writes
//!   the empty array by design (`recording_mbids_or_fallback` in
//!   workers/ingest/src/matching.rs).
//! - Rich metadata (titles, durations, positions, artist) is NOT duplicated
//!   in the ledger: it stays authoritative in the MusicBrainz mirror
//!   (`mb_artist`, `mb_release`, `mb_recording`, `mb_track` from migrations
//!   0001/0016). The app database and the mirror live on separate clusters
//!   (ADR 0002 §1), so no single SQL statement can join them: the store
//!   reads one app-side page ([`library_page_statement`], migration 0035)
//!   plus three bounded mirror probes ([`library_recordings_statement`],
//!   [`library_tracks_statement`], [`library_releases_statement`]) and joins
//!   them in [`assemble_library_rows`]. A stored MBID that is malformed,
//!   uppercase, or absent from the mirror degrades that row's enrichment to
//!   NULL — never the listing, never an error, and never a fill from tenant
//!   file tags. The page statement carries no `text::uuid` cast at all (a
//!   bare cast on a poisoned value would abort the whole listing with a
//!   500); the enrichment probes cast only `$n::uuid[]` over inputs the
//!   store pre-filtered through [`is_catalogue_mbid`], so the cast cannot
//!   fail.
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
//!   answers 401 before any store read. Owner ids never leave SQL: the
//!   library page filters `WHERE ... user_id = $1`, the browse lists and the
//!   release detail resolve only through the caller's ledger membership (ADR
//!   0006 §6 — a non-member id returns 404 on detail reads and never appears
//!   in a list), and no owner column is selected.
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

/// One app-side library page row (see [`library_page_statement`]): the
/// ingest ledger row (`job_id`, `release_mbid`, the full `recording_mbids`
/// array) crossed with one unnested recording slot, in page order.
/// `recording_mbid` is `None` exactly for the unmatched-union rows (empty
/// array by design). No mirror data: enrichment joins in
/// [`assemble_library_rows`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryPageRow {
    pub job_id: String,
    pub release_mbid: Option<String>,
    pub recording_mbids: Vec<String>,
    pub recording_mbid: Option<String>,
    pub recording_ord: i64,
}

/// One mirror recording hit for library assembly: title and duration by
/// recording MBID. Unknown MBIDs simply have no hit — assembly degrades
/// those fields to `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRecordingHit {
    pub recording_mbid: String,
    pub title: Option<String>,
    pub duration_ms: Option<i32>,
}

/// One mirror occurrence hit for library assembly: the medium/track
/// position of an exact (release, recording) pair. The tracks probe returns
/// a superset of the page's pairs; assembly matches exact pairs only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryTrackHit {
    pub release_mbid: String,
    pub recording_mbid: String,
    pub medium_no: Option<i16>,
    pub track_no: Option<i16>,
}

/// One mirror release hit for library assembly: the release title with its
/// artist. Both page branches carry release MBIDs (an unmatched upload may
/// still name a release), so every page release resolves here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryReleaseHit {
    pub release_mbid: String,
    pub title: Option<String>,
    pub artist_mbid: Option<String>,
    pub artist_name: Option<String>,
}

/// One assembled library row as the store returns it: one [`LibraryPageRow`]
/// plus its mirror enrichment. `recording_mbid` is `None` exactly for the
/// unmatched-union rows (empty array by design); every other `Option` is
/// `None` when the mirror has no row (unknown stays NULL, never filled).
/// Feeds [`library_entry`].
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

/// Normalize a stored MBID for enrichment lookup: lowercase, then the
/// catalogue shape check. `None` for anything malformed — poisoned stored
/// values never reach a mirror probe and degrade that field to `None`,
/// while uppercase hex (accepted by the `uuid` cast) still enriches. The
/// store applies this to page MBIDs before binding the enrichment probes
/// and assembly applies it on lookup, so both sides meet on one key shape.
pub fn enrichment_key(raw: &str) -> Option<String> {
    let normalized = raw.to_ascii_lowercase();
    is_catalogue_mbid(&normalized).then_some(normalized)
}

/// Join one library page with its mirror enrichment into the assembled rows
/// the store returns (see [`LibraryDbRow`]). Page order is preserved, and
/// every enrichment degrades per field — unknown, malformed, or
/// mirror-absent MBIDs yield `None`, never an error. Track hits match exact
/// (release, recording) pairs only: the tracks probe returns a superset, so
/// a same-recording row from another release never enriches this row.
pub fn assemble_library_rows(
    page: &[LibraryPageRow],
    recordings: &[LibraryRecordingHit],
    tracks: &[LibraryTrackHit],
    releases: &[LibraryReleaseHit],
) -> Vec<LibraryDbRow> {
    use std::collections::HashMap;
    let recording_by_mbid: HashMap<String, &LibraryRecordingHit> = recordings
        .iter()
        .filter_map(|hit| enrichment_key(&hit.recording_mbid).map(|key| (key, hit)))
        .collect();
    let track_by_pair: HashMap<(String, String), &LibraryTrackHit> = tracks
        .iter()
        .filter_map(|hit| {
            enrichment_key(&hit.release_mbid).and_then(|release| {
                enrichment_key(&hit.recording_mbid)
                    .map(|recording| ((release, recording), hit))
            })
        })
        .collect();
    let release_by_mbid: HashMap<String, &LibraryReleaseHit> = releases
        .iter()
        .filter_map(|hit| enrichment_key(&hit.release_mbid).map(|key| (key, hit)))
        .collect();
    page.iter()
        .map(|row| {
            let recording_key = row.recording_mbid.as_deref().and_then(enrichment_key);
            let release_key = row.release_mbid.as_deref().and_then(enrichment_key);
            let recording_hit = recording_key
                .as_deref()
                .and_then(|key| recording_by_mbid.get(key));
            let release_hit = release_key
                .as_deref()
                .and_then(|key| release_by_mbid.get(key));
            let track_hit = match (&release_key, &recording_key) {
                (Some(release), Some(recording)) => {
                    track_by_pair.get(&(release.clone(), recording.clone()))
                }
                _ => None,
            };
            LibraryDbRow {
                job_id: row.job_id.clone(),
                release_mbid: row.release_mbid.clone(),
                recording_mbids: row.recording_mbids.clone(),
                recording_mbid: row.recording_mbid.clone(),
                recording_ord: row.recording_ord,
                recording_title: recording_hit.and_then(|hit| hit.title.clone()),
                duration_ms: recording_hit.and_then(|hit| hit.duration_ms),
                medium_no: track_hit.and_then(|hit| hit.medium_no),
                track_no: track_hit.and_then(|hit| hit.track_no),
                release_title: release_hit.and_then(|hit| hit.title.clone()),
                artist_mbid: release_hit.and_then(|hit| hit.artist_mbid.clone()),
                artist_name: release_hit.and_then(|hit| hit.artist_name.clone()),
            }
        })
        .collect()
}

/// Per-user library page: the ingest ledger crossed with one unnested
/// recording per row, matched content first. App-database only (the
/// `library` table is app-target, migration 0035): this statement names no
/// mirror relation, because the mirror lives on its own cluster (ADR 0002
/// §1) and a join across databases cannot run on either connection.
/// Parameters: owner user id ($1), row bound ($2,
/// [`CATALOGUE_MAX_PAGE_SIZE`]).
///
/// - The `LATERAL` subselect unnests the job's `recording_mbids` with
///   ordinality (album order). There is deliberately no `text::uuid` cast:
///   malformed stored values flow through as text and degrade in
///   [`assemble_library_rows`] instead of aborting the listing.
/// - Rows whose array is empty (the by-design unmatched write) produce no
///   unnest rows, so the `UNION ALL` branch re-adds them as single entries
///   (NULL recording, `0` ordinal): a user's upload is never silently
///   hidden. Both branches select the same ledger columns so page rows are
///   uniform; mirror enrichment (including release/artist for an
///   unmatched-but-released job) arrives in the probes below.
/// - `ORDER BY matched_last ASC, job_id DESC, recording_ord ASC` applies to
///   the union: matched ingests first, then job recency, then album
///   position. The ordering names the computed `matched_last` output column
///   because Postgres rejects expressions in a `UNION` `ORDER BY` (`(x IS
///   NULL)` is an expression, not a result column — and naming the bare
///   `release_mbid` column would sort matched rows by release text instead
///   of job recency).
///
pub fn library_page_statement() -> &'static str {
    "SELECT l.job_id, l.release_mbid, l.recording_mbids, u.recording_mbid, u.recording_ord, \
     (l.release_mbid IS NULL) AS matched_last \
     FROM library l \
     LEFT JOIN LATERAL (SELECT value AS recording_mbid, ord AS recording_ord \
     FROM unnest(l.recording_mbids) WITH ORDINALITY AS u2(value, ord)) AS u ON true \
     WHERE l.user_id = $1 AND coalesce(cardinality(l.recording_mbids), 0) > 0 \
     UNION ALL \
     SELECT l.job_id, l.release_mbid, l.recording_mbids, NULL::text AS recording_mbid, \
     0::bigint AS recording_ord, (l.release_mbid IS NULL) AS matched_last \
     FROM library l \
     WHERE l.user_id = $1 AND coalesce(cardinality(l.recording_mbids), 0) = 0 \
     ORDER BY matched_last ASC, job_id DESC, recording_ord ASC LIMIT $2"
}

/// Mirror recording enrichment for one library page: titles and durations
/// for the page's recording MBIDs. Mirror-database only (`mb_recording` is
/// mirror-target, migrations 0001/0016): runs on the mirror connection.
/// Parameters: the page's well-formed recording MBIDs ($1, `text[]` cast to
/// `uuid[]` — the store pre-filters every element through
/// [`is_catalogue_mbid`], so the cast cannot fail; the probe hits the
/// primary-key index either way).
pub fn library_recordings_statement() -> &'static str {
    "SELECT mbid::text AS recording_mbid, title AS recording_title, duration_ms \
     FROM mb_recording WHERE mbid = ANY($1::uuid[])"
}

/// Mirror occurrence enrichment for one library page: medium/track numbers
/// for the page's (release, recording) pairs. Mirror-database only
/// (`mb_track` is mirror-target, migration 0016). Parameters: the page's
/// well-formed release MBIDs ($1) and recording MBIDs ($2), same cast and
/// pre-filter as above. The two `ANY` probes are independent, so the result
/// is a SUPERSET of the page's exact pairs (a recording that appears on
/// two releases returns both rows); [`assemble_library_rows`] matches exact
/// pairs only, so a track from another release never enriches this row.
pub fn library_tracks_statement() -> &'static str {
    "SELECT release_mbid::text AS release_mbid, recording_mbid::text AS recording_mbid, \
     medium_no, track_no FROM mb_track \
     WHERE release_mbid = ANY($1::uuid[]) AND recording_mbid = ANY($2::uuid[])"
}

/// Mirror release enrichment for one library page: titles with artists for
/// the page's release MBIDs. Mirror-database only (`mb_release` and
/// `mb_artist` are mirror-target, migrations 0001/0016). Parameters: the
/// page's well-formed release MBIDs ($1, same cast and pre-filter as
/// above). Both page branches carry release MBIDs, so an unmatched upload
/// that names a release still enriches here.
pub fn library_releases_statement() -> &'static str {
    "SELECT r.mbid::text AS release_mbid, r.title AS release_title, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     WHERE r.mbid = ANY($1::uuid[])"
}

/// One app-side ledger row's release for the membership gate: the distinct
/// release MBIDs the caller owns. App-database only (the `library` table is
/// app-target, migration 0035). There is deliberately no `::uuid` cast:
/// malformed stored values flow through as text and are dropped by
/// [`member_release_mbids`], never bound into a mirror probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipReleaseRow {
    pub release_mbid: String,
}

/// One app-side ledger occurrence for the membership gate: a distinct
/// (release, recording) pair the caller owns. Same no-cast discipline as
/// [`MembershipReleaseRow`]; [`member_pair_arrays`] normalizes both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipTrackPairRow {
    pub release_mbid: String,
    pub recording_mbid: String,
}

/// Distinct well-formed release MBIDs the caller owns, sorted for a stable
/// probe. Malformed stored values never leave this function.
pub fn member_release_mbids(rows: &[MembershipReleaseRow]) -> Vec<String> {
    let mut mbids: Vec<String> = rows
        .iter()
        .filter_map(|row| enrichment_key(&row.release_mbid))
        .collect();
    mbids.sort();
    mbids.dedup();
    mbids
}

/// Distinct well-formed (release, recording) pairs the caller owns plus the
/// release and recording arrays that probe them: the two `ANY` probes are
/// independent, so the mirror returns a superset and the store keeps only
/// exact pairs via [`retain_member_track_occurrences`]. All three outputs are
/// sorted and deduplicated for a stable probe.
pub fn member_pair_arrays(
    rows: &[MembershipTrackPairRow],
) -> (Vec<String>, Vec<String>, Vec<(String, String)>) {
    let mut pairs: Vec<(String, String)> = rows
        .iter()
        .filter_map(|row| {
            enrichment_key(&row.release_mbid).and_then(|release| {
                enrichment_key(&row.recording_mbid).map(|recording| (release, recording))
            })
        })
        .collect();
    pairs.sort();
    pairs.dedup();
    let mut releases: Vec<String> = pairs.iter().map(|pair| pair.0.clone()).collect();
    releases.sort();
    releases.dedup();
    let mut recordings: Vec<String> = pairs.iter().map(|pair| pair.1.clone()).collect();
    recordings.sort();
    recordings.dedup();
    (releases, recordings, pairs)
}

/// Keep only the mirror track occurrences whose (release, recording) pair is
/// in the caller's membership, preserving probe order. A same-recording row
/// from another release never survives: membership is per occurrence (ADR
/// 0002 §6), not per recording.
pub fn retain_member_track_occurrences(
    rows: Vec<TrackDbRow>,
    pairs: &[(String, String)],
) -> Vec<TrackDbRow> {
    use std::collections::HashSet;
    let owned: HashSet<&(String, String)> = pairs.iter().collect();
    rows.into_iter()
        .filter(|row| {
            enrichment_key(&row.release_mbid)
                .and_then(|release| {
                    enrichment_key(&row.recording_mbid).map(|recording| (release, recording))
                })
                .is_some_and(|pair| owned.contains(&pair))
        })
        .collect()
}

/// App-side distinct member releases for the browse lists. Parameters: owner
/// user id ($1), member-release bound ($2, [`CATALOGUE_MAX_PAGE_SIZE`]): the
/// responses are first pages too, so the membership input is capped at the
/// same bound, ordered by release MBID for a stable page.
pub fn catalogue_member_releases_statement() -> &'static str {
    "SELECT DISTINCT release_mbid FROM library \
     WHERE user_id = $1 AND release_mbid IS NOT NULL \
     ORDER BY release_mbid ASC LIMIT $2"
}

/// App-side distinct member (release, recording) pairs for the track list.
/// Parameters: owner user id ($1), member-pair bound ($2,
/// [`CATALOGUE_MAX_PAGE_SIZE`]), same stable-page discipline as above.
pub fn catalogue_member_track_pairs_statement() -> &'static str {
    "SELECT DISTINCT l.release_mbid AS release_mbid, u.recording_mbid AS recording_mbid \
     FROM library l \
     JOIN LATERAL (SELECT value AS recording_mbid \
     FROM unnest(l.recording_mbids) AS u2(value)) AS u ON true \
     WHERE l.user_id = $1 AND l.release_mbid IS NOT NULL \
     AND coalesce(cardinality(l.recording_mbids), 0) > 0 \
     ORDER BY release_mbid ASC, recording_mbid ASC LIMIT $2"
}

/// App-side membership probe for the release detail read. Parameters: owner
/// user id ($1), requested release MBID lowercased through
/// [`enrichment_key`] ($2). `lower()` keeps the check case-insensitive like
/// the mirror's `::uuid` cast: one row means member, zero rows means the
/// handler answers 404 before any mirror probe.
pub fn catalogue_release_membership_statement() -> &'static str {
    "SELECT 1 FROM library WHERE user_id = $1 AND lower(release_mbid) = $2 LIMIT 1"
}

/// Artist collection page, member-scoped: only artists of the caller's member
/// releases, ordered by MBID (stable under renames, no false "ranked"
/// implication). Parameters: the caller's well-formed member release MBIDs
/// ($1, `uuid[]` cast — pre-filtered through [`enrichment_key`], so the cast
/// cannot fail), row bound ($2, [`CATALOGUE_MAX_PAGE_SIZE`]).
pub fn artists_list_statement() -> &'static str {
    "SELECT mbid::text AS mbid, name, sort_name, type, country::text AS country \
     FROM mb_artist WHERE mbid IN (SELECT r.artist_mbid FROM mb_release r \
     WHERE r.mbid = ANY($1::uuid[])) ORDER BY mbid ASC LIMIT $2"
}

/// Release collection page with each release's artist, member-scoped and
/// ordered by MBID. Parameters: the caller's well-formed member release
/// MBIDs ($1, same cast and pre-filter as above), row bound ($2,
/// [`CATALOGUE_MAX_PAGE_SIZE`]).
pub fn releases_list_statement() -> &'static str {
    "SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     WHERE r.mbid = ANY($1::uuid[]) ORDER BY r.mbid ASC LIMIT $2"
}

/// Track-occurrence collection page, member-scoped: every member `mb_track`
/// appearance with its recording, release, and artist (inner joins —
/// `mb_track` foreign keys guarantee the parents). Ordered by release, then
/// true album position. Parameters: the caller's well-formed member release
/// ($1) and recording ($2) MBIDs (same cast and pre-filter as above); the
/// store keeps exact pairs only via [`retain_member_track_occurrences`].
/// Row bound ($3, [`CATALOGUE_MAX_PAGE_SIZE`]).
pub fn tracks_list_statement() -> &'static str {
    "SELECT t.release_mbid::text AS release_mbid, t.recording_mbid::text AS recording_mbid, \
     rec.title, rec.duration_ms, t.medium_no, t.track_no, r.title AS release_title, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid \
     JOIN mb_release r ON r.mbid = t.release_mbid \
     LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     WHERE t.release_mbid = ANY($1::uuid[]) AND t.recording_mbid = ANY($2::uuid[]) \
     ORDER BY t.release_mbid ASC, t.medium_no ASC, t.track_no ASC LIMIT $3"
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
        // never reach a response. The member lists bind their membership
        // arrays first, so the row bound moves to $2 ($3 for tracks).
        assert!(artists_list_statement().ends_with("LIMIT $2"));
        assert!(releases_list_statement().ends_with("LIMIT $2"));
        assert!(tracks_list_statement().ends_with("LIMIT $3"));
        assert!(library_page_statement().ends_with("LIMIT $2"));
        assert!(artists_list_statement().contains("ANY($1::uuid[])"));
        assert!(releases_list_statement().contains("ANY($1::uuid[])"));
        assert!(tracks_list_statement().contains("ANY($1::uuid[])"));
        assert!(tracks_list_statement().contains("ANY($2::uuid[])"));
        assert!(library_recordings_statement().contains("ANY($1::uuid[])"));
        assert!(library_tracks_statement().contains("ANY($2::uuid[])"));
        assert!(library_releases_statement().contains("ANY($1::uuid[])"));
        assert!(release_tracks_statement().ends_with("LIMIT $2"));
        // Owner scoping never leaves SQL: the library page, the membership
        // reads, and nothing else take a user, and each filters on it. The
        // mirror probes take member MBID arrays only.
        assert!(library_page_statement().contains("l.user_id = $1"));
        assert!(catalogue_member_releases_statement().contains("user_id = $1"));
        assert!(catalogue_member_track_pairs_statement().contains("l.user_id = $1"));
        assert!(catalogue_release_membership_statement().contains("user_id = $1"));
        for statement in [
            artists_list_statement(),
            releases_list_statement(),
            tracks_list_statement(),
            release_read_statement(),
            release_tracks_statement(),
            library_recordings_statement(),
            library_tracks_statement(),
            library_releases_statement(),
        ] {
            assert!(
                !statement.contains("user_id"),
                "mirror read must not name owners: {statement}"
            );
        }
        // The page runs on the app database, which carries no mirror
        // tables: naming one would 500 every library read on staging.
        for table in ["mb_recording", "mb_track", "mb_release", "mb_artist"] {
            assert!(
                !library_page_statement().contains(table),
                "app-side page must not name {table}"
            );
        }
    }

    #[test]
    fn statements_never_touch_cover_archive_urls() {
        // Licence posture: the mirror's third-party art URL stays in the
        // database. Art URLs are built in Rust from our own origin.
        for statement in [
            library_page_statement(),
            library_recordings_statement(),
            library_tracks_statement(),
            library_releases_statement(),
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
        let statement = library_page_statement();
        // Unmatched (empty-array) rows re-enter through the union branch.
        assert!(statement.contains("UNION ALL"));
        assert!(statement.contains("cardinality(l.recording_mbids)"));
        // Matched-first, then job recency, then album position: the
        // ordering names the computed `matched_last` column because
        // Postgres rejects expressions in a UNION ORDER BY (and the bare
        // `release_mbid` column would sort matched rows by release text,
        // not recency).
        assert!(statement.contains("(l.release_mbid IS NULL) AS matched_last"));
        assert!(
            statement.contains("ORDER BY matched_last ASC, job_id DESC, recording_ord ASC")
        );
        // Unnest preserves album order with ordinality.
        assert!(statement.contains("WITH ORDINALITY"));
    }

    #[test]
    fn library_statements_are_poison_proof_by_construction() {
        // The app-side page carries no `::uuid` cast at all: malformed
        // stored values flow through as text (a bare cast would 500 the
        // whole page) and degrade in `assemble_library_rows`.
        assert!(!library_page_statement().contains("::uuid"));
        assert!(!library_page_statement().contains("CASE"));
        // The app-side membership reads carry no `::uuid` cast either:
        // they select raw stored text and the `member_*` helpers normalize
        // it in Rust, so one poisoned ledger value degrades one membership,
        // never the read.
        for statement in [
            catalogue_member_releases_statement(),
            catalogue_member_track_pairs_statement(),
            catalogue_release_membership_statement(),
        ] {
            assert!(
                !statement.contains("::uuid"),
                "membership read must not cast stored text: {statement}"
            );
        }
        // The enrichment and browse probes DO cast — but only over
        // `$n::uuid[]` inputs the store pre-filtered through
        // `is_catalogue_mbid`, so the cast cannot fail. Pin the cast shape
        // so a future edit cannot widen the probes to raw stored values.
        assert_eq!(
            library_recordings_statement().matches("::uuid[]").count(),
            1,
            "recordings probe must cast exactly its MBID array"
        );
        assert_eq!(
            library_tracks_statement().matches("::uuid[]").count(),
            2,
            "tracks probe must cast exactly its release and recording arrays"
        );
        assert_eq!(
            library_releases_statement().matches("::uuid[]").count(),
            1,
            "releases probe must cast exactly its MBID array"
        );
        assert_eq!(
            artists_list_statement().matches("::uuid[]").count(),
            1,
            "artist list must cast exactly its member release array"
        );
        assert_eq!(
            releases_list_statement().matches("::uuid[]").count(),
            1,
            "release list must cast exactly its member release array"
        );
        assert_eq!(
            tracks_list_statement().matches("::uuid[]").count(),
            2,
            "track list must cast exactly its member release and recording arrays"
        );
    }

    #[test]
    fn membership_reads_stay_bounded_and_owner_scoped() {
        // The browse lists resolve through these app-side reads, so they
        // carry the same disciplines as the library page: owner filter,
        // stable order, and a bound LIMIT. The detail probe answers one
        // row for a member and zero rows for anyone else.
        let releases = catalogue_member_releases_statement();
        assert!(releases.contains("user_id = $1"));
        assert!(releases.contains("release_mbid IS NOT NULL"));
        assert!(releases.contains("ORDER BY release_mbid ASC LIMIT $2"));
        let pairs = catalogue_member_track_pairs_statement();
        assert!(pairs.contains("l.user_id = $1"));
        assert!(pairs.contains("unnest(l.recording_mbids)"));
        assert!(pairs.contains("ORDER BY release_mbid ASC, recording_mbid ASC LIMIT $2"));
        let membership = catalogue_release_membership_statement();
        assert!(membership.contains("user_id = $1"));
        assert!(membership.contains("lower(release_mbid) = $2"));
        assert!(membership.ends_with("LIMIT 1"));
        for statement in [releases, pairs] {
            assert!(
                !statement.contains("mb_"),
                "app-side membership must not name mirror tables: {statement}"
            );
        }
    }

    #[test]
    fn member_helpers_keep_exact_pairs_only() {
        let releases = vec![
            MembershipReleaseRow {
                release_mbid: GOOD_RELEASE.to_string(),
            },
            MembershipReleaseRow {
                release_mbid: GOOD_RELEASE.to_ascii_uppercase(),
            },
            MembershipReleaseRow {
                release_mbid: "not-a-uuid".to_string(),
            },
        ];
        assert_eq!(member_release_mbids(&releases), vec![GOOD_RELEASE.to_string()]);
        let rows = vec![
            MembershipTrackPairRow {
                release_mbid: GOOD_RELEASE.to_string(),
                recording_mbid: GOOD_MBID.to_string(),
            },
            MembershipTrackPairRow {
                release_mbid: GOOD_RELEASE.to_ascii_uppercase(),
                recording_mbid: GOOD_MBID_UPPER.to_string(),
            },
            MembershipTrackPairRow {
                release_mbid: GOOD_RELEASE.to_string(),
                recording_mbid: "not-a-uuid".to_string(),
            },
            MembershipTrackPairRow {
                release_mbid: "not-a-uuid".to_string(),
                recording_mbid: GOOD_MBID.to_string(),
            },
        ];
        let (releases, recordings, pairs) = member_pair_arrays(&rows);
        assert_eq!(releases, vec![GOOD_RELEASE.to_string()]);
        assert_eq!(recordings, vec![GOOD_MBID.to_string()]);
        assert_eq!(pairs, vec![(GOOD_RELEASE.to_string(), GOOD_MBID.to_string())]);
        let member = TrackDbRow {
            release_mbid: GOOD_RELEASE.to_ascii_uppercase(),
            recording_mbid: GOOD_MBID_UPPER.to_string(),
            title: "Member".to_string(),
            duration_ms: None,
            medium_no: Some(1),
            track_no: Some(1),
            release_title: "Release".to_string(),
            artist_mbid: None,
            artist_name: None,
        };
        let cross_release = TrackDbRow {
            release_mbid: "bbbbbbbb-1111-2222-3333-444444444444".to_string(),
            recording_mbid: GOOD_MBID.to_string(),
            ..member.clone()
        };
        assert_eq!(
            retain_member_track_occurrences(vec![member.clone(), cross_release], &pairs),
            vec![member],
            "same recording on another release is a different occurrence"
        );
    }

    #[test]
    fn enrichment_key_normalizes_case_and_rejects_poison() {
        assert_eq!(
            enrichment_key(GOOD_MBID_UPPER),
            Some(GOOD_MBID.to_string()),
            "uppercase hex must meet lowercase mirror output on one key"
        );
        assert_eq!(enrichment_key(GOOD_MBID), Some(GOOD_MBID.to_string()));
        for bad in ["not-a-uuid", "", "../../etc/passwd", "job-1"] {
            assert_eq!(enrichment_key(bad), None, "{bad:?} must match nothing");
        }
    }

    fn page_row(job: &str, release: Option<&str>, rec: Option<&str>, ord: i64) -> LibraryPageRow {
        LibraryPageRow {
            job_id: job.to_string(),
            release_mbid: release.map(str::to_string),
            recording_mbids: rec.map(|m| vec![m.to_string()]).unwrap_or_default(),
            recording_mbid: rec.map(str::to_string),
            recording_ord: ord,
        }
    }

    fn assembled_fixtures() -> Vec<LibraryDbRow> {
        let page = vec![
            // Fully matched row.
            page_row("job-1", Some(GOOD_RELEASE), Some(GOOD_MBID), 1),
            // Uppercase stored MBIDs (uuid comparison is case-insensitive).
            page_row("job-2", Some(&GOOD_RELEASE.to_ascii_uppercase()), Some(GOOD_MBID_UPPER), 1),
            // Unknown-but-well-formed MBIDs: every enrichment degrades.
            page_row("job-3", Some("bbbbbbbb-1111-2222-3333-444444444444"), Some("99999999-1111-2222-3333-444444444444"), 1),
            // Poisoned stored MBIDs: degrade, never error, never match.
            page_row("job-4", Some("not-a-uuid"), Some("also-not-a-uuid"), 1),
            // Unmatched upload: NULL recording, job id identity.
            page_row("job-9", None, None, 0),
        ];
        let recordings = vec![LibraryRecordingHit {
            recording_mbid: GOOD_MBID.to_string(),
            title: Some("Blue Monday".to_string()),
            duration_ms: Some(420_000),
        }];
        let tracks = vec![
            LibraryTrackHit {
                release_mbid: GOOD_RELEASE.to_string(),
                recording_mbid: GOOD_MBID.to_string(),
                medium_no: Some(1),
                track_no: Some(3),
            },
            // Same recording on another release: the probe returns it
            // (superset), but it must never enrich a row of GOOD_RELEASE.
            LibraryTrackHit {
                release_mbid: "bbbbbbbb-1111-2222-3333-444444444444".to_string(),
                recording_mbid: GOOD_MBID.to_string(),
                medium_no: Some(2),
                track_no: Some(7),
            },
        ];
        let releases = vec![LibraryReleaseHit {
            release_mbid: GOOD_RELEASE.to_string(),
            title: Some("Power, Corruption & Lies".to_string()),
            artist_mbid: Some("artist-artist-artist-artist-artist00".to_string()),
            artist_name: Some("New Order".to_string()),
        }];
        assemble_library_rows(&page, &recordings, &tracks, &releases)
    }

    #[test]
    fn assemble_library_rows_preserves_order_and_enriches_exact_pairs() {
        let rows = assembled_fixtures();
        assert_eq!(rows.len(), 5);
        // Page order is preserved (the statement owns the ordering).
        let jobs: Vec<&str> = rows.iter().map(|row| row.job_id.as_str()).collect();
        assert_eq!(jobs, vec!["job-1", "job-2", "job-3", "job-4", "job-9"]);
        // Fully matched row: every field enriched, exact-pair positions.
        let matched = &rows[0];
        assert_eq!(matched.recording_mbid.as_deref(), Some(GOOD_MBID));
        assert_eq!(matched.recording_title.as_deref(), Some("Blue Monday"));
        assert_eq!(matched.duration_ms, Some(420_000));
        assert_eq!((matched.medium_no, matched.track_no), (Some(1), Some(3)));
        assert_eq!(
            matched.release_title.as_deref(),
            Some("Power, Corruption & Lies")
        );
        assert_eq!(matched.artist_name.as_deref(), Some("New Order"));
        // Uppercase stored MBIDs still enrich (keys normalize).
        let upper = &rows[1];
        assert_eq!(upper.recording_title.as_deref(), Some("Blue Monday"));
        assert_eq!((upper.medium_no, upper.track_no), (Some(1), Some(3)));
        assert_eq!(upper.artist_name.as_deref(), Some("New Order"));
        // Unknown-but-well-formed MBIDs: the write shape survives, every
        // enrichment is None — and the cross-release same-recording track
        // row (2.7) must NOT leak into this row (no release hit either).
        let unknown = &rows[2];
        assert_eq!(
            unknown.recording_mbid.as_deref(),
            Some("99999999-1111-2222-3333-444444444444")
        );
        assert_eq!(unknown.recording_title, None);
        assert_eq!(unknown.medium_no, None);
        assert_eq!(unknown.release_title, None);
        // Poisoned stored MBIDs degrade too, and match nothing.
        let poisoned = &rows[3];
        assert_eq!(
            poisoned.recording_mbid.as_deref(),
            Some("also-not-a-uuid")
        );
        assert_eq!(poisoned.recording_title, None);
        assert_eq!(poisoned.release_title, None);
        // Unmatched upload: NULL recording, job-id identity downstream.
        let unmatched = &rows[4];
        assert_eq!(unmatched.recording_mbid, None);
        assert_eq!(unmatched.recording_ord, 0);
        assert!(unmatched.recording_mbids.is_empty());
    }

    #[test]
    fn assemble_library_rows_needs_both_pair_halves_for_positions() {
        // A row whose release is poisoned but whose recording is known
        // gets the recording enrichment but NO occurrence position: the
        // old listing required both halves of the pair, and so does the
        // assembly.
        let page = vec![page_row("job-5", Some("not-a-uuid"), Some(GOOD_MBID), 1)];
        let recordings = vec![LibraryRecordingHit {
            recording_mbid: GOOD_MBID.to_string(),
            title: Some("Blue Monday".to_string()),
            duration_ms: Some(420_000),
        }];
        let tracks = vec![LibraryTrackHit {
            release_mbid: GOOD_RELEASE.to_string(),
            recording_mbid: GOOD_MBID.to_string(),
            medium_no: Some(1),
            track_no: Some(3),
        }];
        let rows = assemble_library_rows(&page, &recordings, &tracks, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recording_title.as_deref(), Some("Blue Monday"));
        assert_eq!(rows[0].medium_no, None);
        assert_eq!(rows[0].release_title, None);
    }
}
