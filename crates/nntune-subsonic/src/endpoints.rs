//! Endpoint handlers: one arm per supported op, in v1 route order.
//!
//! Conventions: missing required params are error 10, unknown ids error 70
//! (both HTTP 200 inside the envelope, as in v1). `maxBitRate`/`timeOffset`
//! on the byte routes are accepted and ignored — no live transcoding, as in
//! v1. Mutations operate on the injected `Library`, so fixtures exercise the
//! real lifecycle (create → get → update → delete).

use crate::auth::Identity;
use crate::envelope::code;
use crate::library::{Album, Artist, Library, PlayQueue, Playlist, Song, StarKind, NOW};
use crate::value::Value;

/// Parsed query string: first value wins for `get`, all values for `get_all`.
#[derive(Clone, Debug, Default)]
pub struct Params {
    pairs: Vec<(String, String)>,
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

fn not_found(message: &str) -> EndpointResult {
    EndpointResult::Err(code::NOT_FOUND, message.to_string())
}

fn req<'a>(params: &'a Params, name: &str) -> Result<&'a str, EndpointResult> {
    match params.get(name) {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(missing()),
    }
}

// --- shared shapes (key order is the wire order) ---

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
        ("bitRate", Value::int_value(song.bitrate)),
        ("path", Value::str_value(&song.path)),
        ("playCount", Value::int_value(song.play_count)),
        ("discNumber", Value::int_value(song.disc)),
        ("created", Value::str_value(&song.created)),
        ("albumId", Value::str_value(&song.album_id)),
        ("artistId", Value::str_value(&song.artist_id)),
        ("type", Value::str_value("music")),
        ("isVideo", Value::bool_value(false)),
    ])
}

