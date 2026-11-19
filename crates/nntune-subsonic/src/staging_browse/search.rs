use super::{album_fields, charged_list, song_item, MemberBrowse};
use crate::construction_budget::{BudgetExceeded, ConstructionBudget};
use crate::endpoints::Params;
use crate::envelope::{charged_error_document, charged_ok_document, code};
use crate::value::{copy_text, Value};
use std::cmp::Ordering;

// Rust 1.98.1 alloc::str::to_lowercase starts with the input byte capacity
// and pushes one scalar at a time. RawVec grows to max(8, 2 * cap, need).
// Contextual sigma emits two bytes either way. Charge every capacity before
// calling std, including relocation overlap, then check the returned capacity.
fn lowercase(budget: &mut ConstructionBudget, text: &str) -> Result<String, BudgetExceeded> {
    let mut cap = text.len();
    if cap != 0 {
        budget.charge(cap, 1, 1)?;
    }
    let mut len = 0usize;
    for ch in text.chars().flat_map(char::to_lowercase) {
        len = len.checked_add(ch.len_utf8()).ok_or(BudgetExceeded)?;
        if len > cap {
            cap = cap.checked_mul(2).ok_or(BudgetExceeded)?.max(len).max(8);
            budget.charge(cap, 1, 1)?;
        }
    }
    let lower = text.to_lowercase();
    if lower.capacity() != cap || lower.len() != len {
        return Err(BudgetExceeded);
    }
    Ok(lower)
}

