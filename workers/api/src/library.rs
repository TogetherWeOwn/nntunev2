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
//!   owned by someone else are [`LibraryError::Forbidden`].
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

/// Random 128-bit opaque public id (base64url, 22 chars). Never sequential,
/// never derived from the internal row id.
pub fn new_public_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(22);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
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

fn genre_visible(caller: &str, row: &GenreRow) -> bool {
    row.is_public || row.owner_user_id == caller
}

fn playlist_visible(caller: &str, row: &PlaylistRow) -> bool {
    row.is_public || row.owner_user_id == caller
}

// --- Reads --------------------------------------------------------------------

/// List caller-visible genres: own rows plus public rows.
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

/// List caller-visible playlists: own rows plus public rows.
pub fn list_playlists(
    caller: Option<&str>,
    rows: &[PlaylistRow],
) -> Result<Vec<PlaylistResponse>, LibraryError> {
    let caller = require_caller(caller)?;
    Ok(rows
        .iter()
        .filter(|row| playlist_visible(caller, row))
        .map(playlist_response)
        .collect())
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
// filters server-side in WHERE and is never returned.

/// Parameters: owner_user_id.
pub fn genre_list_statement() -> &'static str {
    "SELECT public_id, name, is_public FROM genres WHERE owner_user_id = $1 OR is_public = TRUE"
}

/// Parameters: public_id, owner_user_id.
pub fn genre_read_statement() -> &'static str {
    "SELECT public_id, name, is_public FROM genres WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)"
}

/// Parameters: public_id, owner_user_id, name, is_public, created_at.
pub fn genre_insert_statement() -> &'static str {
    "INSERT INTO genres (public_id, owner_user_id, name, is_public, created_at) VALUES ($1, $2, $3, $4, $5)"
}

/// Parameters: public_id, owner_user_id, name, is_public.
pub fn genre_update_statement() -> &'static str {
    "UPDATE genres SET name = $3, is_public = $4 WHERE public_id = $1 AND owner_user_id = $2"
}

/// Parameters: public_id, owner_user_id.
pub fn genre_delete_statement() -> &'static str {
    "DELETE FROM genres WHERE public_id = $1 AND owner_user_id = $2"
}

/// Parameters: owner_user_id.
pub fn playlist_list_statement() -> &'static str {
    "SELECT public_id, name, items, is_public FROM playlists WHERE owner_user_id = $1 OR is_public = TRUE"
}

/// Parameters: public_id, owner_user_id.
pub fn playlist_read_statement() -> &'static str {
    "SELECT public_id, name, items, is_public FROM playlists WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)"
}

/// Parameters: public_id, owner_user_id, name, items, is_public, created_at.
pub fn playlist_insert_statement() -> &'static str {
    "INSERT INTO playlists (public_id, owner_user_id, name, items, is_public, created_at) VALUES ($1, $2, $3, $4, $5, $6)"
}

/// Parameters: public_id, owner_user_id, name, items, is_public.
pub fn playlist_update_statement() -> &'static str {
    "UPDATE playlists SET name = $3, items = $4, is_public = $5 WHERE public_id = $1 AND owner_user_id = $2"
}

/// Parameters: public_id, owner_user_id.
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
}
