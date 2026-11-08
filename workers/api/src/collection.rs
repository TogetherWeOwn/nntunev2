//! Collection saves: durable per-account saved albums + followed artists.
//!
//! Native core for the `/v1/me/library/albums/{mbid}` and
//! `/v1/me/library/artists/{mbid}` routes (idempotent save/remove, state
//! read) and the bounded `GET /v1/me/library/albums` +
//! `GET /v1/me/library/artists` collections — an account preference
//! ledger, never ingest, playback, listens, or access. The wire contract
//! is `docs/collection-saves-api-contract.md`. Pure row shapes plus the
//! parameterized statements the Worker entry binds against PlanetScale
//! Postgres via Hyperdrive — never string-interpolated SQL.
//!
//! Sources:
//! - Save rows live in the app database (`collection_saves`, migration
//!   0037): `(user_id, kind, mbid)` primary key with `saved_at` unix
//!   seconds. `kind` is `'album'` or `'artist'`, the stored `mbid` is
//!   TEXT with no `::uuid` cast anywhere on the app side (the Rust shape
//!   gate below binds only well-formed values).
//! - Target validation splits across both clusters like the catalogue
//!   reads (ADR 0002 §1): album membership comes from the app-side
//!   `library` ledger's own-or-shared rows
//!   ([`catalogue_release_membership_statement`], uncapped — the whole
//!   ledger, not the first listing page), album existence and artist
//!   reachability plus list enrichment come from mirror reads
//!   (`mb_release`, `mb_artist`). A well-formed MBID that fails either
//!   leg is `NotFound`: malformed, non-member and mirror-absent targets
//!   are indistinguishable (no existence oracle), and an absent source
//!   is never reported as saved.
//!
//! Rules (the HTTP layer maps store/auth failures; this module never
//! sees them):
//! - Authentication first: dispatch answers 401 before any store read;
//!   the caller's user id always comes from the verified session or
//!   bearer credential, never from the path or body. No owner id and no
//!   other account's state ever serialize.
//! - Saves are idempotent: [`collection_save_statement`] is one
//!   `INSERT … ON CONFLICT … DO UPDATE … RETURNING saved_at`, so a
//!   replay lands nothing new and returns the ORIGINAL first-save
//!   instant. [`collection_delete_statement`] removes 0..1 rows, so
//!   removing an unsaved or already-removed MBID is the same success.
//!   Removal never re-runs target validation: a target whose membership
//!   lapsed stays removable.
//! - Lists are bounded first pages (the request transport carries no
//!   query field, same as the catalogue slice-1 lists): `LIMIT $n`
//!   bound to [`COLLECTION_PAGE_MAX`], ordered `saved_at DESC, mbid ASC`.
//!   Saved rows are never silently dropped: mirror enrichment degrades
//!   per field to `None`, it is never an error and never a hidden row.
//! - Every write statement carries the ADR 0002 rule-3 account-deletion
//!   fence in-statement (`writer_fence`), so no save or removal lands
//!   after `users.deleting_at` is set. 0 affected rows maps through
//!   [`collection_write_outcome`] to the typed `account_deleting` 409
//!   (or `NotFound` when the fence probe reports the account live), the
//!   same classification shape as `library::fenced_write_outcome`.

use serde::Serialize;

/// Maximum entries one collection page returns. Same bound as the
/// catalogue slice-1 collections; the Worker MUST bind it as the list
/// `LIMIT` so a single response can never grow with the saved set.
pub const COLLECTION_PAGE_MAX: usize = 50;

/// Save kind: one stored `kind` literal per collection. The SQL
/// statements take the literal text; only these two values ever bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    Album,
    Artist,
}

impl SaveKind {
    /// Stored `kind` literal (the `collection_saves` CHECK values).
    pub fn as_str(self) -> &'static str {
        match self {
            SaveKind::Album => "album",
            SaveKind::Artist => "artist",
        }
    }

    /// Parse a stored `kind`. Anything else is `None`: stores fail the
    /// row decode instead of guessing (defense in depth — the CHECK keeps
    /// other values out).
    pub fn parse(kind: &str) -> Option<SaveKind> {
        match kind {
            "album" => Some(SaveKind::Album),
            "artist" => Some(SaveKind::Artist),
            _ => None,
        }
    }
}

