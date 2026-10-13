//! Route table, ported from v1 `routes/subsonic.php`.
//!
//! 48 supported endpoints (GET or POST, optional `.view` suffix), 9 explicit
//! refusals answered inside the envelope with error 0 (`UnsupportedOperation`
//! in v1), and `hls.m3u8` as a plain 410 (a stream URL, not a document).
//! Anything else under `/rest/` is a plain 404 outside the envelope.

/// Supported endpoints, in v1 route order.
pub const SUPPORTED_ENDPOINTS: &[&str] = &[
    "ping",
    "getLicense",
    "getMusicFolders",
    "getArtists",
    "getIndexes",
    "getMusicDirectory",
    "getArtist",
    "getAlbum",
    "getSong",
    "getGenres",
    "search2",
    "search3",
    "getAlbumList",
    "getAlbumList2",
    "stream",
    "getCoverArt",
    "getPlaylists",
    "getPlaylist",
    "createPlaylist",
    "updatePlaylist",
    "deletePlaylist",
    "scrobble",
    "star",
    "unstar",
    "setRating",
    "getStarred",
    "getStarred2",
    "getRandomSongs",
    "getNowPlaying",
    "getUser",
    "getAvatar",
    "getOpenSubsonicExtensions",
    "download",
    "getSongsByGenre",
    "getLyrics",
    "getLyricsBySongId",
    "getArtistInfo",
    "getArtistInfo2",
    "getAlbumInfo",
    "getAlbumInfo2",
    "getPodcasts",
    "getNewestPodcasts",
    "refreshPodcasts",
    "createPodcastChannel",
    "deletePodcastChannel",
    "getPlayQueue",
    "savePlayQueue",
    "getBookmarks",
];

/// Endpoints with no backing store: explicit error 0, never a 404.
pub const UNSUPPORTED_ENDPOINTS: &[&str] = &[
    "jukeboxControl",
    "getVideos",
    "getVideoInfo",
    "getCaptions",
    "savePlayQueueByIndex",
    "getInternetRadioStations",
    "createInternetRadioStation",
    "updateInternetRadioStation",
    "deleteInternetRadioStation",
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Route<'a> {
    Endpoint(&'a str),
    Unsupported(&'a str),
    Hls,
    Unknown,
}

pub fn route(path: &str) -> Route<'_> {
    let rest = path.strip_prefix("/rest/").unwrap_or("\u{0}");
    if rest.contains('\u{0}') {
        return Route::Unknown;
    }
    let op = rest.strip_suffix(".view").unwrap_or(rest);
    if op.is_empty() || op.contains('/') {
        return Route::Unknown;
    }
    if op == "hls.m3u8" {
        return Route::Hls;
    }
    if let Some(hit) = SUPPORTED_ENDPOINTS.iter().find(|e| **e == op) {
        return Route::Endpoint(hit);
    }
    if let Some(hit) = UNSUPPORTED_ENDPOINTS.iter().find(|e| **e == op) {
        return Route::Unsupported(hit);
    }
    Route::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_shapes() {
        assert_eq!(route("/rest/ping"), Route::Endpoint("ping"));
        assert_eq!(route("/rest/ping.view"), Route::Endpoint("ping"));
        assert_eq!(
            route("/rest/jukeboxControl"),
            Route::Unsupported("jukeboxControl")
        );
        assert_eq!(route("/rest/hls.m3u8"), Route::Hls);
        assert_eq!(route("/rest/bogus"), Route::Unknown);
        assert_eq!(route("/other/ping"), Route::Unknown);
        assert_eq!(route("/rest/"), Route::Unknown);
        assert_eq!(SUPPORTED_ENDPOINTS.len(), 48);
        assert_eq!(UNSUPPORTED_ENDPOINTS.len(), 9);
    }
}
