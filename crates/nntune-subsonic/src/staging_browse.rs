use crate::construction_budget::{BudgetExceeded, ConstructionBudget};
use crate::endpoints::Params;
use crate::envelope::{charged_error_document, charged_ok_document, code};
use crate::library::{Album, Artist, Song};
use crate::value::Value;

mod album_list;

#[cfg(any(test, feature = "ledger-wasm-test"))]
pub use album_list::album_list_boundaries;

#[cfg(any(test, feature = "ledger-wasm-test"))]
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(fields) = value else {
        panic!("expected map")
    };
    &fields.iter().find(|(name, _)| name == key).unwrap().1
}

/// Borrows a member snapshot in browse order. The adapter enforces membership
/// and keeps raw occurrence provenance for direct recording-ID resolution.
pub struct MemberBrowse<'a> {
    pub artists: &'a [Artist],
    pub albums: &'a [Album],
    pub songs: &'a [Song],
    pub now_millis: i64,
}

impl<'a> MemberBrowse<'a> {
    pub fn supports(name: &str) -> bool {
        matches!(
            name,
            "getArtists"
                | "getIndexes"
                | "getArtist"
                | "getAlbum"
                | "getSong"
                | "getMusicDirectory"
                | "getAlbumList"
                | "getAlbumList2"
        )
    }

    pub fn document(
        &self,
        budget: &mut ConstructionBudget,
        name: &str,
        params: &Params,
        resolve_song: impl FnOnce(&str) -> Option<&'a Song>,
    ) -> Result<Option<Value>, BudgetExceeded> {
        if !Self::supports(name) {
            return Ok(None);
        }
        let doc = match name {
            "getArtists" | "getIndexes" => self.index_document(budget, name)?,
            "getAlbumList" | "getAlbumList2" => self.album_list_document(budget, name, params)?,
            _ => {
                let Some(id) = params.get("id").filter(|id| !id.is_empty()) else {
                    return charged_error_document(
                        budget,
                        code::REQUIRED_PARAM_MISSING,
                        "Required parameter is missing.",
                    )
                    .map(Some);
                };
                let album = || self.albums.iter().find(|album| album.id == id);
                let artist = || self.artists.iter().find(|artist| artist.id == id);
                match name {
                    "getSong" => match resolve_song(id) {
                        Some(song) => {
                            let item = song_item(budget, song)?;
                            charged_ok_document(budget, [("song", item)])?
                        }
                        None => missing_item(budget, "Song not found.")?,
                    },
                    "getAlbum" => match album() {
                        Some(album) => {
                            let songs = self.album_songs(budget, album)?;
                            let [id, name, artist, artist_id, art, count, duration, created] =
                                album_fields(budget, album, songs.len() as i64)?;
                            let song = ("song", Value::List(songs));
                            let payload = Value::charged_map(
                                budget,
                                [
                                    id, name, artist, artist_id, art, count, duration, created,
                                    song,
                                ],
                            )?;
                            charged_ok_document(budget, [("album", payload)])?
                        }
                        None => missing_item(budget, "Album not found.")?,
                    },
                    "getArtist" => match artist() {
                        Some(artist) => {
                            let albums = self.artist_albums(budget, artist)?;
                            let id = Value::charged_str(budget, &artist.id)?;
                            let name = Value::charged_str(budget, &artist.name)?;
                            let payload = Value::charged_map(
                                budget,
                                [
                                    ("id", id),
                                    ("name", name),
                                    ("albumCount", Value::Int(albums.len() as i64)),
                                    ("album", Value::List(albums)),
                                ],
                            )?;
                            charged_ok_document(budget, [("artist", payload)])?
                        }
                        None => missing_item(budget, "Artist not found.")?,
                    },
                    "getMusicDirectory" => {
                        let (id, label, children) = if let Some(album) = album() {
                            (&album.id, &album.name, self.album_songs(budget, album)?)
                        } else if let Some(artist) = artist() {
                            (
                                &artist.id,
                                &artist.name,
                                self.artist_albums(budget, artist)?,
                            )
                        } else {
                            return missing_item(budget, "Directory not found.").map(Some);
                        };
                        let id = Value::charged_str(budget, id)?;
                        let label = Value::charged_str(budget, label)?;
                        let payload = Value::charged_map(
                            budget,
                            [
                                ("id", id),
                                ("name", label),
                                ("child", Value::List(children)),
                            ],
                        )?;
                        charged_ok_document(budget, [("directory", payload)])?
                    }
                    _ => return Ok(None),
                }
            }
        };
        Ok(Some(doc))
    }

