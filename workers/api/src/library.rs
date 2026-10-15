//! Genre + playlist library: opaque ids, owner-scoped authz, no internal-id leaks.
//!
//! Native core for the genre/playlist slice. Pure functions over row slices
//! plus the parameterized statements the Worker entry binds against
//! PlanetScale Postgres via Hyperdrive — never string-interpolated SQL.
//!
//! Rules (the HTTP layer maps [`LibraryError::http_status`] to the response
//! status, so unauthenticated `/api/genres` + `/api/playlists` return 401):
//! - Authentication first: a missing caller is [`LibraryError::Unauthorized`]
//!   even when the id does not exist (no existence oracle for anonymous calls).
//! - Visibility: a caller sees their own rows plus public rows. Reads of
//!   invisible rows return [`LibraryError::NotFound`], never Forbidden, so
//!   private rows of other users are not enumerable.
//! - Mutations: invisible rows are [`LibraryError::NotFound`]; visible rows
//!   owned by someone else are [`LibraryError::Forbidden`]. The Worker
//!   decides this from [`MutationLookup`] (an `is_owner` boolean), never
//!   from the owner id itself, and maps 0 affected UPDATE/DELETE rows to
//!   [`LibraryError::NotFound`] (the row vanished between lookup and write;
//!   the owner predicate already makes cross-owner writes inexpressible).
//! - Lists are keyset pages ordered by the opaque public id descending
//!   (`ORDER BY public_id COLLATE "C" DESC`), capped at
//!   [`MAX_PAGE_SIZE`]. The order is by the random 128-bit handle, NOT
//!   newest-first: a newly created row sorts by its id and may land on any
//!   page. The public-id cursor is compared directly with the ordering key
//!   and is independent of row existence, so a deleted or newly private
//!   boundary row cannot truncate later pages. A well-formed cursor above
//!   every visible row replays from the top in both the in-memory pager and
//!   the list SQL (`public_id < $3` matches everything); the replay is
//!   harmless because visibility still filters every returned row.
//!   Playlist lists omit `items` (single reads return them).
//! - Responses carry only the opaque `public_id` as `id` plus display fields.
//!   Internal row ids and `owner_user_id` never serialize. The authorizers
//!   below return intents (validated parameters), never mutated rows.

use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Maximum display-name length (chars).
pub const MAX_NAME_LEN: usize = 120;
/// Maximum items on a playlist.
pub const MAX_ITEMS: usize = 500;
/// Maximum length of a single playlist item reference (chars).
pub const MAX_ITEM_LEN: usize = 128;
/// Maximum rows one list page returns. The Worker MUST bind a LIMIT clamped
/// through [`clamp_page_size`]; the list statements take it as `$2` so a
/// single response can never grow with the total public library.
pub const MAX_PAGE_SIZE: usize = 50;

/// Clamp a caller-supplied page size into `1..=MAX_PAGE_SIZE`.
pub fn clamp_page_size(requested: usize) -> usize {
    requested.clamp(1, MAX_PAGE_SIZE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryError {
    /// Missing caller: the HTTP layer returns 401.
    Unauthorized,
    /// Visible row owned by someone else: the HTTP layer returns 403.
    Forbidden,
    /// Invisible or unknown id: the HTTP layer returns 404.
    NotFound,
    /// Shape violation (blank/overlong name or items): HTTP layer returns 422.
    Invalid,
}

impl LibraryError {
    pub fn http_status(&self) -> u16 {
        match self {
            LibraryError::Unauthorized => 401,
            LibraryError::Forbidden => 403,
            LibraryError::NotFound => 404,
            LibraryError::Invalid => 422,
        }
    }
}

impl std::fmt::Display for LibraryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibraryError::Unauthorized => write!(f, "authentication required"),
            LibraryError::Forbidden => write!(f, "not the owner"),
            LibraryError::NotFound => write!(f, "not found"),
            LibraryError::Invalid => write!(f, "invalid genre or playlist"),
        }
    }
}

/// Random 128-bit opaque public id (base64url, 22 chars, via the shared
/// [`crate::b64`] helper). Never sequential, never derived from the internal
/// row id. Sorts arbitrarily: list order is by this handle, not by recency
/// (see the list pages).
pub fn new_public_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    crate::b64::encode(&bytes)
}

fn require_caller(caller: Option<&str>) -> Result<&str, LibraryError> {
    match caller {
        Some(user) if !user.is_empty() => Ok(user),
        _ => Err(LibraryError::Unauthorized),
    }
}

fn check_name(name: &str) -> Result<String, LibraryError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME_LEN {
        return Err(LibraryError::Invalid);
    }
    Ok(trimmed.to_string())
}

fn check_items(items: Vec<String>) -> Result<Vec<String>, LibraryError> {
    if items.len() > MAX_ITEMS {
        return Err(LibraryError::Invalid);
    }
    items
        .into_iter()
        .map(|item| {
            let trimmed = item.trim();
            if trimmed.is_empty() || trimmed.chars().count() > MAX_ITEM_LEN {
                return Err(LibraryError::Invalid);
            }
            Ok(trimmed.to_string())
        })
        .collect()
}

// --- Stored rows (never serialized to clients) --------------------------------

