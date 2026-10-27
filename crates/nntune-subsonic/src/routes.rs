//! Route table, ported from v1 `routes/subsonic.php`.
//!
//! 49 supported endpoints (GET or POST, optional `.view` suffix), 9 explicit
//! refusals answered inside the envelope with error 0 (`UnsupportedOperation`
//! in v1), and `hls.m3u8` as a plain 410 (a stream URL, not a document).
//! Anything else under `/rest/` is a plain 404 outside the envelope.
//!
//! `getOpenSubsonicExtensions` is public per the OpenSubsonic spec (v1 serves
//! it outside the auth group): capability advertisement cannot require the
//! credential it helps obtain. It still merges POST bodies and enforces the
//! body cap like every other route — only authentication is skipped.

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
    "tokenInfo",
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

/// Supported endpoints served without authentication (v1 public group).
pub const PUBLIC_ENDPOINTS: &[&str] = &["getOpenSubsonicExtensions"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Route<'a> {
    Endpoint(&'a str),
    PublicEndpoint(&'a str),
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
        if PUBLIC_ENDPOINTS.contains(hit) {
            return Route::PublicEndpoint(hit);
        }
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
        assert_eq!(
            route("/rest/getOpenSubsonicExtensions"),
            Route::PublicEndpoint("getOpenSubsonicExtensions")
        );
        assert_eq!(route("/rest/tokenInfo"), Route::Endpoint("tokenInfo"));
        assert_eq!(SUPPORTED_ENDPOINTS.len(), 49);
        assert_eq!(UNSUPPORTED_ENDPOINTS.len(), 9);
    }
}