/// Normalize one target MBID into the lowercase key every app-side
/// comparison and mirror probe uses, or `None` when the value fails the
/// catalogue shape gate. The HTTP layer answers 404 on `None` before any
/// store work, so malformed and unknown targets stay indistinguishable.
/// Wraps the catalogue's own gate + normalizer so the two surfaces can
/// never drift on what counts as well-formed.
pub fn normalize_mbid(raw: &str) -> Option<String> {
    crate::catalogue::enrichment_key(raw)
}

/// One saved-collection list row as the app-side page read returns it:
/// raw stored MBID plus the first-save instant. Mirror enrichment joins
/// in the Worker entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionListRow {
    pub mbid: String,
    pub saved_at: i64,
}

/// Caller-visible saved-album entry. `saved_at` renders ISO-8601 UTC via
/// [`crate::ledger::iso8601_utc`] in the handler. Mirror-absent
/// enrichment degrades per field to `None` — never a dropped row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedAlbumEntry {
    pub mbid: String,
    pub saved_at: Option<String>,
    pub title: Option<String>,
    pub release_type: Option<String>,
    pub year: Option<i16>,
    pub artist_mbid: Option<String>,
    pub artist: Option<String>,
    /// Own-origin art path (same reserved `/v1/releases/{mbid}/art`
    /// shape the catalogue emits): `Some` exactly when the stored MBID
    /// is well-formed — which is always true for a real save row, so a
    /// poisoned stored value degrades to `None`, never a crafted URL.
    pub art_url: Option<String>,
}

/// Caller-visible followed-artist entry. `followed_at` renders ISO-8601
/// UTC like `saved_at`. Field spellings match the catalogue artist
/// response (`type` renames the reserved word).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FollowedArtistEntry {
    pub mbid: String,
    pub followed_at: Option<String>,
    pub name: Option<String>,
    pub sort_name: Option<String>,
    #[serde(rename = "type")]
    pub artist_type: Option<String>,
    pub country: Option<String>,
}

/// Wire state read for one album MBID: `saved` plus the first-save
/// instant (ISO-8601, `None` when unsaved). A `None` row means the
/// target is not saved — never an existence claim about the target
/// itself (validation ran before the probe).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedState {
    pub saved: bool,
    pub saved_at: Option<String>,
}

/// Wire state read for one artist MBID (same shape, `followed` key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FollowedState {
    pub followed: bool,
    pub followed_at: Option<String>,
}

/// Assemble one saved-album entry from its app row plus mirror release
/// enrichment (`None` when the mirror has no row — degrades per field,
/// never drops the save).
pub fn saved_album_entry(
    row: &CollectionListRow,
    hit: Option<&crate::catalogue::ReleaseDbRow>,
) -> SavedAlbumEntry {
    SavedAlbumEntry {
        mbid: row.mbid.clone(),
        saved_at: Some(crate::ledger::iso8601_utc(row.saved_at)),
        title: hit.map(|hit| hit.title.clone()),
        release_type: hit.and_then(|hit| hit.release_type.clone()),
        year: hit.and_then(|hit| hit.year),
        artist_mbid: hit.and_then(|hit| hit.artist_mbid.clone()),
        artist: hit.and_then(|hit| hit.artist_name.clone()),
        art_url: crate::catalogue::enrichment_key(&row.mbid)
            .map(|mbid| crate::catalogue::release_art_path(&mbid)),
    }
}

/// Assemble one followed-artist entry from its app row plus mirror
/// artist enrichment (`None` degrades per field like above).
pub fn followed_artist_entry(
    row: &CollectionListRow,
    hit: Option<&crate::catalogue::ArtistDbRow>,
) -> FollowedArtistEntry {
    FollowedArtistEntry {
        mbid: row.mbid.clone(),
        followed_at: Some(crate::ledger::iso8601_utc(row.saved_at)),
        name: hit.map(|hit| hit.name.clone()),
        sort_name: hit.map(|hit| hit.sort_name.clone()),
        artist_type: hit.and_then(|hit| hit.artist_type.clone()),
        country: hit.and_then(|hit| hit.country.clone()),
    }
}