/// Stored genre row. No `Serialize`: only [`GenreResponse`] leaves the server.
#[derive(Debug, Clone)]
pub struct GenreRow {
    /// Internal Postgres id. Never exposed: responses carry `public_id`.
    pub internal_id: i64,
    pub public_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub is_public: bool,
}

/// Stored playlist row. No `Serialize`: only [`PlaylistResponse`] leaves.
#[derive(Debug, Clone)]
pub struct PlaylistRow {
    /// Internal Postgres id. Never exposed: responses carry `public_id`.
    pub internal_id: i64,
    pub public_id: String,
    pub owner_user_id: String,
    pub name: String,
    /// Opaque track references (never validated against the catalog here).
    pub items: Vec<String>,
    pub is_public: bool,
}

// --- Responses (the only shapes clients see) ----------------------------------

/// Public genre shape: opaque id + display fields. No owner, no internal id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenreResponse {
    pub id: String,
    pub name: String,
    pub is_public: bool,
}

/// Public playlist shape: opaque id + display fields. No owner, no internal id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistResponse {
    pub id: String,
    pub name: String,
    pub items: Vec<String>,
    pub is_public: bool,
}

/// Playlist list shape: like [`PlaylistResponse`] but without `items`.
/// Lists never carry track references; single reads return them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    pub id: String,
    pub name: String,
    pub is_public: bool,
}

fn genre_response(row: &GenreRow) -> GenreResponse {
    GenreResponse {
        id: row.public_id.clone(),
        name: row.name.clone(),
        is_public: row.is_public,
    }
}

fn playlist_response(row: &PlaylistRow) -> PlaylistResponse {
    PlaylistResponse {
        id: row.public_id.clone(),
        name: row.name.clone(),
        items: row.items.clone(),
        is_public: row.is_public,
    }
}

fn playlist_summary(row: &PlaylistRow) -> PlaylistSummary {
    PlaylistSummary {
        id: row.public_id.clone(),
        name: row.name.clone(),
        is_public: row.is_public,
    }
}

/// Maximum public-id length accepted inside a cursor.
pub const MAX_CURSOR_PUBLIC_ID_LEN: usize = 128;
/// Maximum encoded cursor length (covers a 128-byte id, base64url no-pad).
const MAX_CURSOR_LEN: usize = 192;

/// Opaque keyset cursor: base64url (no padding, via the shared
/// [`crate::b64`] helper) of the UTF-8 bytes of the last row's public `id`
/// on the page. Clients pass it back as `after`; they must never parse or
/// forge it.
///
/// The cursor carries only the already-public opaque id — the same value
/// returned as `id` in list responses. Lists compare it directly with the
/// public-id ordering key, so paging remains stable if the boundary row is
/// deleted or becomes private. Visibility always applies to returned rows.
/// A cursor above every visible row replays from the top in both the mirror
/// and the SQL (`public_id < $3` matches everything); that replay is harmless
/// because every returned row is still visibility-filtered.
pub fn page_cursor(last_public_id: &str) -> String {
    crate::b64::encode(last_public_id.as_bytes())
}

/// Decode a cursor from [`page_cursor`] into the public id it wraps.
/// Garbage is [`LibraryError::Invalid`] (a client shape error, surfaced
/// as 422). Decoding is strict via [`crate::b64::decode`]: non-canonical
/// trailing bits are rejected, so two different strings never decode to the
/// same id. A well-formed cursor continues by keyset comparison downstream —
/// an above-max cursor replays from the top in both the mirror and the SQL,
/// never a leak and never a truncation.
pub fn parse_page_cursor(cursor: &str) -> Result<String, LibraryError> {
    if cursor.is_empty() || cursor.len() > MAX_CURSOR_LEN {
        return Err(LibraryError::Invalid);
    }
    let bytes = crate::b64::decode(cursor).map_err(|_| LibraryError::Invalid)?;
    let id = std::str::from_utf8(&bytes).map_err(|_| LibraryError::Invalid)?;
    if id.is_empty() || id.len() > MAX_CURSOR_PUBLIC_ID_LEN {
        return Err(LibraryError::Invalid);
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(LibraryError::Invalid);
    }
    Ok(id.to_string())
}

fn genre_visible(caller: &str, row: &GenreRow) -> bool {
    row.is_public || row.owner_user_id == caller
}

fn playlist_visible(caller: &str, row: &PlaylistRow) -> bool {
    row.is_public || row.owner_user_id == caller
}

// --- Reads --------------------------------------------------------------------

/// List caller-visible genres: own rows plus public rows.
///
/// In-memory mirror of the visibility rule. The Worker MUST use
/// [`list_genres_page`] (backed by [`genre_list_statement`]) so responses
/// stay bounded; this helper exists for rule tests over small slices.
pub fn list_genres(
    caller: Option<&str>,
    rows: &[GenreRow],
) -> Result<Vec<GenreResponse>, LibraryError> {
    let caller = require_caller(caller)?;
    Ok(rows
        .iter()
        .filter(|row| genre_visible(caller, row))
        .map(genre_response)
        .collect())
}

