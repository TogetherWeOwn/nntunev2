//! Inert ordering primitive for a future bounded snapshot builder.
//!
//! The budget covers comparator calls and artist-row probes only. String
//! comparison length, mapping, allocation and all other work need separate
//! bounds; acceptance here is not memory admission.

use std::cmp::Ordering;

use nntune_subsonic::library::{Album, Artist, Song};

use crate::catalogue::ArtistDbRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderWork {
    pub comparisons: usize,
    pub artist_row_probes: usize,
}

/// Worst-case work for stable insertion sorting. Overflow is a refusal.
pub fn order_work(artists: usize, albums: usize, songs: usize, rows: usize) -> Option<OrderWork> {
    fn pairs(n: usize) -> Option<usize> {
        let previous = n.saturating_sub(1);
        if n % 2 == 0 {
            (n / 2).checked_mul(previous)
        } else {
            n.checked_mul(previous / 2)
        }
    }
    let artist_comparisons = pairs(artists)?;
    Some(OrderWork {
        comparisons: artist_comparisons
            .checked_add(pairs(albums)?)?
            .checked_add(pairs(songs)?)?,
        artist_row_probes: artist_comparisons.checked_mul(2)?.checked_mul(rows)?,
    })
}

/// Rejects before touching any output if either work limit is insufficient.
pub fn order_snapshot(
    artists: &mut [Artist],
    albums: &mut [Album],
    songs: &mut [Song],
    artist_rows: &[ArtistDbRow],
    budget: OrderWork,
) -> Result<(), ()> {
    let work = order_work(artists.len(), albums.len(), songs.len(), artist_rows.len()).ok_or(())?;
    if work.comparisons > budget.comparisons || work.artist_row_probes > budget.artist_row_probes {
        return Err(());
    }
    stable_insertion(artists, |a, b| {
        artist_sort(a, artist_rows)
            .cmp(artist_sort(b, artist_rows))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    stable_insertion(albums, |a, b| {
        a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id))
    });
    stable_insertion(songs, |a, b| {
        a.album_id
            .cmp(&b.album_id)
            .then_with(|| a.disc.cmp(&b.disc))
            .then_with(|| a.track.cmp(&b.track))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(())
}

fn artist_sort<'a>(artist: &'a Artist, rows: &'a [ArtistDbRow]) -> &'a str {
    rows.iter()
        .rev()
        .find(|row| row.mbid == artist.id)
        .map(|row| row.sort_name.as_str())
        .unwrap_or(artist.name.as_str())
}

fn stable_insertion<T>(items: &mut [T], compare: impl Fn(&T, &T) -> Ordering) {
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 && compare(&items[j], &items[j - 1]) == Ordering::Less {
            items.swap(j, j - 1);
            j -= 1;
        }
    }
}