/// Join one saved page with its mirror release enrichment. Page order is
/// preserved; a release the mirror does not carry enriches to all-`None`
/// display fields (unknown stays NULL, never an error, never a hidden
/// row, never a tag fill).
pub fn assemble_saved_albums(
    page: &[CollectionListRow],
    hits: &[crate::catalogue::ReleaseDbRow],
) -> Vec<SavedAlbumEntry> {
    use std::collections::HashMap;
    let by_mbid: HashMap<String, &crate::catalogue::ReleaseDbRow> = hits
        .iter()
        .filter_map(|hit| crate::catalogue::enrichment_key(&hit.mbid).map(|key| (key, hit)))
        .collect();
    page.iter()
        .map(|row| {
            let hit = crate::catalogue::enrichment_key(&row.mbid)
                .and_then(|key| by_mbid.get(&key).copied());
            saved_album_entry(row, hit)
        })
        .collect()
}

/// Join one followed page with its mirror artist enrichment (same
/// degrade-per-field discipline as [`assemble_saved_albums`]).
pub fn assemble_followed_artists(
    page: &[CollectionListRow],
    hits: &[crate::catalogue::ArtistDbRow],
) -> Vec<FollowedArtistEntry> {
    use std::collections::HashMap;
    let by_mbid: HashMap<String, &crate::catalogue::ArtistDbRow> = hits
        .iter()
        .filter_map(|hit| crate::catalogue::enrichment_key(&hit.mbid).map(|key| (key, hit)))
        .collect();
    page.iter()
        .map(|row| {
            let hit = crate::catalogue::enrichment_key(&row.mbid)
                .and_then(|key| by_mbid.get(&key).copied());
            followed_artist_entry(row, hit)
        })
        .collect()
}

/// Wire state object for one save row: `saved: true` plus the original
/// instant. Rendered through [`crate::ledger::iso8601_utc`].
pub fn saved_state(saved_at: Option<i64>) -> SavedState {
    SavedState {
        saved: saved_at.is_some(),
        saved_at: saved_at.map(crate::ledger::iso8601_utc),
    }
}

/// Wire state object for one follow row (same semantics as
/// [`saved_state`]).
pub fn followed_state(saved_at: Option<i64>) -> FollowedState {
    FollowedState {
        followed: saved_at.is_some(),
        followed_at: saved_at.map(crate::ledger::iso8601_utc),
    }
}

/// Outcome of one fenced save/remove write, classified like the library
/// module's [`crate::library::fenced_write_outcome`]: a landed write is
/// `Written`; 0 affected rows is `AccountDeleting` when the probe says
/// the fence closed (the typed 409) and `NotFound` when the user row
/// itself is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionWriteOutcome {
    Written,
    AccountDeleting,
    NotFound,
}

/// Map a fenced write's affected-row count to its outcome. `written`
/// means the statement produced a row (the save's `RETURNING saved_at`
/// for upserts, or `affected_rows > 0` for deletes); `account_deleting`
/// is the store's fence-state probe result — the handler re-reads it
/// only when the write affected zero rows.
pub fn collection_write_outcome(written: bool, account_deleting: bool) -> CollectionWriteOutcome {
    use crate::account_deletion::{classify_fenced_write, FencedWriteOutcome};
    match classify_fenced_write(u64::from(written), Some(account_deleting)) {
        FencedWriteOutcome::Written => CollectionWriteOutcome::Written,
        FencedWriteOutcome::AccountDeleting => CollectionWriteOutcome::AccountDeleting,
        FencedWriteOutcome::NoRow => CollectionWriteOutcome::NotFound,
    }
}

// --- Statements: parameterized, caller-scoped --------------------------------
//
// The Worker binds positional parameters only; no value is ever
// interpolated into SQL. Owner scoping lives entirely in the statements:
// every read filters `user_id = $n` on the caller's own id, and every
// write carries the in-statement `writer_fence` (`SELECT id FROM users
// WHERE id = $n AND deleting_at IS NULL FOR SHARE`) so no write lands
// after account deletion is scheduled. `mbid` binds as TEXT everywhere —
// no `::uuid` cast on the app side.

/// Saved-state probe for one target. Parameters: user id ($1), kind
/// literal ($2), target MBID ($3, pre-normalized via [`normalize_mbid`]).
/// One row is saved, zero is unsaved — both reach this point only after
/// target validation, so the probe never decides whether the target
/// exists.
pub fn collection_state_statement() -> &'static str {
    "SELECT saved_at FROM collection_saves WHERE user_id = $1 AND kind = $2 AND lower(mbid) = $3"
}

/// Bounded saved-collection page, newest first with the MBID as a stable
/// tiebreak. Parameters: user id ($1), kind literal ($2), row bound
/// ($3, [`COLLECTION_PAGE_MAX`]). App-database only.
pub fn collection_list_statement() -> &'static str {
    "SELECT mbid, saved_at FROM collection_saves WHERE user_id = $1 AND kind = $2 \
     ORDER BY saved_at DESC, mbid ASC LIMIT $3"
}