fn starred_song_entry<S: Library>(store: &S, identity: &Identity, song: &Song) -> Value {
    let Value::Map(mut entries) = song_entry(song) else {
        unreachable!()
    };
    if store.starred(identity, StarKind::Song).contains(&song.id) {
        entries.push(("starred".to_string(), Value::str_value(NOW)));
    }
    if let Some(rating) = store.rating(identity, &song.id) {
        entries.push(("userRating".to_string(), Value::int_value(rating)));
    }
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
        "getArtists" | "getIndexes" => {
            let mut groups: std::collections::BTreeMap<String, Vec<Value>> =
                std::collections::BTreeMap::new();
            for artist in store.artists() {
                let albums = store.albums_for_artist(&artist.id).len() as i64;
                let key = artist
                    .name
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_else(|| "-".to_string());
                groups
                    .entry(key)
                    .or_default()
                    .push(artist_item(&artist, albums));
            }
            let index: Vec<Value> = groups
                .into_iter()
                .map(|(name, artists)| {
                    Value::map(vec![
                        ("name", Value::str_value(name)),
                        ("artist", Value::List(artists)),
                    ])
                })
                .collect();
            let artists = if index.is_empty() {
                Value::Map(vec![])
            } else {
                Value::map(vec![("index", Value::List(index))])
            };
            EndpointResult::Doc(vec![("artists".to_string(), artists)])
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
                not_found("Directory not found.")
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
                None => not_found("Artist not found."),
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
                None => not_found("Album not found."),
            }
        }
        "getSong" => {
            let id = ok_or_return!(req(params, "id"));
            match store.song(id) {
                Some(song) => EndpointResult::Doc(vec![(
                    "song".to_string(),
                    starred_song_entry(store, identity, &song),
                )]),
                None => not_found("Song not found."),
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
            let query = ok_or_return!(req(params, "query"));
            let matched: Vec<Value> = store
                .all_songs()
                .iter()
                .filter(|s| song_matches(s, query))
                .map(|s| starred_song_entry(store, identity, s))
                .collect();
            let result = if matched.is_empty() {
                Value::Map(vec![])
            } else {
                Value::map(vec![("match", Value::List(matched))])
            };
            EndpointResult::Doc(vec![("searchResult3".to_string(), result)])
        }
        "getAlbumList" | "getAlbumList2" => {
            let list_type = ok_or_return!(req(params, "type"));
            let (size, offset) = size_offset(params);
            let mut albums = store.all_albums();
            if list_type == "starred" {
                let starred_albums: std::collections::HashSet<String> = store
                    .starred(identity, StarKind::Album)
                    .into_iter()
                    .collect();
                let via_songs: std::collections::HashSet<String> = store
                    .starred(identity, StarKind::Song)
                    .into_iter()
                    .filter_map(|id| store.song(&id))
                    .map(|s| s.album_id)
                    .collect();
                albums.retain(|a| starred_albums.contains(&a.id) || via_songs.contains(&a.id));
            }
            // Deterministic newest-first; single-seed store keeps fixtures stable.
            albums.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id)));
            let page: Vec<Value> = albums
                .into_iter()
                .skip(offset)
                .take(size)
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
                None => not_found("Song not found."),
            }
        }
        "getCoverArt" => {
            let id = ok_or_return!(req(params, "id"));
            match store.cover(id) {
                Some(bytes) => EndpointResult::Binary {
                    content_type: "image/png".to_string(),
                    body: bytes,
                },
                None => not_found("Cover art not found."),
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
                Some(_) => EndpointResult::Err(code::NOT_AUTHORIZED, "Not authorized.".to_string()),
                None => not_found("Playlist not found."),
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
                Err(_) => not_found("Playlist not found."),
            }
        }
        "deletePlaylist" => {
            let id = ok_or_return!(req(params, "id"));
            match store.delete_playlist(identity, id) {
                Ok(()) => EndpointResult::Doc(vec![]),
                Err((error_code, message)) if error_code == code::NOT_AUTHORIZED => {
                    EndpointResult::Err(error_code, message)
                }
                Err(_) => not_found("Playlist not found."),
            }
        }
        "scrobble" => {
            let id = ok_or_return!(req(params, "id"));
            if store.scrobble(identity, id) {
                EndpointResult::Doc(vec![])
            } else {
                not_found("Song not found.")
            }
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
                not_found("Requested data was not found.")
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
            let (size, _) = size_offset(params);
            let songs: Vec<Value> = store
                .all_songs()
                .iter()
                .take(size)
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
                return EndpointResult::Err(code::NOT_AUTHORIZED, "Not authorized.".to_string());
            }
            match store.user_email(username) {
                Some(email) => EndpointResult::Doc(vec![(
                    "user".to_string(),
                    Value::map(vec![
                        ("username", Value::str_value(username)),
                        ("email", Value::str_value(email)),
                        ("scrobblingEnabled", Value::bool_value(true)),
                        ("adminRole", Value::bool_value(username == "admin")),
                        ("settingsRole", Value::bool_value(false)),
                        ("downloadRole", Value::bool_value(true)),
                        ("uploadRole", Value::bool_value(false)),
                        ("playlistRole", Value::bool_value(true)),
                        ("coverArtRole", Value::bool_value(true)),
                        ("commentRole", Value::bool_value(true)),
                        ("podcastRole", Value::bool_value(true)),
                        ("streamRole", Value::bool_value(true)),
                        ("jukeboxRole", Value::bool_value(false)),
                        ("shareRole", Value::bool_value(false)),
                        ("folder", Value::List(vec![Value::int_value(1)])),
                    ]),
                )]),
                None => not_found("User not found."),
            }
        }
        "getAvatar" => {
            let username = ok_or_return!(req(params, "username"));
            match store.avatar(username) {
                Some(bytes) => EndpointResult::Binary {
                    content_type: "image/png".to_string(),
                    body: bytes,
                },
                None => not_found("User not found."),
            }
        }
        "getOpenSubsonicExtensions" => EndpointResult::Doc(vec![(
            "openSubsonicExtensions".to_string(),
            Value::List(vec![Value::map(vec![
                ("name", Value::str_value("songLyrics")),
                ("versions", Value::List(vec![Value::int_value(1)])),
            ])]),
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
            match store.lyrics(artist, title) {
                Some(text) => EndpointResult::Doc(vec![(
                    "lyrics".to_string(),
                    Value::map(vec![
                        ("artist", Value::str_value(artist)),
                        ("title", Value::str_value(title)),
                        ("value", Value::str_value(text)),
                    ]),
                )]),
                None => not_found("Lyrics not found."),
            }
        }
        "getLyricsBySongId" => {
            let id = ok_or_return!(req(params, "id"));
            match store.song(id) {
                Some(song) => match store.lyrics(&song.artist.clone(), &song.title.clone()) {
                    Some(text) => EndpointResult::Doc(vec![(
                        "lyrics".to_string(),
                        Value::map(vec![
                            ("artist", Value::str_value(&song.artist)),
                            ("title", Value::str_value(&song.title)),
                            ("value", Value::str_value(text)),
                        ]),
                    )]),
                    None => not_found("Lyrics not found."),
                },
                None => not_found("Song not found."),
            }
        }
        "getArtistInfo" | "getArtistInfo2" => {
            let id = ok_or_return!(req(params, "id"));
            if store.artist(id).is_none() {
                return not_found("Artist not found.");
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
                return not_found("Album not found.");
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
    let mut apply = |kind: StarKind, id: &str| -> Result<(), EndpointResult> {
        if star {
            if store.star(identity, kind, id) {
                Ok(())
            } else {
                Err(not_found("Requested data was not found."))
            }
        } else {
            store.unstar(identity, kind, id);
            Ok(())
        }
    };
    for id in ids {
        ok_or_return!(apply(StarKind::Song, id));
    }
    for id in album_ids {
        ok_or_return!(apply(StarKind::Album, id));
    }
    for id in artist_ids {
        ok_or_return!(apply(StarKind::Artist, id));
    }
    EndpointResult::Doc(vec![])
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