/// List caller-visible playlists (summaries: no `items`).
///
/// Mirrors [`genre_list_statement`] visibility for rule tests; the Worker
/// MUST use [`list_playlists_page`]. Track references come only from
/// [`get_playlist`].
pub fn list_playlists(
    caller: Option<&str>,
    rows: &[PlaylistRow],
) -> Result<Vec<PlaylistSummary>, LibraryError> {
    let caller = require_caller(caller)?;
    Ok(rows
        .iter()
        .filter(|row| playlist_visible(caller, row))
        .map(playlist_summary)
        .collect())
}

/// One bounded genre page, ordered by the opaque public id descending
/// (arbitrary order, NOT newest-first: new rows sort by id and may land on
/// any page). `limit` is clamped to [`MAX_PAGE_SIZE`]; `after` continues
/// from a [`page_cursor`] wrapping the last row's public `id`. The cursor is
/// compared directly with the ordering key — mirroring
/// [`genre_list_statement`] (`public_id < $3`) — so it is stable even if the
/// boundary row is deleted or becomes private. A cursor above every visible
/// row replays from the top in both the mirror and the SQL; every returned
/// row is still visibility-filtered.
pub fn list_genres_page(
    caller: Option<&str>,
    rows: &[GenreRow],
    limit: usize,
    after: Option<&str>,
) -> Result<Vec<GenreResponse>, LibraryError> {
    let caller = require_caller(caller)?;
    let limit = clamp_page_size(limit);
    let mut visible: Vec<&GenreRow> = rows
        .iter()
        .filter(|row| genre_visible(caller, row))
        .collect();
    visible.sort_by(|a, b| b.public_id.cmp(&a.public_id));
    if let Some(cursor) = after {
        let cursor_id = parse_page_cursor(cursor)?;
        // Direct keyset comparison, mirroring the list SQL. No existence
        // lookup: a deleted or newly hidden boundary row cannot truncate
        // later pages. An above-max cursor matches everything and replays
        // from the top, exactly like the SQL.
        visible.retain(|row| row.public_id < cursor_id);
    }
    visible.truncate(limit);
    Ok(visible.iter().map(|row| genre_response(row)).collect())
}

/// One bounded playlist-summary page, ordered by the opaque public id
/// descending (arbitrary order, NOT newest-first).
/// See [`list_genres_page`]: the cursor is compared directly with the public
/// ordering key — mirroring [`playlist_list_statement`] — so continuation
/// does not depend on the boundary row existing. An above-max cursor replays
/// from the top in both the mirror and the SQL; visibility still filters
/// every returned row.
pub fn list_playlists_page(
    caller: Option<&str>,
    rows: &[PlaylistRow],
    limit: usize,
    after: Option<&str>,
) -> Result<Vec<PlaylistSummary>, LibraryError> {
    let caller = require_caller(caller)?;
    let limit = clamp_page_size(limit);
    let mut visible: Vec<&PlaylistRow> = rows
        .iter()
        .filter(|row| playlist_visible(caller, row))
        .collect();
    visible.sort_by(|a, b| b.public_id.cmp(&a.public_id));
    if let Some(cursor) = after {
        let cursor_id = parse_page_cursor(cursor)?;
        // Direct keyset comparison, mirroring the list SQL. No existence
        // lookup: a deleted or newly hidden boundary row cannot truncate
        // later pages. An above-max cursor matches everything and replays
        // from the top, exactly like the SQL.
        visible.retain(|row| row.public_id < cursor_id);
    }
    visible.truncate(limit);
    Ok(visible.iter().map(|row| playlist_summary(row)).collect())
}

/// Read one genre. Invisible ids are NotFound (no existence oracle).
pub fn get_genre(
    caller: Option<&str>,
    rows: &[GenreRow],
    public_id: &str,
) -> Result<GenreResponse, LibraryError> {
    let caller = require_caller(caller)?;
    rows.iter()
        .filter(|row| genre_visible(caller, row))
        .find(|row| row.public_id == public_id)
        .map(genre_response)
        .ok_or(LibraryError::NotFound)
}

/// Read one playlist. Invisible ids are NotFound (no existence oracle).
pub fn get_playlist(
    caller: Option<&str>,
    rows: &[PlaylistRow],
    public_id: &str,
) -> Result<PlaylistResponse, LibraryError> {
    let caller = require_caller(caller)?;
    rows.iter()
        .filter(|row| playlist_visible(caller, row))
        .find(|row| row.public_id == public_id)
        .map(playlist_response)
        .ok_or(LibraryError::NotFound)
}

// --- Write intents (validated parameters for the statements below) -------------

/// Validated genre insert. The Worker binds it to [`genre_insert_statement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGenre {
    pub public_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub is_public: bool,
    pub created_at: u64,
}

/// Validated playlist insert. The Worker binds it to [`playlist_insert_statement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlaylist {
    pub public_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub items: Vec<String>,
    pub is_public: bool,
    pub created_at: u64,
}

/// Validated genre update. The Worker binds it to [`genre_update_statement`];
/// the `owner_user_id` predicate keeps cross-owner writes inexpressible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreUpdate {
    pub public_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub is_public: bool,
}

/// Validated playlist update. The Worker binds it to [`playlist_update_statement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistUpdate {
    pub public_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub items: Vec<String>,
    pub is_public: bool,
}

/// Validated genre delete. The Worker binds it to [`genre_delete_statement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreDelete {
    pub public_id: String,
    pub owner_user_id: String,
}