fn matches<const N: usize>(
    budget: &mut ConstructionBudget,
    query: &str,
    fields: [&str; N],
) -> Result<bool, BudgetExceeded> {
    if query.is_empty() {
        return Ok(true);
    }
    for field in fields {
        if lowercase(budget, field)?.contains(query) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn page<'a, T>(
    budget: &mut ConstructionBudget,
    rows: &'a [T],
    offset: usize,
    count: usize,
    mut includes: impl FnMut(&mut ConstructionBudget, &'a T) -> Result<bool, BudgetExceeded>,
    compare: impl Fn(&T, &T) -> Ordering,
    build: impl FnMut(&mut ConstructionBudget, &'a T) -> Result<Value, BudgetExceeded>,
) -> Result<Vec<Value>, BudgetExceeded> {
    if count == 0 || offset >= rows.len() {
        return Ok(Vec::new());
    }
    let mut found = budget.allocate_exact::<(usize, &T)>(rows.len())?;
    for (i, row) in rows.iter().enumerate() {
        if includes(budget, row)? {
            found.push((i, row));
        }
    }
    found.sort_unstable_by(|(i, a), (j, b)| compare(a, b).then(i.cmp(j)));
    charged_list(
        budget,
        found.iter().skip(offset).take(count).map(|&(_, row)| row),
        build,
    )
}

impl MemberBrowse<'_> {
    pub(super) fn search_document(
        &self,
        budget: &mut ConstructionBudget,
        endpoint: &str,
        params: &Params,
    ) -> Result<Value, BudgetExceeded> {
        let legacy = endpoint == "search2";
        let query = params.get("query").unwrap_or("");
        let missing = |budget: &mut ConstructionBudget| {
            charged_error_document(
                budget,
                code::REQUIRED_PARAM_MISSING,
                "Required parameter is missing.",
            )
        };
        if legacy && query.is_empty() {
            return missing(budget);
        }
        let (offset, [artist_count, album_count, song_count]) = if legacy {
            (0, [self.artists.len(), self.albums.len(), self.songs.len()])
        } else {
            let default = if query.is_empty() {
                params
                    .get("size")
                    .or_else(|| params.get("count"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(10)
            } else {
                20
            };
            let count = |key| match params.get(key) {
                None => Some(default),
                Some(value) => value.parse::<usize>().ok().filter(|&n| n <= 500),
            };
            let (Some(artists), Some(albums), Some(songs)) = (
                count("artistCount"),
                count("albumCount"),
                count("songCount"),
            ) else {
                return missing(budget);
            };
            let offset = params
                .get("offset")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            (offset, [artists, albums, songs])
        };
        let query = lowercase(budget, query)?;
        let artists = page(
            budget,
            self.artists,
            offset,
            artist_count,
            |budget, row| matches(budget, &query, [&row.name]),
            |_, _| Ordering::Equal,
            |budget, row| {
                let id = Value::charged_str(budget, &row.id)?;
                let name = Value::charged_str(budget, &row.name)?;
                if legacy {
                    Value::charged_map(budget, [("id", id), ("name", name)])
                } else {
                    let count = Value::Int(self.albums_for(row).count() as i64);
                    Value::charged_map(budget, [("id", id), ("name", name), ("albumCount", count)])
                }
            },
        )?;
        let albums = page(
            budget,
            self.albums,
            offset,
            album_count,
            |budget, row| matches(budget, &query, [&row.name, &row.artist]),
            |_, _| Ordering::Equal,
            |budget, row| {
                let fields = album_fields(budget, row, self.songs_for(row).count() as i64)?;
                Value::charged_map(budget, fields)
            },
        )?;
        // The ordinary search orders by recording ID, preserving catalogue
        // occurrence order among equal IDs. Do not deduplicate before paging.
        let songs = page(
            budget,
            self.songs,
            offset,
            song_count,
            |budget, row| matches(budget, &query, [&row.title, &row.album, &row.artist]),
            |a, b| a.id.cmp(&b.id),
            song_item,
        )?;
        let entries = [("artist", artists), ("album", albums), ("song", songs)];
        let include = |(_, items): &(&str, Vec<Value>)| !legacy || !items.is_empty();
        let mut payload =
            budget.allocate_exact(entries.iter().filter(|entry| include(entry)).count())?;
        for (key, items) in entries.into_iter().filter(include) {
            payload.push((copy_text(budget, key)?, Value::List(items)));
        }
        let key = if legacy {
            "searchResult2"
        } else {
            "searchResult3"
        };
        charged_ok_document(budget, [(key, Value::Map(payload))])
    }
}

#[cfg(any(test, feature = "ledger-wasm-test"))]
#[cfg_attr(test, test)]
#[doc(hidden)]
pub fn search_boundaries() {
    use crate::library::{Library, MemStore};

    let store = MemStore::seeded();
    let mut first = store.all_songs()[0].clone();
    first.id = "a".into();
    first.title = "ΑΘΗΝΑΣ İ".into();
    let mut second = first.clone();
    second.disc = first.disc + 1;
    let mut later = first.clone();
    later.id = "z".into();
    let songs = [later, first, second];
    let view = MemberBrowse {
        artists: &[],
        albums: &[],
        songs: &songs,
        now_millis: 0,
    };
    for (endpoint, query, ids) in [
        ("search3", "query=αθηνας+İ&offset=1&songCount=1", &["a"][..]),
        ("search3", "query=αθηνας&songCount=2", &["a", "a"]),
        ("search3", "query=no-match", &[]),
        (
            "search2",
            "query=αθηνας&offset=1&songCount=1",
            &["a", "a", "z"],
        ),
    ] {
        let params = Params::parse(query);
        let build =
            |budget: &mut ConstructionBudget| view.search_document(budget, endpoint, &params);
        let mut probe = ConstructionBudget::new(usize::MAX);
        let actual = build(&mut probe).unwrap();
        let key = if endpoint == "search2" {
            "searchResult2"
        } else {
            "searchResult3"
        };
        let Value::List(results) = super::field(super::field(&actual, key), "song") else {
            panic!("expected song page for {endpoint} {query}")
        };
        assert_eq!(results.len(), ids.len());
        assert_eq!(results.capacity(), ids.len());
        for (item, id) in results.iter().zip(ids) {
            assert_eq!(super::field(item, "id"), &Value::Str((*id).into()));
        }
        if query.contains("offset=1") && endpoint == "search3" {
            assert_eq!(
                super::field(&results[0], "discNumber"),
                &Value::Int(songs[2].disc)
            );
        }
        assert_eq!(
            build(&mut ConstructionBudget::new(probe.charged())).unwrap(),
            actual
        );
        assert_eq!(
            build(&mut ConstructionBudget::new(probe.charged() - 1)),
            Err(BudgetExceeded)
        );
    }
    let mut expected = ConstructionBudget::new(usize::MAX);
    expected.charge(32, 1, 1).unwrap();
    expected.charge(64, 1, 1).unwrap();
    let text = "İ".repeat(16);
    let mut exact = ConstructionBudget::new(expected.charged());
    let lower = lowercase(&mut exact, &text).unwrap();
    assert_eq!(lower, text.to_lowercase());
    assert_eq!(lower.capacity(), 64);
    let mut short = ConstructionBudget::new(expected.charged() - 1);
    assert_eq!(lowercase(&mut short, &text), Err(BudgetExceeded));
}

#[cfg(any(test, feature = "ledger-wasm-test"))]
#[cfg_attr(test, test)]
#[doc(hidden)]
pub fn search_matches_dispatch() {
    use crate::auth::Identity;
    use crate::endpoints::{handle_endpoint, EndpointResult};
    use crate::envelope::{error_document, ok_document};
    use crate::library::{Library, MemStore};

    let mut store = MemStore::seeded();
    let artists = store.artists();
    let albums = store.all_albums();
    let songs = store.all_songs();
    let view = MemberBrowse {
        artists: &artists,
        albums: &albums,
        songs: &songs,
        now_millis: store.artists_last_modified(),
    };
    let identity = Identity::for_public_route();
    for (endpoint, query) in [
        ("search2", ""),
        ("search2", "query="),
        ("search2", "query=no-match"),
        (
            "search2",
            "query=Test&artistCount=invalid&albumCount=0&songCount=0",
        ),
        ("search3", ""),
        ("search3", "count=1"),
        ("search3", "size=0&count=1"),
        ("search3", "size=invalid&offset=invalid"),
        ("search3", "size=2&offset=1&artistCount=0&songCount=500"),
        ("search3", "artistCount=501"),
        ("search3", "albumCount=-1"),
        ("search3", "songCount="),
    ] {
        let params = Params::parse(query);
        let expected = match handle_endpoint(&mut store, &identity, endpoint, &params, false) {
            EndpointResult::Doc(payload) => ok_document(payload),
            EndpointResult::Err(code, message) => error_document(code, &message),
            _ => panic!("expected document"),
        };
        let mut budget = ConstructionBudget::new(8 * 1024 * 1024);
        let actual = view.document(&mut budget, endpoint, &params, |_| {
            panic!("search must not invoke the direct ID resolver")
        });
        assert_eq!(actual.unwrap(), Some(expected), "{endpoint} {query}");
    }
}