/// Idempotent save, fenced on the caller's user row (ADR 0002 rule 3).
/// Parameters: user id ($1), kind literal ($2), target MBID ($3,
/// normalized), save instant unix seconds ($4). The `DO UPDATE`
/// self-assignment keeps the conflicted row unchanged — the
/// `RETURNING saved_at` projects the ORIGINAL first-save instant on
/// replay, so a repeated save is a clean no-op that reports the same
/// state instead of refreshing it. One returned row means written-or-
/// replayed; zero rows maps through [`collection_write_outcome`]
/// (the fence closed, or the user row is gone).
pub fn collection_save_statement() -> &'static str {
    "INSERT INTO collection_saves (user_id, kind, mbid, saved_at) \
     SELECT $1, $2, $3, $4 \
     FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
     ON CONFLICT (user_id, kind, mbid) DO UPDATE SET saved_at = collection_saves.saved_at \
     RETURNING saved_at"
}

/// Idempotent removal, fenced like [`collection_save_statement`].
/// Parameters: user id ($1), kind literal ($2), target MBID ($3,
/// normalized — `lower(mbid)` keeps removal case-insensitive like the
/// state probe, so an uppercase-stored legacy value is still removable).
/// 0..1 affected rows: removing an unsaved MBID is the same success and
/// never an error.
pub fn collection_delete_statement() -> &'static str {
    "DELETE FROM collection_saves \
     USING (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
     WHERE collection_saves.user_id = $1 AND collection_saves.kind = $2 \
     AND lower(collection_saves.mbid) = $3"
}

/// Mirror artist-existence probe for target validation and (joined by
/// MBID array) list enrichment. Parameters: well-formed artist MBIDs
/// ($1, `text[]` cast to `uuid[]` — the Worker binds only
/// [`normalize_mbid`] output, so the cast cannot fail), row bound ($2,
/// [`COLLECTION_PAGE_MAX`] for lists, 1 for validation).
/// Mirror-database only (`mb_artist`, migrations 0001/0016). Ordering is
/// MBID ascending for a stable validation read; list assembly preserves
/// the app page order and joins hits by MBID, so this order never
/// leaks into the response.
pub fn artists_by_mbid_statement() -> &'static str {
    "SELECT mbid::text AS mbid, name, sort_name, type, country::text AS country \
     FROM mb_artist WHERE mbid = ANY($1::uuid[]) ORDER BY mbid ASC LIMIT $2"
}

/// Mirror release-existence/enrichment probe. Parameters and join
/// discipline match [`artists_by_mbid_statement`]. Selection mirrors the
/// catalogue [`crate::catalogue::ReleaseDbRow`] projection (artist
/// included), so list entries carry the same fields the release
/// collection serves.
pub fn releases_by_mbid_statement() -> &'static str {
    "SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year, \
     r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
     FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
     WHERE r.mbid = ANY($1::uuid[]) ORDER BY r.mbid ASC LIMIT $2"
}