/// Validated playlist delete. The Worker binds it to [`playlist_delete_statement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistDelete {
    pub public_id: String,
    pub owner_user_id: String,
}

pub fn prepare_genre_insert(
    caller: Option<&str>,
    name: &str,
    is_public: bool,
    now: u64,
) -> Result<NewGenre, LibraryError> {
    let caller = require_caller(caller)?;
    Ok(NewGenre {
        public_id: new_public_id(),
        owner_user_id: caller.to_string(),
        name: check_name(name)?,
        is_public,
        created_at: now,
    })
}

pub fn prepare_playlist_insert(
    caller: Option<&str>,
    name: &str,
    items: Vec<String>,
    is_public: bool,
    now: u64,
) -> Result<NewPlaylist, LibraryError> {
    let caller = require_caller(caller)?;
    Ok(NewPlaylist {
        public_id: new_public_id(),
        owner_user_id: caller.to_string(),
        name: check_name(name)?,
        items: check_items(items)?,
        is_public,
        created_at: now,
    })
}

fn find_visible_genre<'a>(
    caller: &str,
    rows: &'a [GenreRow],
    public_id: &str,
) -> Result<&'a GenreRow, LibraryError> {
    rows.iter()
        .filter(|row| genre_visible(caller, row))
        .find(|row| row.public_id == public_id)
        .ok_or(LibraryError::NotFound)
}

fn find_visible_playlist<'a>(
    caller: &str,
    rows: &'a [PlaylistRow],
    public_id: &str,
) -> Result<&'a PlaylistRow, LibraryError> {
    rows.iter()
        .filter(|row| playlist_visible(caller, row))
        .find(|row| row.public_id == public_id)
        .ok_or(LibraryError::NotFound)
}

fn require_genre_owner(caller: &str, row: &GenreRow) -> Result<(), LibraryError> {
    if row.owner_user_id == caller {
        Ok(())
    } else {
        Err(LibraryError::Forbidden)
    }
}

fn require_playlist_owner(caller: &str, row: &PlaylistRow) -> Result<(), LibraryError> {
    if row.owner_user_id == caller {
        Ok(())
    } else {
        Err(LibraryError::Forbidden)
    }
}

/// Mutation lookup: the only owner-derived shape the Worker reads before a
/// mutation. `is_owner` answers the 403-vs-404 question while the owner id
/// itself never leaves SQL (see [`genre_mutation_lookup_statement`]).
/// `None` means the lookup returned no row: the id is invisible to the
/// caller ([`LibraryError::NotFound`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationLookup {
    pub public_id: String,
    pub is_owner: bool,
}

fn require_lookup_owner(lookup: Option<MutationLookup>) -> Result<String, LibraryError> {
    let lookup = lookup.ok_or(LibraryError::NotFound)?;
    if lookup.is_owner {
        Ok(lookup.public_id)
    } else {
        Err(LibraryError::Forbidden)
    }
}

/// Authorize a genre update from a [`genre_mutation_lookup_statement`] row.
/// The Worker MUST fetch the lookup for the target id first; authentication
/// still runs before the existence check.
pub fn authorize_genre_update_from_lookup(
    caller: Option<&str>,
    lookup: Option<MutationLookup>,
    name: &str,
    is_public: bool,
) -> Result<GenreUpdate, LibraryError> {
    let caller = require_caller(caller)?;
    let public_id = require_lookup_owner(lookup)?;
    Ok(GenreUpdate {
        public_id,
        owner_user_id: caller.to_string(),
        name: check_name(name)?,
        is_public,
    })
}

/// Authorize a genre delete from a [`genre_mutation_lookup_statement`] row.
pub fn authorize_genre_delete_from_lookup(
    caller: Option<&str>,
    lookup: Option<MutationLookup>,
) -> Result<GenreDelete, LibraryError> {
    let caller = require_caller(caller)?;
    let public_id = require_lookup_owner(lookup)?;
    Ok(GenreDelete {
        public_id,
        owner_user_id: caller.to_string(),
    })
}

/// Authorize a playlist update from a [`playlist_mutation_lookup_statement`]
/// row.
pub fn authorize_playlist_update_from_lookup(
    caller: Option<&str>,
    lookup: Option<MutationLookup>,
    name: &str,
    items: Vec<String>,
    is_public: bool,
) -> Result<PlaylistUpdate, LibraryError> {
    let caller = require_caller(caller)?;
    let public_id = require_lookup_owner(lookup)?;
    Ok(PlaylistUpdate {
        public_id,
        owner_user_id: caller.to_string(),
        name: check_name(name)?,
        items: check_items(items)?,
        is_public,
    })
}

/// Authorize a playlist delete from a [`playlist_mutation_lookup_statement`]
/// row.
pub fn authorize_playlist_delete_from_lookup(
    caller: Option<&str>,
    lookup: Option<MutationLookup>,
) -> Result<PlaylistDelete, LibraryError> {
    let caller = require_caller(caller)?;
    let public_id = require_lookup_owner(lookup)?;
    Ok(PlaylistDelete {
        public_id,
        owner_user_id: caller.to_string(),
    })
}

