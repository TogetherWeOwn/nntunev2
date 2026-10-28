//! Endpoint handlers: one arm per supported op, in v1 route order.
//!
//! Conventions: missing required params are error 10, unknown ids error 70
//! (both HTTP 200 inside the envelope, as in v1). `maxBitRate`/`timeOffset`
//! on the byte routes are accepted and ignored — no live transcoding, as in
//! v1. Mutations operate on the injected `Library`, so fixtures exercise the
//! real lifecycle (create → get → update → delete).

use crate::auth::Identity;
use crate::envelope::code;
use crate::library::{
    Album, AlbumListQuery, Artist, Library, PlayQueue, Playlist, Song, StarKind, NOW,
};
use crate::value::Value;

/// Parsed query string: first value wins for `get`, all values for `get_all`.
#[derive(Clone, Default)]
pub struct Params {
    pairs: Vec<(String, String)>,
}

impl std::fmt::Debug for Params {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Even parameter names are caller-controlled: expose neither names
        // nor values, including under alternate/pretty formatting.
        f.debug_struct("Params")
            .field("pair_count", &self.pairs.len())
            .finish()
    }
}

impl Params {
    pub fn parse(query: &str) -> Params {
        let mut pairs = Vec::new();
        for part in query.split('&') {
            if part.is_empty() {
                continue;
            }
            let (k, v) = match part.find('=') {
                Some(i) => (&part[..i], &part[i + 1..]),
                None => (part, ""),
            };
            pairs.push((decode(k), decode(v)));
        }
        Params { pairs }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

fn decode(s: &str) -> String {
    // Percent-decode over raw bytes, then UTF-8 (lossy): multi-byte UTF-8
    // stays intact (`Bj%C3%B6rk` -> `Björk`) and a stray `%` never panics
    // (byte indexing is always safe; `&str` slicing is not).
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Early return from `handle_endpoint` on `Err` results.
macro_rules! ok_or_return {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
}

pub enum EndpointResult {
    Doc(Vec<(String, Value)>),
    Binary { content_type: String, body: Vec<u8> },
    Err(i64, String),
}

fn missing() -> EndpointResult {
    EndpointResult::Err(
        code::REQUIRED_PARAM_MISSING,
        "Required parameter is missing.".to_string(),
    )
}

// v1 `findOrFail` misses share one text via `SubsonicAwareErrorRenderer`.
fn not_found() -> EndpointResult {
    not_found_with("The requested data was not found.")
}

fn not_found_with(message: &str) -> EndpointResult {
    EndpointResult::Err(code::NOT_FOUND, message.to_string())
}

// v1 authorization failures share one text via the same renderer.
fn forbidden() -> EndpointResult {
    EndpointResult::Err(
        code::NOT_AUTHORIZED,
        "User is not authorized for the given operation.".to_string(),
    )
}

fn req<'a>(params: &'a Params, name: &str) -> Result<&'a str, EndpointResult> {
    match params.get(name) {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(missing()),
    }
}

// --- shared shapes (key order is the wire order) ---

// v1 `SongResource::toArray` order: `bitRate`/`path`/`playCount` are not
// emitted, and `transcodedContentType`/`transcodedSuffix` are always present
// (empty: no transcoding exists). Nullable fields stay non-null here because
// the seeded store fills every column; a real backend omits empties.
fn song_entry(song: &Song) -> Value {
    Value::map(vec![
        ("id", Value::str_value(&song.id)),
        ("parent", Value::str_value(&song.album_id)),
        ("isDir", Value::bool_value(false)),
        ("title", Value::str_value(&song.title)),
        ("album", Value::str_value(&song.album)),
        ("artist", Value::str_value(&song.artist)),
        ("track", Value::int_value(song.track)),
        ("year", Value::int_value(song.year)),
        ("genre", Value::str_value(&song.genre)),
        ("coverArt", Value::str_value(&song.cover_art)),
        ("size", Value::int_value(song.size)),
        ("contentType", Value::str_value(&song.content_type)),
        ("suffix", Value::str_value(&song.suffix)),
        ("duration", Value::int_value(song.duration)),
        ("created", Value::str_value(&song.created)),
        ("albumId", Value::str_value(&song.album_id)),
        ("artistId", Value::str_value(&song.artist_id)),
        ("type", Value::str_value("music")),
        ("discNumber", Value::int_value(song.disc)),
        ("isVideo", Value::bool_value(false)),
    ])
}

fn starred_song_entry<S: Library>(store: &S, identity: &Identity, song: &Song) -> Value {
    let Value::Map(mut entries) = song_entry(song) else {
        unreachable!()
    };
    if let Some(rating) = store.rating(identity, &song.id) {
        entries.push(("userRating".to_string(), Value::int_value(rating)));
    }
    if store.starred(identity, StarKind::Song).contains(&song.id) {
        entries.push(("starred".to_string(), Value::str_value(NOW)));
    }
    entries.push(("transcodedContentType".to_string(), Value::str_value("")));
    entries.push(("transcodedSuffix".to_string(), Value::str_value("")));
    Value::Map(entries)
}

fn album_item(album: &Album, song_count: i64) -> Value {
    Value::map(vec![
        ("id", Value::str_value(&album.id)),
        ("name", Value::str_value(&album.name)),
        ("artist", Value::str_value(&album.artist)),
        ("artistId", Value::str_value(&album.artist_id)),
        ("coverArt", Value::str_value(&album.id)),
        ("songCount", Value::int_value(song_count)),
        ("duration", Value::int_value(album.duration)),
        ("created", Value::str_value(&album.created)),
    ])
}

fn artist_item(artist: &Artist, album_count: i64) -> Value {
    Value::map(vec![
        ("id", Value::str_value(&artist.id)),
        ("name", Value::str_value(&artist.name)),
        ("albumCount", Value::int_value(album_count)),
    ])
}

fn playlist_duration<S: Library>(store: &S, playlist: &Playlist) -> i64 {
    playlist
        .song_ids
        .iter()
        .filter_map(|id| store.song(id))
        .map(|s| s.duration)
        .sum()
}

fn playlist_detail<S: Library>(store: &S, identity: &Identity, playlist: &Playlist) -> Value {
    let mut entries = vec![
        ("id", Value::str_value(&playlist.id)),
        ("name", Value::str_value(&playlist.name)),
        (
            "songCount",
            Value::int_value(playlist.song_ids.len() as i64),
        ),
        (
            "duration",
            Value::int_value(playlist_duration(store, playlist)),
        ),
        ("created", Value::str_value(&playlist.created)),
        ("changed", Value::str_value(&playlist.changed)),
    ];
    if !playlist.song_ids.is_empty() {
        entries.push((
            "entry",
            Value::List(
                playlist
                    .song_ids
                    .iter()
                    .filter_map(|id| store.song(id))
                    .map(|s| starred_song_entry(store, identity, &s))
                    .collect(),
            ),
        ));
    }
    Value::map(entries)
}

fn size_offset(params: &Params) -> (usize, usize) {
    let size = params
        .get("size")
        .or_else(|| params.get("count"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    (size, offset)
}

fn search_count(
    params: &Params,
    name: &str,
    default_count: usize,
) -> Result<usize, EndpointResult> {
    match params.get(name) {
        None => Ok(default_count),
        Some(value) => match value.parse::<usize>() {
            Ok(count) if count <= 500 => Ok(count),
            _ => Err(missing()),
        },
    }
}

fn list_size(params: &Params) -> Result<usize, EndpointResult> {
    match params.get("size").unwrap_or("10").parse::<usize>() {
        Ok(size) if (1..=500).contains(&size) => Ok(size),
        _ => Err(missing()),
    }
}

fn artist_index<S: Library>(store: &S) -> Vec<Value> {
    let mut groups: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();
    for artist in store.artists() {
        let albums = store.albums_for_artist(&artist.id).len() as i64;
        let name = artist.name.trim();
        let stripped = ["The", "El", "La", "Los", "Las", "Le", "Les"]
            .iter()
            .find_map(|article| {
                name.get(..article.len())
                    .filter(|prefix| prefix.eq_ignore_ascii_case(article))
                    .and_then(|_| name.get(article.len()..))
                    .filter(|suffix| suffix.starts_with(' '))
            })
            .map(str::trim)
            .unwrap_or(name);
        let key = stripped
            .chars()
            .next()
            .map(|c| {
                // v1 uppercases before byte-based ctype_alpha (e.g. ß → SS).
                let upper = c.to_uppercase().to_string();
                if upper.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                    upper
                } else {
                    "#".to_string()
                }
            })
            .unwrap_or_else(|| "#".to_string());
        groups
            .entry(key)
            .or_default()
            .push(artist_item(&artist, albums));
    }
    groups
        .into_iter()
        .map(|(name, artists)| {
            Value::map(vec![
                ("name", Value::str_value(name)),
                ("artist", Value::List(artists)),
            ])
        })
        .collect()
}

// --- dispatch ---

pub fn handle_endpoint<S: Library>(
    store: &mut S,
    identity: &Identity,
    name: &str,
    params: &Params,
) -> EndpointResult {
    match name {
        "ping" => EndpointResult::Doc(vec![]),
        "getLicense" => EndpointResult::Doc(vec![(
            "license".to_string(),
            Value::map(vec![
                ("valid", Value::bool_value(true)),
                ("email", Value::str_value(&identity.email)),
            ]),
        )]),
        "getMusicFolders" => EndpointResult::Doc(vec![(
            "musicFolders".to_string(),
            Value::map(vec![(
                "musicFolder",
                Value::List(vec![Value::map(vec![
                    ("id", Value::int_value(1)),
                    ("name", Value::str_value("Music")),
                ])]),
            )]),
        )]),
        "getArtists" => {
            let index = artist_index(store);
            EndpointResult::Doc(vec![(
                "artists".to_string(),
                Value::map(vec![
                    (
                        "ignoredArticles",
                        Value::str_value("The El La Los Las Le Les"),
                    ),
                    ("index", Value::List(index)),
                ]),
            )])
        }
        "getIndexes" => {
            let index = artist_index(store);
            EndpointResult::Doc(vec![(
                "indexes".to_string(),
                Value::map(vec![
                    (
                        "lastModified",
                        Value::int_value(store.artists_last_modified()),
                    ),
                    (
                        "ignoredArticles",
                        Value::str_value("The El La Los Las Le Les"),
                    ),
                    ("index", Value::List(index)),
                ]),
            )])
        }
        "getMusicDirectory" => {
            let id = ok_or_return!(req(params, "id"));
            if let Some(album) = store.album(id) {
                let child: Vec<Value> = store
                    .songs_for_album(&album.id)
                    .iter()
                    .map(|s| starred_song_entry(store, identity, s))
                    .collect();
                EndpointResult::Doc(vec![(
                    "directory".to_string(),
                    Value::map(vec![
                        ("id", Value::str_value(&album.id)),
                        ("name", Value::str_value(&album.name)),
                        ("child", Value::List(child)),
                    ]),
                )])
            } else if let Some(artist) = store.artist(id) {
                let child: Vec<Value> = store
                    .albums_for_artist(&artist.id)
                    .iter()
                    .map(|a| {
                        let n = store.songs_for_album(&a.id).len() as i64;
                        album_item(a, n)
                    })
                    .collect();
                EndpointResult::Doc(vec![(
                    "directory".to_string(),
                    Value::map(vec![
                        ("id", Value::str_value(&artist.id)),
                        ("name", Value::str_value(&artist.name)),
                        ("child", Value::List(child)),
                    ]),
                )])
            } else {
                not_found_with("Directory not found.")
            }
        }
        "getArtist" => {
            let id = ok_or_return!(req(params, "id"));
            match store.artist(id) {
                Some(artist) => {
                    let albums: Vec<Value> = store
                        .albums_for_artist(&artist.id)
                        .iter()
                        .map(|a| {
                            let n = store.songs_for_album(&a.id).len() as i64;
                            album_item(a, n)
                        })
                        .collect();
                    EndpointResult::Doc(vec![(
                        "artist".to_string(),
                        Value::map(vec![
                            ("id", Value::str_value(&artist.id)),
                            ("name", Value::str_value(&artist.name)),
                            ("albumCount", Value::int_value(albums.len() as i64)),
                            ("album", Value::List(albums)),
                        ]),
                    )])
                }
                None => not_found_with("Artist not found."),
            }
        }
        "getAlbum" => {
            let id = ok_or_return!(req(params, "id"));
            match store.album(id) {
                Some(album) => {
                    let songs: Vec<Value> = store
                        .songs_for_album(&album.id)
                        .iter()
                        .map(|s| starred_song_entry(store, identity, s))
                        .collect();
                    let Value::Map(mut entries) = album_item(&album, songs.len() as i64) else {
                        unreachable!()
                    };
                    entries.push(("song".to_string(), Value::List(songs)));
                    EndpointResult::Doc(vec![("album".to_string(), Value::Map(entries))])
                }
                None => not_found_with("Album not found."),
            }
        }
        "getSong" => {
            let id = ok_or_return!(req(params, "id"));
            match store.song(id) {
                Some(song) => EndpointResult::Doc(vec![(
                    "song".to_string(),
                    starred_song_entry(store, identity, &song),
                )]),
                None => not_found_with("Song not found."),
            }
        }
        "getGenres" => {
            let genres: Vec<Value> = store
                .genres()
                .into_iter()
                .map(|(name, songs, albums)| {
                    Value::map(vec![
                        ("songCount", Value::int_value(songs)),
                        ("albumCount", Value::int_value(albums)),
                        ("value", Value::str_value(name)),
                    ])
                })
                .collect();
            EndpointResult::Doc(vec![(
                "genres".to_string(),
                Value::map(vec![("genre", Value::List(genres))]),
            )])
        }
        "search2" => {
            let query = ok_or_return!(req(params, "query"));
            search2(store, identity, query)
        }
        "search3" => {
            let query = params.get("query").unwrap_or("");
            let (size, offset) = size_offset(params);
            let default_count = if query.is_empty() { size } else { 20 };
            let artist_count = ok_or_return!(search_count(params, "artistCount", default_count));
            let album_count = ok_or_return!(search_count(params, "albumCount", default_count));
            let song_count = ok_or_return!(search_count(params, "songCount", default_count));
            let artists: Vec<Value> = store
                .search_artists(query)
                .into_iter()
                .skip(offset)
                .take(artist_count)
                .map(|artist| {
                    let album_count = store.albums_for_artist(&artist.id).len() as i64;
                    artist_item(&artist, album_count)
                })
                .collect();
            let albums: Vec<Value> = store
                .search_albums(query)
                .into_iter()
                .skip(offset)
                .take(album_count)
                .map(|album| {
                    let songs = store.songs_for_album(&album.id).len() as i64;
                    album_item(&album, songs)
                })
                .collect();
            let songs: Vec<Value> = store
                .search_songs(query)
                .into_iter()
                .skip(offset)
                .take(song_count)
                .map(|song| starred_song_entry(store, identity, &song))
                .collect();
            EndpointResult::Doc(vec![(
                "searchResult3".to_string(),
                Value::map(vec![
                    ("artist", Value::List(artists)),
                    ("album", Value::List(albums)),
                    ("song", Value::List(songs)),
                ]),
            )])
        }
        "getAlbumList" | "getAlbumList2" => {
            let list_type = ok_or_return!(req(params, "type"));
            const ALBUM_LIST_TYPES: &[&str] = &[
                "newest",
                "frequent",
                "random",
                "starred",
                "recent",
                "highest",
                "byYear",
                "byGenre",
                "alphabeticalByName",
                "alphabeticalByArtist",
            ];
            if !ALBUM_LIST_TYPES.contains(&list_type) {
                return missing();
            }
            let from_year = if list_type == "byYear" {
                match req(params, "fromYear")
                    .ok()
                    .and_then(|year| year.parse().ok())
                {
                    Some(year) => Some(year),
                    None => return missing(),
                }
            } else {
                None
            };
            let to_year = if list_type == "byYear" {
                match req(params, "toYear")
                    .ok()
                    .and_then(|year| year.parse().ok())
                {
                    Some(year) => Some(year),
                    _ => return missing(),
                }
            } else {
                None
            };
            let genre = if list_type == "byGenre" {
                match req(params, "genre") {
                    Ok(genre) => Some(genre),
                    Err(error) => return error,
                }
            } else {
                None
            };
            let size = ok_or_return!(list_size(params));
            let offset = match params.get("offset").unwrap_or("0").parse::<usize>() {
                Ok(offset) => offset,
                Err(_) => return missing(),
            };
            let query = AlbumListQuery {
                identity,
                list_type,
                size,
                offset,
                from_year,
                to_year,
                genre,
            };
            let albums = if name == "getAlbumList" {
                store.album_list(query)
            } else {
                ok_or_return!(store
                    .album_list2(query)
                    .map_err(|(code, message)| EndpointResult::Err(code, message)))
            };
            let page: Vec<Value> = albums
                .into_iter()
                .map(|a| {
                    let n = store.songs_for_album(&a.id).len() as i64;
                    album_item(&a, n)
                })
                .collect();
            let key = if name == "getAlbumList" {
                "albumList"
            } else {
                "albumList2"
            };
            EndpointResult::Doc(vec![(
                key.to_string(),
                Value::map(vec![("album", Value::List(page))]),
            )])
        }
        "stream" | "download" => {
            let id = ok_or_return!(req(params, "id"));
            match store.audio(id) {
                Some(bytes) => {
                    let content_type = store
                        .song(id)
                        .map(|s| s.content_type)
                        .unwrap_or_else(|| "audio/mpeg".to_string());
                    EndpointResult::Binary {
                        content_type,
                        body: bytes,
                    }
                }
                None => not_found_with("Song not found."),
            }
        }
        "getCoverArt" => {
            let id = ok_or_return!(req(params, "id"));
            match store.cover(id) {
                Some(bytes) => EndpointResult::Binary {
                    content_type: "image/png".to_string(),
                    body: bytes,
                },
                None => not_found_with("Cover art not found."),
            }
        }
        "getPlaylists" => {
            let items: Vec<Value> = store
                .playlists(identity)
                .iter()
                .map(|p| playlist_detail(store, identity, p))
                .collect();
            let playlists = if items.is_empty() {
                Value::Map(vec![])
            } else {
                Value::map(vec![("playlist", Value::List(items))])
            };
            EndpointResult::Doc(vec![("playlists".to_string(), playlists)])
        }
        "getPlaylist" => {
            let id = ok_or_return!(req(params, "id"));
            match store.playlist(identity, id) {
                Some(playlist) if playlist.owner == identity.username => {
                    EndpointResult::Doc(vec![(
                        "playlist".to_string(),
                        playlist_detail(store, identity, &playlist),
                    )])
                }
                Some(_) => forbidden(),
                None => not_found(),
            }
        }
        "createPlaylist" => {
            let name = ok_or_return!(req(params, "name")).to_string();
            let mut playlist = store.create_playlist(identity, name);
            let add: Vec<String> = params
                .get_all("songId")
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            if !add.is_empty() {
                let id = playlist.id.clone();
                ok_or_return!(store
                    .update_playlist(identity, &id, None, add, vec![])
                    .map_err(|(c, m)| EndpointResult::Err(c, m)));
                playlist = store.playlist(identity, &id).unwrap_or(playlist);
            }
            EndpointResult::Doc(vec![(
                "playlist".to_string(),
                playlist_detail(store, identity, &playlist),
            )])
        }
        "updatePlaylist" => {
            let id = ok_or_return!(req(params, "playlistId")).to_string();
            let name = params.get("name").map(|s| s.to_string());
            let add: Vec<String> = params
                .get_all("songIdToAdd")
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            let remove: Vec<usize> = params
                .get_all("songIndexToRemove")
                .into_iter()
                .filter_map(|s| s.parse().ok())
                .collect();
            match store.update_playlist(identity, &id, name, add, remove) {
                Ok(()) => EndpointResult::Doc(vec![]),
                Err((error_code, message)) if error_code == code::NOT_AUTHORIZED => {
                    EndpointResult::Err(error_code, message)
                }
                Err(_) => not_found(),
            }
        }
        "deletePlaylist" => {
            let id = ok_or_return!(req(params, "id"));
            match store.delete_playlist(identity, id) {
                Ok(()) => EndpointResult::Doc(vec![]),
                Err((error_code, message)) if error_code == code::NOT_AUTHORIZED => {
                    EndpointResult::Err(error_code, message)
                }
                Err(_) => not_found(),
            }
        }
        "scrobble" => {
            let ids = params.get_all("id");
            if ids.is_empty() {
                return missing();
            }
            // v1 FILTER_VALIDATE_BOOL: absent defaults true; only these
            // case-insensitive values are true, including surrounding space.
            let submission = params.get("submission").is_none_or(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            });
            if submission {
                for id in ids {
                    store.scrobble(identity, id);
                }
            }
            EndpointResult::Doc(vec![])
        }
        "star" => star_unstar(store, identity, params, true),
        "unstar" => star_unstar(store, identity, params, false),
        "setRating" => {
            let id = ok_or_return!(req(params, "id")).to_string();
            let rating: i64 = match params.get("rating").and_then(|v| v.parse().ok()) {
                Some(r) if (0..=5).contains(&r) => r,
                _ => return missing(),
            };
            if store.set_rating(identity, &id, rating) {
                EndpointResult::Doc(vec![])
            } else {
                not_found()
            }
        }
        "getStarred" | "getStarred2" => {
            let key = if name == "getStarred" {
                "starred"
            } else {
                "starred2"
            };
            let mut entries: Vec<(String, Value)> = Vec::new();
            let artists: Vec<Value> = store
                .starred(identity, StarKind::Artist)
                .into_iter()
                .filter_map(|id| store.artist(&id))
                .map(|a| {
                    let n = store.albums_for_artist(&a.id).len() as i64;
                    artist_item(&a, n)
                })
                .collect();
            if !artists.is_empty() {
                entries.push(("artist".to_string(), Value::List(artists)));
            }
            let albums: Vec<Value> = store
                .starred(identity, StarKind::Album)
                .into_iter()
                .filter_map(|id| store.album(&id))
                .map(|a| {
                    let n = store.songs_for_album(&a.id).len() as i64;
                    album_item(&a, n)
                })
                .collect();
            if !albums.is_empty() {
                entries.push(("album".to_string(), Value::List(albums)));
            }
            let songs: Vec<Value> = store
                .starred(identity, StarKind::Song)
                .into_iter()
                .filter_map(|id| store.song(&id))
                .map(|s| starred_song_entry(store, identity, &s))
                .collect();
            if !songs.is_empty() {
                entries.push(("song".to_string(), Value::List(songs)));
            }
            EndpointResult::Doc(vec![(key.to_string(), Value::Map(entries))])
        }
        "getRandomSongs" => {
            let size = ok_or_return!(list_size(params));
            let songs: Vec<Value> = store
                .random_songs(identity, size)
                .iter()
                .map(|s| starred_song_entry(store, identity, s))
                .collect();
            EndpointResult::Doc(vec![(
                "randomSongs".to_string(),
                Value::map(vec![("song", Value::List(songs))]),
            )])
        }
        "getNowPlaying" => {
            EndpointResult::Doc(vec![("nowPlaying".to_string(), Value::Map(vec![]))])
        }
        "getUser" => {
            let username = ok_or_return!(req(params, "username"));
            // v1 `GetUserController`: only self or an admin may read a user.
            if username != identity.username && !identity.is_admin {
                return forbidden();
            }
            // Roles are v1 `UserResource::toArray` exactly: `adminRole` from
            // the target's role, `uploadRole`/`coverArtRole` from
            // admin-or-manager, the rest fixed constants, no `folder`.
            match store.user_account(username) {
                Some(account) => EndpointResult::Doc(vec![(
                    "user".to_string(),
                    Value::map(vec![
                        ("username", Value::str_value(username)),
                        ("email", Value::str_value(&account.email)),
                        ("scrobblingEnabled", Value::bool_value(true)),
                        ("adminRole", Value::bool_value(account.is_admin)),
                        ("settingsRole", Value::bool_value(true)),
                        ("downloadRole", Value::bool_value(true)),
                        ("uploadRole", Value::bool_value(account.can_manage())),
                        ("playlistRole", Value::bool_value(true)),
                        ("coverArtRole", Value::bool_value(account.can_manage())),
                        ("commentRole", Value::bool_value(false)),
                        ("podcastRole", Value::bool_value(true)),
                        ("streamRole", Value::bool_value(true)),
                        ("jukeboxRole", Value::bool_value(false)),
                        ("shareRole", Value::bool_value(false)),
                    ]),
                )]),
                None => not_found(),
            }
        }
        "getAvatar" => {
            let username = ok_or_return!(req(params, "username"));
            // v1 `GetAvatarController`: same self-or-admin rule as getUser,
            // so one account's avatar is never an oracle for another's.
            if username != identity.username && !identity.is_admin {
                return forbidden();
            }
            match store.avatar(username) {
                Some(bytes) => EndpointResult::Binary {
                    content_type: "image/png".to_string(),
                    body: bytes,
                },
                None => not_found(),
            }
        }
        // v1 `TokenInfoController`: the `apiKeyAuthentication`
        // endpoint — reports the username the credential authenticated as.
        "tokenInfo" => EndpointResult::Doc(vec![(
            "tokenInfo".to_string(),
            Value::map(vec![("username", Value::str_value(&identity.username))]),
        )]),
        // v1 `GetOpenSubsonicExtensionsController`: exactly
        // `apiKeyAuthentication` + `formPost`. `songLyrics` is deliberately
        // NOT claimed because `getLyrics` is a stub; `transcodeOffset` is
        // not claimed because no transcoding exists. Served without auth
        // (see `Route::PublicEndpoint`).
        "getOpenSubsonicExtensions" => EndpointResult::Doc(vec![(
            "openSubsonicExtensions".to_string(),
            Value::List(vec![
                Value::map(vec![
                    ("name", Value::str_value("apiKeyAuthentication")),
                    ("versions", Value::List(vec![Value::int_value(1)])),
                ]),
                Value::map(vec![
                    ("name", Value::str_value("formPost")),
                    ("versions", Value::List(vec![Value::int_value(1)])),
                ]),
            ]),
        )]),
        "getSongsByGenre" => {
            let genre = ok_or_return!(req(params, "genre")).to_lowercase();
            let (size, offset) = size_offset(params);
            let songs: Vec<Value> = store
                .all_songs()
                .iter()
                .filter(|s| s.genre.to_lowercase() == genre)
                .skip(offset)
                .take(size)
                .map(|s| starred_song_entry(store, identity, s))
                .collect();
            EndpointResult::Doc(vec![(
                "songsByGenre".to_string(),
                Value::map(vec![("song", Value::List(songs))]),
            )])
        }
        "getLyrics" => {
            let artist = ok_or_return!(req(params, "artist"));
            let title = ok_or_return!(req(params, "title"));
            let lyrics = store.lyrics(artist, title).map(|text| {
                Value::map(vec![
                    ("artist", Value::str_value(artist)),
                    ("title", Value::str_value(title)),
                    ("value", Value::str_value(text)),
                ])
            });
            EndpointResult::Doc(vec![(
                "lyrics".to_string(),
                lyrics.unwrap_or_else(|| Value::Map(vec![])),
            )])
        }
        "getLyricsBySongId" => {
            let id = ok_or_return!(req(params, "id"));
            match store.song(id) {
                Some(song) => {
                    let lyrics = store.lyrics(&song.artist, &song.title);
                    EndpointResult::Doc(vec![(
                        "lyricsList".to_string(),
                        lyrics
                            .map(|text| structured_lyrics(&song.artist, &song.title, &text))
                            .unwrap_or_else(|| Value::List(vec![])),
                    )])
                }
                None => not_found(),
            }
        }
        "getArtistInfo" | "getArtistInfo2" => {
            let id = ok_or_return!(req(params, "id"));
            if store.artist(id).is_none() {
                return not_found();
            }
            EndpointResult::Doc(vec![(
                "artistInfo".to_string(),
                Value::map(vec![
                    ("biography", Value::str_value("")),
                    ("musicBrainzId", Value::str_value("")),
                    ("lastFmUrl", Value::str_value("")),
                    ("smallImageUrl", Value::str_value("")),
                    ("mediumImageUrl", Value::str_value("")),
                    ("largeImageUrl", Value::str_value("")),
                ]),
            )])
        }
        "getAlbumInfo" | "getAlbumInfo2" => {
            let id = ok_or_return!(req(params, "id"));
            if store.album(id).is_none() {
                return not_found();
            }
            EndpointResult::Doc(vec![(
                "albumInfo".to_string(),
                Value::map(vec![
                    ("notes", Value::str_value("")),
                    ("musicBrainzId", Value::str_value("")),
                    ("lastFmUrl", Value::str_value("")),
                    ("smallImageUrl", Value::str_value("")),
                    ("mediumImageUrl", Value::str_value("")),
                    ("largeImageUrl", Value::str_value("")),
                ]),
            )])
        }
        "getPodcasts" => EndpointResult::Doc(vec![("podcasts".to_string(), Value::Map(vec![]))]),
        "getNewestPodcasts" => {
            EndpointResult::Doc(vec![("newestPodcasts".to_string(), Value::Map(vec![]))])
        }
        "refreshPodcasts" => EndpointResult::Doc(vec![]),
        "createPodcastChannel" => {
            let url = ok_or_return!(req(params, "url")).to_string();
            EndpointResult::Doc(vec![(
                "podcasts".to_string(),
                Value::map(vec![(
                    "channel",
                    Value::List(vec![Value::map(vec![
                        ("id", Value::str_value("pc-1")),
                        ("url", Value::str_value(url)),
                        ("title", Value::str_value("")),
                        ("description", Value::str_value("")),
                        ("status", Value::str_value("completed")),
                    ])]),
                )]),
            )])
        }
        "deletePodcastChannel" => {
            ok_or_return!(req(params, "id"));
            EndpointResult::Doc(vec![])
        }
        "getPlayQueue" => {
            let queue = store.queue(identity).unwrap_or(PlayQueue {
                username: identity.username.clone(),
                changed: NOW.into(),
                changed_by: identity.username.clone(),
                ..Default::default()
            });
            let mut entries = vec![
                ("username", Value::str_value(&queue.username)),
                ("changed", Value::str_value(&queue.changed)),
                ("changedBy", Value::str_value(&queue.changed_by)),
            ];
            if !queue.ids.is_empty() {
                entries.push(("current", Value::str_value(&queue.current)));
                entries.push(("position", Value::int_value(queue.position)));
                entries.push((
                    "entry",
                    Value::List(
                        queue
                            .ids
                            .iter()
                            .filter_map(|id| store.song(id))
                            .map(|s| starred_song_entry(store, identity, &s))
                            .collect(),
                    ),
                ));
            }
            EndpointResult::Doc(vec![("playQueue".to_string(), Value::map(entries))])
        }
        "savePlayQueue" => {
            let ids: Vec<String> = params
                .get_all("id")
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            store.save_queue(
                identity,
                PlayQueue {
                    username: identity.username.clone(),
                    current: params.get("current").unwrap_or("").to_string(),
                    position: params
                        .get("position")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    changed: NOW.into(),
                    changed_by: identity.username.clone(),
                    ids,
                },
            );
            EndpointResult::Doc(vec![])
        }
        "getBookmarks" => EndpointResult::Doc(vec![("bookmarks".to_string(), Value::Map(vec![]))]),
        _ => EndpointResult::Err(code::GENERIC, format!("Unknown endpoint {name}.")),
    }
}

fn search2<S: Library>(store: &S, identity: &Identity, query: &str) -> EndpointResult {
    let q = query.to_lowercase();
    let artists: Vec<Value> = store
        .artists()
        .iter()
        .filter(|a| a.name.to_lowercase().contains(&q))
        .map(|a| {
            Value::map(vec![
                ("id", Value::str_value(&a.id)),
                ("name", Value::str_value(&a.name)),
            ])
        })
        .collect();
    let albums: Vec<Value> = store
        .all_albums()
        .iter()
        .filter(|a| a.name.to_lowercase().contains(&q) || a.artist.to_lowercase().contains(&q))
        .map(|a| {
            let n = store.songs_for_album(&a.id).len() as i64;
            album_item(a, n)
        })
        .collect();
    let songs: Vec<Value> = store
        .all_songs()
        .iter()
        .filter(|s| song_matches(s, query))
        .map(|s| starred_song_entry(store, identity, s))
        .collect();
    let mut entries = Vec::new();
    if !artists.is_empty() {
        entries.push(("artist".to_string(), Value::List(artists)));
    }
    if !albums.is_empty() {
        entries.push(("album".to_string(), Value::List(albums)));
    }
    if !songs.is_empty() {
        entries.push(("song".to_string(), Value::List(songs)));
    }
    EndpointResult::Doc(vec![("searchResult2".to_string(), Value::Map(entries))])
}

fn star_unstar<S: Library>(
    store: &mut S,
    identity: &Identity,
    params: &Params,
    star: bool,
) -> EndpointResult {
    let ids = params.get_all("id");
    let album_ids = params.get_all("albumId");
    let artist_ids = params.get_all("artistId");
    if ids.is_empty() && album_ids.is_empty() && artist_ids.is_empty() {
        return missing();
    }
    let mut apply = |kind: StarKind, id: &str| {
        if star {
            store.star(identity, kind, id);
        } else {
            store.unstar(identity, kind, id);
        }
    };
    for id in ids {
        apply(StarKind::Song, id);
    }
    for id in album_ids {
        apply(StarKind::Album, id);
    }
    for id in artist_ids {
        apply(StarKind::Artist, id);
    }
    EndpointResult::Doc(vec![])
}

fn structured_lyrics(artist: &str, title: &str, text: &str) -> Value {
    let text = text.trim();
    if text.is_empty() {
        return Value::List(vec![]);
    }
    let mut offset = 0i64;
    let mut timed_lines = Vec::new();
    let mut plain_lines = Vec::new();
    // v1 ParsedLyrics splits all three newline forms, including bare CR.
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for raw_line in normalized.split('\n') {
        if let Some(value) = raw_line
            .trim()
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .and_then(|tag| tag.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("offset"))
            .and_then(|(_, value)| value.trim().parse::<i64>().ok())
        {
            offset = value;
            continue;
        }
        let mut content = raw_line.trim_start();
        let mut timestamps = Vec::new();
        while let Some(rest) = content.strip_prefix('[') {
            let Some(end) = rest.find(']') else {
                break;
            };
            let Some(start) = parse_lyric_timestamp(&rest[..end]) else {
                break;
            };
            timestamps.push(start);
            content = rest[end + 1..].trim_start();
        }
        if timestamps.is_empty() {
            if !is_lyric_metadata(raw_line) {
                plain_lines.push(Value::map(vec![("value", Value::str_value(raw_line))]));
            }
        } else {
            for start in timestamps {
                timed_lines.push((start, content.trim().to_string()));
            }
        }
    }
    let synced = !timed_lines.is_empty();
    let lines = if synced {
        timed_lines.sort_by_key(|(start, _)| *start);
        timed_lines
            .into_iter()
            .map(|(start, text)| {
                Value::map(vec![
                    ("start", Value::int_value(start)),
                    ("value", Value::str_value(text)),
                ])
            })
            .collect()
    } else {
        plain_lines
    };
    if lines.is_empty() {
        return Value::List(vec![]);
    }
    Value::map(vec![(
        "structuredLyrics",
        Value::List(vec![Value::map(vec![
            ("displayArtist", Value::str_value(artist)),
            ("displayTitle", Value::str_value(title)),
            ("lang", Value::str_value("und")),
            ("offset", Value::int_value(offset)),
            ("synced", Value::bool_value(synced)),
            ("line", Value::List(lines)),
        ])]),
    )])
}

fn is_lyric_metadata(line: &str) -> bool {
    let mut rest = line.trim();
    let mut found = false;
    while let Some(tag) = rest.strip_prefix('[') {
        let Some((tag, suffix)) = tag.split_once(']') else {
            return false;
        };
        let Some((name, _)) = tag.split_once(':') else {
            return false;
        };
        if !["ar", "ti", "al", "au", "length", "by", "offset", "re", "ve"]
            .iter()
            .any(|known| name.trim_end().eq_ignore_ascii_case(known))
        {
            return false;
        }
        found = true;
        rest = suffix.trim_start();
    }
    found && rest.is_empty()
}

fn parse_lyric_timestamp(value: &str) -> Option<i64> {
    let (minutes, seconds) = value.split_once(':')?;
    let (seconds, fraction) = seconds.split_once(['.', ':']).unwrap_or((seconds, ""));
    if !(1..=2).contains(&minutes.len())
        || seconds.len() != 2
        || !minutes.bytes().all(|byte| byte.is_ascii_digit())
        || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        || (!fraction.is_empty()
            && (fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit())))
        || (fraction.is_empty() && value.ends_with(['.', ':']))
    {
        return None;
    }
    let minutes = minutes.parse::<i64>().ok()?;
    let seconds = seconds.parse::<i64>().ok()?;
    let millis = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction.parse::<i64>().ok()?,
    };
    Some((minutes * 60 + seconds) * 1000 + millis)
}

fn song_matches(song: &Song, query: &str) -> bool {
    let q = query.to_lowercase();
    song.title.to_lowercase().contains(&q)
        || song.album.to_lowercase().contains(&q)
        || song.artist.to_lowercase().contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_keeps_utf8_intact() {
        let params = Params::parse("query=Bj%C3%B6rk&name=Caf%C3%A9");
        assert_eq!(params.get("query"), Some("Björk"));
        assert_eq!(params.get("name"), Some("Café"));
    }

    #[test]
    fn decode_never_panics_on_stray_percent() {
        // Non-char-boundary slicing used to panic here; byte indexing cannot.
        let params = Params::parse("q=%aé&other=%zz%");
        assert!(params.get("q").is_some());
        assert_eq!(params.get("other"), Some("%zz%"));
        let params = Params::parse("q=%");
        assert_eq!(params.get("q"), Some("%"));
    }
}
