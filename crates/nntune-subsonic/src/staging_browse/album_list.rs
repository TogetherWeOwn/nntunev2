use super::{album_fields, charged_list, page_param, MemberBrowse, MAX_PAGE_OFFSET, MAX_PAGE_SIZE};
use crate::construction_budget::{BudgetExceeded, ConstructionBudget};
use crate::endpoints::Params;
use crate::envelope::{charged_error_document, charged_ok_document, code};
use crate::library::Album;
use crate::value::Value;
use std::cmp::Ordering;

#[derive(Clone, Copy)]
enum AlbumOrder {
    Id,
    Snapshot,
    Created,
    Curated,
    Name,
    Artist { catalogue: bool },
    Year { from: i64, to: i64, catalogue: bool },
    Empty,
}

impl AlbumOrder {
    fn compare(self, a: &Album, b: &Album) -> Ordering {
        match self {
            Self::Id => a.id.cmp(&b.id),
            Self::Snapshot | Self::Empty => Ordering::Equal,
            Self::Created => b.created.cmp(&a.created),
            // Option's None sorts below Some; reversing puts unknown dates last.
            Self::Curated => b.curated_at.cmp(&a.curated_at).then(a.id.cmp(&b.id)),
            Self::Name => a.name.cmp(&b.name),
            Self::Artist { catalogue } => {
                let names = if catalogue {
                    a.artist_sort_name.cmp(&b.artist_sort_name)
                } else {
                    a.artist.cmp(&b.artist)
                };
                names.then(a.name.cmp(&b.name))
            }
            Self::Year {
                catalogue: true, ..
            } => b
                .release_date
                .cmp(&a.release_date)
                .then(b.curated_at.cmp(&a.curated_at))
                .then(a.id.cmp(&b.id)),
            Self::Year { from, to, .. } => {
                let years = if from > to {
                    b.year.cmp(&a.year)
                } else {
                    a.year.cmp(&b.year)
                };
                years.then(a.name.cmp(&b.name))
            }
        }
    }

    fn includes(self, album: &Album) -> bool {
        match self {
            Self::Year {
                from,
                to,
                catalogue: true,
            } => album.year.is_some_and(|year| (from..=to).contains(&year)),
            Self::Year {
                from,
                to,
                catalogue: false,
            } => album
                .year
                .is_some_and(|year| (from.min(to)..=from.max(to)).contains(&year)),
            Self::Empty => false,
            _ => true,
        }
    }
}

impl MemberBrowse<'_> {
    pub(super) fn album_list_document(
        &self,
        budget: &mut ConstructionBudget,
        endpoint: &str,
        params: &Params,
    ) -> Result<Value, BudgetExceeded> {
        let catalogue = endpoint == "getAlbumList2";
        let missing = |budget: &mut ConstructionBudget| {
            charged_error_document(
                budget,
                code::REQUIRED_PARAM_MISSING,
                "Required parameter is missing.",
            )
        };
        let list_type = params.get("type");
        let order = match list_type {
            Some("random") => AlbumOrder::Id,
            Some("newest") if catalogue => AlbumOrder::Curated,
            Some("newest") => AlbumOrder::Created,
            Some("alphabeticalByName") => AlbumOrder::Name,
            Some("alphabeticalByArtist") => AlbumOrder::Artist { catalogue },
            Some("byYear") => {
                let year = |key| params.get(key).and_then(|s| s.parse::<i16>().ok());
                let (Some(from), Some(to)) = (year("fromYear"), year("toYear")) else {
                    return missing(budget);
                };
                AlbumOrder::Year {
                    from: from.into(),
                    to: to.into(),
                    catalogue,
                }
            }
            Some("byGenre") if params.get("genre").is_none_or(str::is_empty) => {
                return missing(budget);
            }
            Some("frequent") => AlbumOrder::Snapshot,
            Some("recent" | "starred" | "highest" | "byGenre") => AlbumOrder::Empty,
            _ => return missing(budget),
        };
        let Some(size @ 1..) = page_param(params, "size", 10, MAX_PAGE_SIZE) else {
            return missing(budget);
        };
        let Some(offset) = page_param(params, "offset", 0, MAX_PAGE_OFFSET) else {
            return missing(budget);
        };
        if catalogue {
            let message = match list_type {
                Some("frequent") => Some("Unsupported album list type: frequent"),
                Some("recent") => Some("Unsupported album list type: recent"),
                Some("starred") => Some("Unsupported album list type: starred"),
                Some("highest") => Some("Unsupported album list type: highest"),
                Some("byGenre") => Some("Unsupported album list type: byGenre"),
                _ => None,
            };
            if let Some(message) = message {
                return charged_error_document(budget, code::GENERIC, message);
            }
        }
        let offset = if !catalogue && matches!(order, AlbumOrder::Id) {
            0
        } else {
            offset
        };
        let candidates = self
            .albums
            .iter()
            .enumerate()
            .filter(|(_, album)| order.includes(album));
        let count = candidates.clone().count();
        let limit = if offset < count { count } else { 0 };
        let mut ordered = budget.allocate_exact(limit)?;
        for (position, album) in candidates.take(limit) {
            let count = self.songs_for(album).count() as i64;
            if !catalogue || count != 0 {
                ordered.push((position, album, count));
            }
        }
        // Preserve snapshot order for otherwise equal keys without sort scratch.
        ordered.sort_unstable_by(|(i, a, _), (j, b, _)| order.compare(a, b).then(i.cmp(j)));
        let page = charged_list(
            budget,
            ordered.iter().skip(offset).take(size),
            |budget, &(_, album, count)| {
                let fields = album_fields(budget, album, count)?;
                Value::charged_map(budget, fields)
            },
        )?;
        let payload = Value::charged_map(budget, [("album", Value::List(page))])?;
        let key = if catalogue { "albumList2" } else { "albumList" };
        charged_ok_document(budget, [(key, payload)])
    }
}