pub fn authorize_genre_update(
    caller: Option<&str>,
    rows: &[GenreRow],
    public_id: &str,
    name: &str,
    is_public: bool,
) -> Result<GenreUpdate, LibraryError> {
    let caller = require_caller(caller)?;
    let row = find_visible_genre(caller, rows, public_id)?;
    require_genre_owner(caller, row)?;
    Ok(GenreUpdate {
        public_id: row.public_id.clone(),
        owner_user_id: caller.to_string(),
        name: check_name(name)?,
        is_public,
    })
}

pub fn authorize_genre_delete(
    caller: Option<&str>,
    rows: &[GenreRow],
    public_id: &str,
) -> Result<GenreDelete, LibraryError> {
    let caller = require_caller(caller)?;
    let row = find_visible_genre(caller, rows, public_id)?;
    require_genre_owner(caller, row)?;
    Ok(GenreDelete {
        public_id: row.public_id.clone(),
        owner_user_id: caller.to_string(),
    })
}

pub fn authorize_playlist_update(
    caller: Option<&str>,
    rows: &[PlaylistRow],
    public_id: &str,
    name: &str,
    items: Vec<String>,
    is_public: bool,
) -> Result<PlaylistUpdate, LibraryError> {
    let caller = require_caller(caller)?;
    let row = find_visible_playlist(caller, rows, public_id)?;
    require_playlist_owner(caller, row)?;
    Ok(PlaylistUpdate {
        public_id: row.public_id.clone(),
        owner_user_id: caller.to_string(),
        name: check_name(name)?,
        items: check_items(items)?,
        is_public,
    })
}

pub fn authorize_playlist_delete(
    caller: Option<&str>,
    rows: &[PlaylistRow],
    public_id: &str,
) -> Result<PlaylistDelete, LibraryError> {
    let caller = require_caller(caller)?;
    let row = find_visible_playlist(caller, rows, public_id)?;
    require_playlist_owner(caller, row)?;
    Ok(PlaylistDelete {
        public_id: row.public_id.clone(),
        owner_user_id: caller.to_string(),
    })
}

// --- Statements: parameterized, owner-scoped ----------------------------------
//
// The Worker binds intent fields positionally ($1..$n); never interpolates a
// value into SQL. SELECT lists enumerate display columns only: `owner_user_id`
// filters server-side in WHERE and is never returned — except inside the
// `(owner_user_id = $2)` boolean of the mutation lookups, which answers the
// 403-vs-404 question without exposing the id itself.
//
// Mutation statements affect 0 rows when the row vanished between lookup and
// write (or never existed): the Worker maps that to NotFound (404). The
// owner predicate already makes cross-owner writes inexpressible, so a 0-row
// write is never a silent foreign mutation.

/// Caller-visible genre page, ordered by the opaque public id descending
/// (arbitrary order, NOT newest-first).
/// Parameters: owner_user_id ($1), limit clamped to [`MAX_PAGE_SIZE`]
/// ($2), cursor public id or NULL for the first page ($3).
///
/// The cursor is compared directly against the public-id ordering key, not
/// looked up in the table. A deleted or newly private boundary row therefore
/// cannot truncate later pages, and there is no cursor lookup that can reveal
/// whether an invisible row exists. The `$3::TEXT IS NULL` guard makes the
/// first page work; an above-max cursor matches every row and replays from
/// the top (mirrored by [`list_genres_page`]); visibility still filters every
/// returned row.
pub fn genre_list_statement() -> &'static str {
    "SELECT public_id, name, is_public FROM genres WHERE (owner_user_id = $1 OR is_public = TRUE) AND ($3::TEXT IS NULL OR public_id COLLATE \"C\" < $3::TEXT COLLATE \"C\") ORDER BY public_id COLLATE \"C\" DESC LIMIT $2"
}

/// Parameters: public_id, owner_user_id.
pub fn genre_read_statement() -> &'static str {
    "SELECT public_id, name, is_public FROM genres WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)"
}

/// Mutation lookup for one genre: visibility-filtered read returning only an
/// ownership boolean. Feeds [`authorize_genre_update_from_lookup`] /
/// [`authorize_genre_delete_from_lookup`].
/// Parameters: public_id, owner_user_id.
pub fn genre_mutation_lookup_statement() -> &'static str {
    "SELECT public_id, (owner_user_id = $2) AS is_owner FROM genres WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)"
}

/// Parameters: public_id, owner_user_id, name, is_public, created_at.
pub fn genre_insert_statement() -> &'static str {
    "INSERT INTO genres (public_id, owner_user_id, name, is_public, created_at) VALUES ($1, $2, $3, $4, $5)"
}

/// Parameters: public_id, owner_user_id, name, is_public.
/// 0 affected rows maps to NotFound (404).
pub fn genre_update_statement() -> &'static str {
    "UPDATE genres SET name = $3, is_public = $4 WHERE public_id = $1 AND owner_user_id = $2"
}

/// Parameters: public_id, owner_user_id.
/// 0 affected rows maps to NotFound (404).
pub fn genre_delete_statement() -> &'static str {
    "DELETE FROM genres WHERE public_id = $1 AND owner_user_id = $2"
}