    fn songs_for(&self, album: &'a Album) -> impl Iterator<Item = &'a Song> + Clone {
        let id = album.id.as_str();
        self.songs.iter().filter(move |song| song.album_id == id)
    }

    fn albums_for(&self, artist: &'a Artist) -> impl Iterator<Item = &'a Album> + Clone {
        let id = artist.id.as_str();
        self.albums
            .iter()
            .filter(move |album| album.artist_id == id)
    }

    fn album_songs(
        &self,
        budget: &mut ConstructionBudget,
        album: &'a Album,
    ) -> Result<Vec<Value>, BudgetExceeded> {
        // Do not deduplicate by recording ID: every member occurrence is a row.
        charged_list(budget, self.songs_for(album), song_item)
    }

    fn artist_albums(
        &self,
        budget: &mut ConstructionBudget,
        artist: &'a Artist,
    ) -> Result<Vec<Value>, BudgetExceeded> {
        charged_list(budget, self.albums_for(artist), |budget, album| {
            let fields = album_fields(budget, album, self.songs_for(album).count() as i64)?;
            Value::charged_map(budget, fields)
        })
    }

    fn index_document(
        &self,
        budget: &mut ConstructionBudget,
        endpoint: &str,
    ) -> Result<Value, BudgetExceeded> {
        let mut ordered = budget.allocate_exact(self.artists.len())?;
        for (position, artist) in self.artists.iter().enumerate() {
            ordered.push((index_key(&artist.name), position, artist));
        }
        // The position tie-break retains browse order without a stable-sort allocation.
        ordered.sort_unstable_by_key(|&(key, position, _)| (key, position));
        let groups = ordered.chunk_by(|left, right| left.0 == right.0);
        let mut index = budget.allocate_exact(groups.clone().count())?;
        for group in groups {
            let key = &group[0].0;
            let len = key.iter().position(|&byte| byte == 0).unwrap_or(key.len());
            let name = std::str::from_utf8(&key[..len]).map_err(|_| BudgetExceeded)?;
            let label = Value::charged_str(budget, name)?;
            let mut artists = budget.allocate_exact(group.len())?;
            for (_, _, artist) in group {
                let id = Value::charged_str(budget, &artist.id)?;
                let name = Value::charged_str(budget, &artist.name)?;
                artists.push(Value::charged_map(
                    budget,
                    [
                        ("id", id),
                        ("name", name),
                        (
                            "albumCount",
                            Value::Int(self.albums_for(artist).count() as i64),
                        ),
                    ],
                )?);
            }
            index.push(Value::charged_map(
                budget,
                [("name", label), ("artist", Value::List(artists))],
            )?);
        }
        let articles = (
            "ignoredArticles",
            Value::charged_str(budget, IGNORED_ARTICLES)?,
        );
        let index = ("index", Value::List(index));
        if endpoint == "getIndexes" {
            let modified = ("lastModified", Value::Int(self.now_millis));
            let payload = Value::charged_map(budget, [modified, articles, index])?;
            charged_ok_document(budget, [("indexes", payload)])
        } else {
            let payload = Value::charged_map(budget, [articles, index])?;
            charged_ok_document(budget, [("artists", payload)])
        }
    }
}

fn charged_list<T>(
    budget: &mut ConstructionBudget,
    source: impl Iterator<Item = T> + Clone,
    mut build: impl FnMut(&mut ConstructionBudget, T) -> Result<Value, BudgetExceeded>,
) -> Result<Vec<Value>, BudgetExceeded> {
    let mut values = budget.allocate_exact(source.clone().count())?;
    for item in source {
        values.push(build(budget, item)?);
    }
    Ok(values)
}

const IGNORED_ARTICLES: &str = "The El La Los Las Le Les";

fn index_key(name: &str) -> [u8; 3] {
    let name = name.trim();
    let stripped = IGNORED_ARTICLES
        .split(' ')
        .find_map(|article| {
            name.get(..article.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(article))
                .and_then(|_| name.get(article.len()..))
                .filter(|suffix| suffix.starts_with(' '))
                .map(str::trim)
        })
        .unwrap_or(name);
    let other = [b'#', 0, 0];
    let Some(first) = stripped.chars().next() else {
        return other;
    };
    let mut key = [0; 3];
    for (index, upper) in first.to_uppercase().enumerate() {
        if !upper.is_ascii_alphabetic() || index >= key.len() {
            return other;
        }
        key[index] = upper as u8;
    }
    key
}

fn missing_item(
    budget: &mut ConstructionBudget,
    message: &'static str,
) -> Result<Value, BudgetExceeded> {
    charged_error_document(budget, code::NOT_FOUND, message)
}

fn song_item(budget: &mut ConstructionBudget, song: &Song) -> Result<Value, BudgetExceeded> {
    let mut text = |s: &str| Value::charged_str(budget, s);
    let fields = [
        ("id", text(&song.id)?),
        ("parent", text(&song.album_id)?),
        ("isDir", Value::Bool(false)),
        ("title", text(&song.title)?),
        ("album", text(&song.album)?),
        ("artist", text(&song.artist)?),
        ("track", Value::Int(song.track)),
        ("year", Value::Int(song.year)),
        ("genre", text(&song.genre)?),
        ("coverArt", text(&song.cover_art)?),
        ("size", Value::Int(song.size)),
        ("contentType", text(&song.content_type)?),
        ("suffix", text(&song.suffix)?),
        ("duration", Value::Int(song.duration)),
        ("created", text(&song.created)?),
        ("albumId", text(&song.album_id)?),
        ("artistId", text(&song.artist_id)?),
        ("type", text("music")?),
        ("discNumber", Value::Int(song.disc)),
        ("isVideo", Value::Bool(false)),
        ("transcodedContentType", text("")?),
        ("transcodedSuffix", text("")?),
    ];
    Value::charged_map(budget, fields)
}

fn album_fields(
    budget: &mut ConstructionBudget,
    album: &Album,
    songs: i64,
) -> Result<[(&'static str, Value); 8], BudgetExceeded> {
    Ok([
        ("id", Value::charged_str(budget, &album.id)?),
        ("name", Value::charged_str(budget, &album.name)?),
        ("artist", Value::charged_str(budget, &album.artist)?),
        ("artistId", Value::charged_str(budget, &album.artist_id)?),
        ("coverArt", Value::charged_str(budget, &album.id)?),
        ("songCount", Value::Int(songs)),
        ("duration", Value::Int(album.duration)),
        ("created", Value::charged_str(budget, &album.created)?),
    ])
}

#[cfg(any(test, feature = "ledger-wasm-test"))]
#[cfg_attr(test, test)]
#[doc(hidden)]
pub fn member_browse_boundaries() {
    use crate::auth::Identity;
    use crate::endpoints::{handle_endpoint, EndpointResult};
    use crate::envelope::{error_document, ok_document};
    use crate::library::{Library, MemStore};

    let mut ordinary = MemStore::seeded();
    let artists = ordinary.artists();
    let albums = ordinary.all_albums();
    let songs = ordinary.all_songs();
    let view = MemberBrowse {
        artists: &artists,
        albums: &albums,
        songs: &songs,
        now_millis: ordinary.artists_last_modified(),
    };
    let id = Identity::for_public_route();
    for (name, query) in [
        ("getArtists", ""),
        ("getIndexes", ""),
        ("getArtist", "id=ar-1"),
        ("getAlbum", "id=al-1"),
        ("getMusicDirectory", "id=al-1"),
        ("getMusicDirectory", "id=ar-1"),
        ("getSong", "id=so-1"),
        ("getSong", "id=private"),
        ("getAlbum", ""),
    ] {
        let params = Params::parse(query);
        let build = |budget: &mut ConstructionBudget| {
            view.document(budget, name, &params, |id| {
                songs.iter().find(|song| song.id == id)
            })
        };
        let mut probe = ConstructionBudget::new(8 * 1024 * 1024);
        let actual = build(&mut probe).unwrap().unwrap();
        let expected = match handle_endpoint(&mut ordinary, &id, name, &params, false) {
            EndpointResult::Doc(payload) => ok_document(payload),
            EndpointResult::Err(code, message) => error_document(code, &message),
            _ => panic!("expected document"),
        };
        assert_eq!(actual, expected, "{name} {query}");
        let mut exact = ConstructionBudget::new(probe.charged());
        assert_eq!(build(&mut exact).unwrap(), Some(actual));
        let mut short = ConstructionBudget::new(probe.charged() - 1);
        assert_eq!(build(&mut short), Err(BudgetExceeded));
    }
    let mut repeats = [songs[0].clone(), songs[0].clone()];
    repeats[1].disc = 2;
    repeats[1].track = 1;
    let view = MemberBrowse {
        songs: &repeats,
        ..view
    };
    let mut budget = ConstructionBudget::new(8 * 1024 * 1024);
    let occurrences = view.album_songs(&mut budget, &albums[0]).unwrap();
    assert_eq!((occurrences.len(), occurrences.capacity()), (2, 2));
    type Entry = (String, Value);
    let mut expected = ConstructionBudget::new(usize::MAX);
    expected
        .charge(2, size_of::<Value>(), align_of::<Value>())
        .unwrap();
    for (occurrence, disc) in occurrences.iter().zip([1, 2]) {
        let Value::Map(fields) = occurrence else {
            panic!("expected song")
        };
        assert_eq!(fields.len(), fields.capacity());
        expected
            .charge(fields.len(), size_of::<Entry>(), align_of::<Entry>())
            .unwrap();
        for (key, value) in fields {
            expected.charge(key.len(), 1, 1).unwrap();
            let Value::Str(text) = value else { continue };
            if !text.is_empty() {
                expected.charge(text.len(), 1, 1).unwrap();
            }
        }
        assert_eq!(fields[0].1, Value::Str("so-1".into()));
        assert!(fields
            .iter()
            .any(|(key, value)| key == "discNumber" && *value == Value::Int(disc)));
    }
    assert_eq!(budget.charged(), expected.charged());
    let params = Params::parse("id=so-1");
    let declined = view.document(&mut budget, "getSong", &params, |_| None);
    let not_found = error_document(code::NOT_FOUND, "Song not found.");
    assert_eq!(declined.unwrap(), Some(not_found));
    assert_eq!(index_key("The ßeta"), [b'S', b'S', 0]);
    assert_eq!(index_key("Les Æthers"), [b'#', 0, 0]);
}