/// Mirror artist-membership probe for artist target validation: one
/// row when the artist exists AND owns at least one of the caller's
/// member release MBIDs ($1, member releases from
/// [`crate::catalogue::member_release_mbids`], the whole uncapped
/// ledger), the artist MBID ($2). The member array can be large, so
/// the artist's own row drives the lookup (primary-key probe plus an
/// `EXISTS` over its releases) instead of scanning the member list.
/// `EXISTS` also makes the artist's own `mbid = $2` release presence
/// unnecessary — `mb_release.artist_mbid` is the FK that proves the
/// artist row exists upstream of this read.
pub fn artist_membership_statement() -> &'static str {
    "SELECT a.mbid::text AS mbid FROM mb_artist a \
     WHERE a.mbid = $2::text::uuid \
     AND EXISTS (SELECT 1 FROM mb_release r \
     WHERE r.artist_mbid = a.mbid AND r.mbid = ANY($1::uuid[])) \
     LIMIT 1"
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_MBID: &str = "12345678-1234-1234-1234-1234567890ab";
    const GOOD_MBID_UPPER: &str = "12345678-1234-1234-1234-1234567890AB";
    const GOOD_MBID_2: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    fn release_hit(mbid: &str) -> crate::catalogue::ReleaseDbRow {
        crate::catalogue::ReleaseDbRow {
            mbid: mbid.to_string(),
            title: "Power, Corruption & Lies".to_string(),
            release_type: Some("Album".to_string()),
            year: Some(1983),
            artist_mbid: Some(GOOD_MBID_2.to_string()),
            artist_name: Some("New Order".to_string()),
        }
    }

    fn artist_hit(mbid: &str) -> crate::catalogue::ArtistDbRow {
        crate::catalogue::ArtistDbRow {
            mbid: mbid.to_string(),
            name: "New Order".to_string(),
            sort_name: "New Order".to_string(),
            artist_type: Some("Group".to_string()),
            country: Some("GB".to_string()),
        }
    }

    #[test]
    fn save_kind_round_trips_the_two_literals_only() {
        assert_eq!(SaveKind::Album.as_str(), "album");
        assert_eq!(SaveKind::Artist.as_str(), "artist");
        assert_eq!(SaveKind::parse("album"), Some(SaveKind::Album));
        assert_eq!(SaveKind::parse("artist"), Some(SaveKind::Artist));
        for bad in ["", "Album", " track", "release", "ALBUM"] {
            assert_eq!(SaveKind::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn normalize_mbid_lowercases_and_rejects_malformed() {
        assert_eq!(normalize_mbid(GOOD_MBID), Some(GOOD_MBID.to_string()));
        assert_eq!(normalize_mbid(GOOD_MBID_UPPER), Some(GOOD_MBID.to_string()));
        for bad in ["", "job-1", "not-an-mbid", "../x", &GOOD_MBID[..35]] {
            assert_eq!(normalize_mbid(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn page_bound_is_the_catalogue_collection_cap() {
        assert_eq!(COLLECTION_PAGE_MAX, 50);
        assert!(collection_list_statement().ends_with("LIMIT $3"));
    }

    #[test]
    fn saved_state_renders_first_instant_or_unsaved() {
        let saved = saved_state(Some(1_700_000_000));
        assert!(saved.saved);
        assert_eq!(saved.saved_at.as_deref(), Some("2023-11-14T22:13:20Z"));
        let unsaved = saved_state(None);
        assert!(!unsaved.saved);
        assert_eq!(unsaved.saved_at, None);
        let followed = followed_state(Some(1_700_000_000));
        assert!(followed.followed);
        assert_eq!(
            followed.followed_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }

    #[test]
    fn entries_degrade_per_field_never_drop_rows() {
        let row = CollectionListRow {
            mbid: GOOD_MBID.to_string(),
            saved_at: 1_700_000_000,
        };
        // Mirror hit: full enrichment.
        let album = saved_album_entry(&row, Some(&release_hit(GOOD_MBID)));
        assert_eq!(album.mbid, GOOD_MBID);
        assert_eq!(album.title.as_deref(), Some("Power, Corruption & Lies"));
        assert_eq!(album.artist.as_deref(), Some("New Order"));
        assert_eq!(
            album.art_url.as_deref(),
            Some(format!("/v1/releases/{GOOD_MBID}/art").as_str())
        );
        // Mirror miss: identity + instant survive, enrichment nulls.
        let missing = saved_album_entry(&row, None);
        assert_eq!(missing.mbid, GOOD_MBID);
        assert_eq!(missing.saved_at, album.saved_at);
        assert_eq!(missing.title, None);
        assert_eq!(missing.artist, None);
        // Poisoned stored value: never a crafted art URL.
        let poisoned = CollectionListRow {
            mbid: "../../etc/passwd".to_string(),
            saved_at: 1,
        };
        assert_eq!(saved_album_entry(&poisoned, None).art_url, None);

        let artist = followed_artist_entry(&row, Some(&artist_hit(GOOD_MBID)));
        assert_eq!(artist.mbid, GOOD_MBID);
        assert_eq!(artist.name.as_deref(), Some("New Order"));
        let missing_artist = followed_artist_entry(&row, None);
        assert_eq!(missing_artist.mbid, GOOD_MBID);
        assert_eq!(missing_artist.name, None);
    }

    #[test]
    fn assembly_preserves_page_order_and_joins_by_normalized_mbid() {
        let page = vec![
            CollectionListRow {
                mbid: GOOD_MBID_2.to_string(),
                saved_at: 200,
            },
            CollectionListRow {
                mbid: GOOD_MBID_UPPER.to_string(),
                saved_at: 100,
            },
        ];
        // Mirror hits arrive unsorted and uppercase-normalized inputs
        // join on the same lowercase key.
        let albums =
            assemble_saved_albums(&page, &[release_hit(GOOD_MBID), release_hit(GOOD_MBID_2)]);
        assert_eq!(
            albums.iter().map(|e| e.mbid.as_str()).collect::<Vec<_>>(),
            vec![GOOD_MBID_2, GOOD_MBID_UPPER]
        );
        assert!(albums.iter().all(|e| e.title.is_some()));
        // Missing hit keeps the row with null enrichment.
        let partial = assemble_followed_artists(&page, &[artist_hit(GOOD_MBID_2)]);
        assert_eq!(partial.len(), 2);
        assert!(partial[0].name.is_some());
        assert_eq!(partial[1].name, None);
        assert_eq!(partial[1].mbid, GOOD_MBID_UPPER);
    }

    #[test]
    fn write_outcome_maps_through_the_deletion_fence() {
        assert_eq!(
            collection_write_outcome(true, true),
            CollectionWriteOutcome::Written
        );
        assert_eq!(
            collection_write_outcome(true, false),
            CollectionWriteOutcome::Written
        );
        assert_eq!(
            collection_write_outcome(false, true),
            CollectionWriteOutcome::AccountDeleting
        );
        assert_eq!(
            collection_write_outcome(false, false),
            CollectionWriteOutcome::NotFound
        );
    }

    #[test]
    fn statements_are_owner_scoped_fenced_and_cast_free() {
        // Every app statement scopes on the caller's bound id.
        for sql in [
            collection_state_statement(),
            collection_list_statement(),
            collection_save_statement(),
            collection_delete_statement(),
        ] {
            assert!(sql.contains("user_id = $1"), "{sql}");
            assert!(!sql.contains("::uuid"), "{sql}");
            assert!(
                sql.contains("lower(mbid)") || sql.contains("mbid, saved_at"),
                "{sql}"
            );
        }
        // Writes carry the in-statement account-deletion fence; reads do
        // not (a fenced read would queue behind the delete it is not).
        for sql in [collection_save_statement(), collection_delete_statement()] {
            assert!(
                sql.contains(
                    "SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE"
                ),
                "{sql}"
            );
        }
        for sql in [collection_state_statement(), collection_list_statement()] {
            assert!(!sql.contains("FOR SHARE"), "{sql}");
            assert!(!sql.contains("FROM users"), "{sql}");
        }
        // Save replays keep the first instant and return it.
        let save = collection_save_statement();
        assert!(
            save.contains(
                "ON CONFLICT (user_id, kind, mbid) DO UPDATE SET saved_at = collection_saves.saved_at"
            ),
            "{save}"
        );
        assert!(save.contains("RETURNING saved_at"), "{save}");
        // Mirror probes bind only normalized arrays and stay bounded.
        for sql in [artists_by_mbid_statement(), releases_by_mbid_statement()] {
            assert!(sql.contains("ANY($1::uuid[])"), "{sql}");
            assert!(sql.ends_with("LIMIT $2"), "{sql}");
        }
        let membership = artist_membership_statement();
        assert!(
            membership.contains("a.mbid = $2::text::uuid"),
            "{membership}"
        );
        assert!(membership.contains("ANY($1::uuid[])"), "{membership}");
        assert!(
            membership.contains("r.artist_mbid = a.mbid"),
            "{membership}"
        );
        // No statement takes a user id the mirror side could leak: the
        // mirror probes bind MBID values only.
        for sql in [
            artists_by_mbid_statement(),
            releases_by_mbid_statement(),
            artist_membership_statement(),
        ] {
            assert!(!sql.contains("user_id"), "{sql}");
            assert!(!sql.contains("collection_saves"), "{sql}");
        }
        // Table pins: app statements hit collection_saves; mirror probes
        // hit the right mirror tables.
        for sql in [
            collection_state_statement(),
            collection_list_statement(),
            collection_save_statement(),
            collection_delete_statement(),
        ] {
            assert!(sql.contains("collection_saves"), "{sql}");
            assert!(!sql.contains("mb_"), "{sql}");
        }
        assert!(artists_by_mbid_statement().contains("FROM mb_artist"), "");
        assert!(
            releases_by_mbid_statement().contains("FROM mb_release r"),
            ""
        );
        assert!(
            artist_membership_statement().contains("FROM mb_artist a"),
            ""
        );
    }
}