/// Caller-visible playlist page (summaries: no `items`), ordered by the
/// opaque public id (arbitrary order, NOT newest-first).
/// Parameters and cursor semantics match [`genre_list_statement`]: the
/// `$3::TEXT IS NULL` guard serves the first page, and an above-max cursor
/// replays from the top in both the mirror and the SQL.
pub fn playlist_list_statement() -> &'static str {
    "SELECT public_id, name, is_public FROM playlists WHERE (owner_user_id = $1 OR is_public = TRUE) AND ($3::TEXT IS NULL OR public_id COLLATE \"C\" < $3::TEXT COLLATE \"C\") ORDER BY public_id COLLATE \"C\" DESC LIMIT $2"
}

/// Parameters: public_id, owner_user_id.
pub fn playlist_read_statement() -> &'static str {
    "SELECT public_id, name, items, is_public FROM playlists WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)"
}

/// Mutation lookup for one playlist: visibility-filtered read returning only
/// an ownership boolean. Feeds [`authorize_playlist_update_from_lookup`] /
/// [`authorize_playlist_delete_from_lookup`].
/// Parameters: public_id, owner_user_id.
pub fn playlist_mutation_lookup_statement() -> &'static str {
    "SELECT public_id, (owner_user_id = $2) AS is_owner FROM playlists WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)"
}

/// Parameters: public_id, owner_user_id, name, items, is_public, created_at.
pub fn playlist_insert_statement() -> &'static str {
    "INSERT INTO playlists (public_id, owner_user_id, name, items, is_public, created_at) VALUES ($1, $2, $3, $4, $5, $6)"
}

/// Parameters: public_id, owner_user_id, name, items, is_public.
/// 0 affected rows maps to NotFound (404).
pub fn playlist_update_statement() -> &'static str {
    "UPDATE playlists SET name = $3, items = $4, is_public = $5 WHERE public_id = $1 AND owner_user_id = $2"
}