#[cfg(any(test, feature = "ledger-wasm-test"))]
#[cfg_attr(test, test)]
#[doc(hidden)]
pub fn album_list_boundaries() {
    use crate::envelope::error_document;
    use crate::library::{Library, MemStore};
    use std::mem::{align_of, size_of};

    fn charge<T>(budget: &mut ConstructionBudget, len: usize) {
        if len != 0 {
            budget.charge(len, size_of::<T>(), align_of::<T>()).unwrap();
        }
    }

    fn charge_graph(budget: &mut ConstructionBudget, value: &Value) {
        match value {
            Value::Map(entries) => {
                charge::<(String, Value)>(budget, entries.len());
                for (key, child) in entries {
                    charge::<u8>(budget, key.len());
                    charge_graph(budget, child);
                }
            }
            Value::List(entries) => {
                charge::<Value>(budget, entries.len());
                for child in entries {
                    charge_graph(budget, child);
                }
            }
            Value::Str(text) => charge::<u8>(budget, text.len()),
            _ => {}
        }
    }

    let store = MemStore::seeded();
    let mut albums = Vec::new();
    let mut songs = Vec::new();
    let album = &store.all_albums()[0];
    let song = &store.all_songs()[0];
    for (id, date, curated, created) in [
        ("c", Some("2023-08-07"), None, "2026-01-05"),
        ("d", None, Some("2026-01-05"), "2026-01-03"),
        ("b", Some("2023-08-07"), Some("2026-01-01"), "2026-01-04"),
        ("a", Some("2023-08-07"), Some("2026-01-01"), "2026-01-02"),
        (
            "empty",
            Some("2023-12-20"),
            Some("2026-02-03"),
            "2026-01-01",
        ),
    ] {
        albums.push(Album {
            id: id.into(),
            year: Some(2023),
            release_date: date.map(str::to_owned),
            curated_at: curated.map(str::to_owned),
            created: created.into(),
            ..album.clone()
        });
        if id != "empty" {
            let mut item = song.clone();
            item.album_id = id.into();
            songs.push(item);
        }
    }
    let view = MemberBrowse {
        artists: &[],
        albums: &albums,
        songs: &songs,
        now_millis: 0,
    };
    let all = albums.len();
    for (name, rest, ids, candidates) in [
        (
            "getAlbumList2",
            "type=newest",
            &["d", "a", "b", "c"][..],
            all,
        ),
        (
            "getAlbumList2",
            "type=byYear&fromYear=2020&toYear=2024",
            &["a", "b", "c", "d"],
            all,
        ),
        (
            "getAlbumList2",
            "type=byYear&fromYear=2020&toYear=2024&offset=1&size=2",
            &["b", "c"],
            all,
        ),
        (
            "getAlbumList2",
            "type=byYear&fromYear=2024&toYear=2020",
            &[],
            0,
        ),
        (
            "getAlbumList2",
            "type=random&offset=1&size=2",
            &["b", "c"],
            all,
        ),
        (
            "getAlbumList",
            "type=random&offset=1&size=2",
            &["a", "b"],
            all,
        ),
        (
            "getAlbumList",
            "type=newest",
            &["c", "b", "d", "a", "empty"],
            all,
        ),
    ] {
        let params = Params::parse(rest);
        let build = |budget: &mut ConstructionBudget| {
            view.document(budget, name, &params, |_| panic!("no song lookup"))
                .map(|doc| doc.expect("album list supported"))
        };
        let mut probe = ConstructionBudget::new(usize::MAX);
        let actual = build(&mut probe).unwrap();
        let key = if name == "getAlbumList" {
            "albumList"
        } else {
            "albumList2"
        };
        let Value::List(page) = super::field(super::field(&actual, key), "album") else {
            panic!("expected page")
        };
        assert_eq!(page.len(), ids.len());
        assert_eq!(page.capacity(), ids.len());
        for (item, id) in page.iter().zip(ids) {
            assert_eq!(super::field(item, "id"), &Value::Str((*id).into()));
        }
        let mut expected = ConstructionBudget::new(usize::MAX);
        // The buffer is reserved before member eligibility and pagination.
        charge::<(usize, &Album, i64)>(&mut expected, candidates);
        charge_graph(&mut expected, &actual);
        let limit = expected.charged();
        assert_eq!(probe.charged(), limit);
        assert_eq!(build(&mut ConstructionBudget::new(limit)), Ok(actual));
        assert_eq!(
            build(&mut ConstructionBudget::new(limit - 1)),
            Err(BudgetExceeded)
        );
    }

    let refusal = |query| {
        view.document(
            &mut ConstructionBudget::new(usize::MAX),
            "getAlbumList2",
            &Params::parse(query),
            |_| panic!("no song lookup"),
        )
        .unwrap()
    };
    for (query, kind) in [
        ("type=frequent", "frequent"),
        ("type=recent", "recent"),
        ("type=starred", "starred"),
        ("type=highest", "highest"),
        ("type=byGenre&genre=Rock", "byGenre"),
    ] {
        assert_eq!(
            refusal(query),
            Some(error_document(
                0,
                &format!("Unsupported album list type: {kind}")
            ))
        );
    }
    for query in [
        "type=byGenre",
        "type=frequent&size=0",
        "type=recent&offset=x",
    ] {
        assert_eq!(
            refusal(query),
            Some(error_document(10, "Required parameter is missing."))
        );
    }
}