/// Parameters: public_id, owner_user_id.
/// 0 affected rows maps to NotFound (404).
pub fn playlist_delete_statement() -> &'static str {
    "DELETE FROM playlists WHERE public_id = $1 AND owner_user_id = $2"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_caller_is_unauthorized() {
        assert_eq!(require_caller(Some("")), Err(LibraryError::Unauthorized));
    }

    #[test]
    fn new_public_id_is_22_char_unpadded_base64url() {
        let id = new_public_id();
        assert_eq!(id.len(), 22);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    #[test]
    fn page_size_is_clamped() {
        assert_eq!(clamp_page_size(0), 1);
        assert_eq!(clamp_page_size(10), 10);
        assert_eq!(clamp_page_size(MAX_PAGE_SIZE), MAX_PAGE_SIZE);
        assert_eq!(clamp_page_size(MAX_PAGE_SIZE + 1), MAX_PAGE_SIZE);
        assert_eq!(clamp_page_size(usize::MAX), MAX_PAGE_SIZE);
    }

    #[test]
    fn page_cursor_round_trips_public_ids_only() {
        for id in ["g-public-opaque-01", "p-public-0001", &"x".repeat(128)] {
            assert_eq!(parse_page_cursor(&page_cursor(id)), Ok(id.to_string()));
        }
        // The cursor wraps the public id itself: decoding it yields no
        // internal row id, only the already-public handle.
        assert_eq!(
            parse_page_cursor(&page_cursor("g-public-opaque-01")),
            Ok("g-public-opaque-01".to_string())
        );
        // Strict decoding via the shared helper: non-canonical trailing bits
        // are rejected, so distinct strings never alias to the same id.
        assert_eq!(crate::b64::decode("Zg").unwrap(), b"f");
        assert!(crate::b64::decode("Zh").is_err());
        assert_eq!(parse_page_cursor("Zg"), Ok("f".to_string()));
        assert_eq!(parse_page_cursor("Zh"), Err(LibraryError::Invalid));
        assert_eq!(parse_page_cursor(""), Err(LibraryError::Invalid));
        assert_eq!(parse_page_cursor("!!!"), Err(LibraryError::Invalid));
        assert_eq!(
            parse_page_cursor(&"A".repeat(193)),
            Err(LibraryError::Invalid)
        );
        assert_eq!(LibraryError::Invalid.http_status(), 422);
    }

    fn many_playlist_rows(n: usize) -> Vec<PlaylistRow> {
        // Deliberately NOT monotonic with internal_id: public ids run in
        // reverse so the tests prove list order follows the public id, not
        // insertion order or recency.
        (0..n)
            .map(|i| PlaylistRow {
                internal_id: i as i64 + 1,
                public_id: format!("p-public-{:04}", n - 1 - i),
                owner_user_id: "user-bob".into(),
                name: format!("Public {i:04}"),
                items: vec!["track-opaque".into()],
                is_public: true,
            })
            .collect()
    }

    #[test]
    fn pages_are_bounded_by_public_id_and_chainable() {
        let rows = many_playlist_rows(MAX_PAGE_SIZE + 10);
        let first = list_playlists_page(Some("user-alice"), &rows, 5000, None).unwrap();
        assert_eq!(first.len(), MAX_PAGE_SIZE);
        // Ordered by the opaque public id descending — NOT by internal id or
        // recency. The fixture ids run opposite to internal ids, so the head
        // is the lowest internal id: order follows the public handle.
        assert_eq!(first[0].id, format!("p-public-{:04}", MAX_PAGE_SIZE + 9));
        assert_eq!(first[0].name, "Public 0000");
        let json = serde_json::to_string(&first[0]).unwrap();
        assert!(!json.contains("items"), "list shapes omit items: {json}");

        // The Worker builds the next cursor from the last returned row —
        // no internal id lookup needed: the cursor wraps the public id.
        let last_first_page = first[MAX_PAGE_SIZE - 1].id.clone();
        let second = list_playlists_page(
            Some("user-alice"),
            &rows,
            MAX_PAGE_SIZE,
            Some(&page_cursor(&last_first_page)),
        )
        .unwrap();
        assert_eq!(second.len(), 10);
        assert!(second.iter().all(|p| p.id != last_first_page));

        // Auth still runs before paging: anonymous callers get 401, and a
        // garbage cursor is 422, not a silent restart.
        assert_eq!(
            list_playlists_page(None, &rows, 10, None),
            Err(LibraryError::Unauthorized)
        );
        assert_eq!(
            list_playlists_page(Some("user-alice"), &rows, 10, Some("!!!")),
            Err(LibraryError::Invalid)
        );
        // A well-formed cursor continues by keyset comparison in both the
        // mirror and the SQL: an above-max cursor replays from the top
        // (harmless — every row is still visibility-filtered), never a leak
        // and never a truncation.
        let replay = list_playlists_page(
            Some("user-alice"),
            &rows,
            MAX_PAGE_SIZE,
            Some(&page_cursor("p-public-9999")),
        )
        .unwrap();
        assert_eq!(replay, first);
    }

    #[test]
    fn paging_continues_when_cursor_row_is_deleted_or_hidden() {
        let rows = many_playlist_rows(60);
        let first = list_playlists_page(Some("user-alice"), &rows, 10, None).unwrap();
        let cursor_id = first.last().unwrap().id.clone();
        let after = page_cursor(&cursor_id);
        let expected: Vec<String> = (40..50).rev().map(|i| format!("p-public-{i:04}")).collect();

        let mut deleted = rows.clone();
        deleted.retain(|row| row.public_id != cursor_id);
        let after_delete =
            list_playlists_page(Some("user-alice"), &deleted, 10, Some(&after)).unwrap();
        assert_eq!(
            after_delete
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            expected
        );

        let mut hidden = rows;
        let boundary = hidden
            .iter_mut()
            .find(|row| row.public_id == cursor_id)
            .unwrap();
        boundary.is_public = false;
        boundary.owner_user_id = "user-bob".into();
        let after_hide =
            list_playlists_page(Some("user-alice"), &hidden, 10, Some(&after)).unwrap();
        assert_eq!(
            after_hide
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            expected
        );

        // Whole previous page vanishes before the next request: the old
        // cursor sits above every remaining row, and the next page must still
        // return them (mirror and SQL agree — no silent end-of-list).
        let first_ids: Vec<String> = first.iter().map(|row| row.id.clone()).collect();
        let mut page_gone = many_playlist_rows(60);
        page_gone.retain(|row| !first_ids.contains(&row.public_id));
        let after_whole_delete =
            list_playlists_page(Some("user-alice"), &page_gone, 10, Some(&after)).unwrap();
        assert_eq!(
            after_whole_delete
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            expected
        );

        let mut page_hidden = many_playlist_rows(60);
        for row in page_hidden
            .iter_mut()
            .filter(|row| first_ids.contains(&row.public_id))
        {
            row.is_public = false;
            row.owner_user_id = "user-bob".into();
        }
        let after_whole_hide =
            list_playlists_page(Some("user-alice"), &page_hidden, 10, Some(&after)).unwrap();
        assert_eq!(
            after_whole_hide
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn own_older_row_is_reachable_by_following_the_cursor() {
        // Regression for the review probe: many newer public rows by
        // another user plus one older private row by the caller. Order is by
        // public id (the Bob block sorts above the Alice row by handle), so
        // every page must be reachable and the caller's own row must appear
        // on the last page — never stranded past an unreachable first 50.
        let mut rows: Vec<PlaylistRow> = vec![PlaylistRow {
            internal_id: 1,
            public_id: "p-alice-own".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Own".into(),
            items: vec![],
            is_public: false,
        }];
        rows.extend((0..120).map(|i| PlaylistRow {
            internal_id: i as i64 + 2,
            public_id: format!("p-bob-{i:04}"),
            owner_user_id: "user-bob".into(),
            name: format!("Bob {i:04}"),
            items: vec![],
            is_public: true,
        }));
        let mut after: Option<String> = None;
        let mut seen_own = false;
        let mut pages = 0;
        loop {
            let page =
                list_playlists_page(Some("user-alice"), &rows, MAX_PAGE_SIZE, after.as_deref())
                    .unwrap();
            pages += 1;
            assert!(pages < 10, "paging must terminate");
            if page.iter().any(|p| p.id == "p-alice-own") {
                seen_own = true;
            }
            if page.len() < MAX_PAGE_SIZE {
                break;
            }
            after = Some(page_cursor(&page.last().unwrap().id));
        }
        assert!(seen_own, "caller's own older row reachable via cursor");
        assert_eq!(pages, 3, "121 rows at 50/page");
    }

    #[test]
    fn genre_pages_are_bounded() {
        // Ids run opposite to internal ids: order must follow the public
        // handle, not insertion order.
        let n = MAX_PAGE_SIZE + 5;
        let rows: Vec<GenreRow> = (0..n)
            .map(|i| GenreRow {
                internal_id: i as i64 + 1,
                public_id: format!("g-public-{:04}", n - 1 - i),
                owner_user_id: "user-bob".into(),
                name: format!("Genre {i:04}"),
                is_public: true,
            })
            .collect();
        let page = list_genres_page(Some("user-alice"), &rows, usize::MAX, None).unwrap();
        assert_eq!(page.len(), MAX_PAGE_SIZE);
        assert_eq!(page[0].id, format!("g-public-{:04}", n - 1));
        assert_eq!(page[0].name, "Genre 0000");
    }

    #[test]
    fn lookup_authorizers_drive_403_404_from_statement_output() {
        // Simulate exactly what the lookup statements return: the target id
        // plus an ownership boolean, never the owner id.
        let own = Some(MutationLookup {
            public_id: "g-alice-public-opaque-02".into(),
            is_owner: true,
        });
        let update =
            authorize_genre_update_from_lookup(Some("user-alice"), own, "Renamed", true).unwrap();
        assert_eq!(update.owner_user_id, "user-alice");
        assert_eq!(update.public_id, "g-alice-public-opaque-02");

        let foreign = Some(MutationLookup {
            public_id: "g-bob-public-opaque-04".into(),
            is_owner: false,
        });
        assert_eq!(
            authorize_genre_update_from_lookup(Some("user-alice"), foreign.clone(), "X", true),
            Err(LibraryError::Forbidden)
        );
        assert_eq!(
            authorize_genre_delete_from_lookup(Some("user-alice"), foreign),
            Err(LibraryError::Forbidden)
        );
        // No lookup row: invisible id, 404 — never 403.
        assert_eq!(
            authorize_genre_delete_from_lookup(Some("user-alice"), None),
            Err(LibraryError::NotFound)
        );
        assert_eq!(
            authorize_playlist_delete_from_lookup(Some("user-alice"), None),
            Err(LibraryError::NotFound)
        );
        // Auth first, even with a valid lookup in hand.
        assert_eq!(
            authorize_genre_update_from_lookup(
                None,
                Some(MutationLookup {
                    public_id: "g-alice-public-opaque-02".into(),
                    is_owner: true,
                }),
                "Renamed",
                true
            ),
            Err(LibraryError::Unauthorized)
        );
        let playlist_update = authorize_playlist_update_from_lookup(
            Some("user-alice"),
            Some(MutationLookup {
                public_id: "p-alice-public-opaque-12".into(),
                is_owner: true,
            }),
            "Party",
            vec!["track-opaque".into()],
            true,
        )
        .unwrap();
        assert_eq!(playlist_update.owner_user_id, "user-alice");
    }

    #[test]
    fn list_and_lookup_statements_are_bounded_and_owner_free() {
        for sql in [genre_list_statement(), playlist_list_statement()] {
            assert!(
                sql.contains("ORDER BY public_id COLLATE \"C\" DESC"),
                "{sql}"
            );
            assert!(sql.contains("LIMIT $2"), "{sql}");
            // The cursor is compared directly with the public ordering key,
            // so continuation does not depend on the boundary row existing.
            // First-page guard + cursor predicate must stay grouped as one
            // unit: `(owner OR public) AND (guard OR cursor_cmp)`. Without
            // the parens, `AND` binds tighter than `OR` and a non-NULL
            // cursor bypasses the visibility filter, leaking foreign private
            // rows; deleting the guard breaks every first page (`NULL`
            // comparison). Pin the whole `AND (...) ORDER BY` unit so
            // neither mutant passes.
            assert!(
                sql.contains(
                    "AND ($3::TEXT IS NULL OR public_id COLLATE \"C\" < $3::TEXT COLLATE \"C\") ORDER BY"
                ),
                "{sql}"
            );
            // No cursor-row lookup exists to bypass or weaken visibility;
            // this predicate applies to every returned row.
            assert_eq!(
                sql.matches("(owner_user_id = $1 OR is_public = TRUE)")
                    .count(),
                1,
                "{sql}"
            );
            assert!(!sql.contains("SELECT id FROM"), "{sql}");
        }
        // List projections return the public id the Worker builds the next
        // cursor from — never items, owner, or internal ids.
        for sql in [genre_list_statement(), playlist_list_statement()] {
            let select = sql.split(" FROM ").next().unwrap();
            assert!(select.contains("public_id"), "{select}");
            assert!(!select.contains("items"), "{select}");
            assert!(!select.contains("owner"), "{select}");
            assert!(!select.contains("internal"), "{select}");
        }
        let playlist_select = playlist_list_statement().split(" FROM ").next().unwrap();
        assert!(!playlist_select.contains("items"), "{playlist_select}");
        assert!(!playlist_select.contains("owner"), "{playlist_select}");
        assert!(!playlist_select.contains("internal"), "{playlist_select}");
        // Lookups answer ownership with a boolean; the id never leaves SQL.
        for sql in [
            genre_mutation_lookup_statement(),
            playlist_mutation_lookup_statement(),
        ] {
            assert!(sql.contains("(owner_user_id = $2) AS is_owner"), "{sql}");
            assert!(sql.contains("public_id = $1"), "{sql}");
            assert!(sql.contains("$1"), "{sql}");
        }
    }
}
