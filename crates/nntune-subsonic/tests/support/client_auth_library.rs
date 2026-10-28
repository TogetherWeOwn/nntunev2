//! Test-only Library tripwire: denied authentication must not reach a handler.
//! Records the actual dispatched compatibility principal unchanged. Live bank
//! expectations bind username to persisted users.id and require non-admin;
//! neither email nor a generated credential name is an ownership substitute.
//! No storage/authentication/limiter implementation belongs in this support.

use nntune_subsonic::library::{
    Album, AlbumListQuery, Artist, PlayQueue, Playlist, Song, StarKind,
};
use nntune_subsonic::{AuthStore, Identity, Library, UserAccount};
use std::cell::RefCell;
use std::rc::Rc;

pub type LibraryCalls = Rc<RefCell<Vec<Identity>>>;

pub struct GuardedLibrary<A> {
    auth: A,
    calls: LibraryCalls,
}

impl<A> GuardedLibrary<A> {
    pub fn new(auth: A) -> (Self, LibraryCalls) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                auth,
                calls: Rc::clone(&calls),
            },
            calls,
        )
    }
}

impl<A: AuthStore> AuthStore for GuardedLibrary<A> {
    fn verify_api_key(&self, key: &str) -> Option<Identity> {
        self.auth.verify_api_key(key)
    }

    fn verify_password(&self, username: &str, password: &str) -> Option<Identity> {
        self.auth.verify_password(username, password)
    }

    fn verify_token(&self, username: &str, token: &str, salt: &str) -> Option<Identity> {
        self.auth.verify_token(username, token, salt)
    }
}

macro_rules! unexpected_read {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> $result {
            let _ = ($($arg,)*);
            panic!("unexpected Library access: {}", stringify!($name));
        })*
    };
}

macro_rules! unexpected_write {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)*) => {
        $(fn $name(&mut self, $($arg: $ty),*) -> $result {
            let _ = ($($arg,)*);
            panic!("unexpected Library mutation: {}", stringify!($name));
        })*
    };
}

impl<A: AuthStore> Library for GuardedLibrary<A> {
    fn playlists(&self, identity: &Identity) -> Vec<Playlist> {
        self.calls.borrow_mut().push(identity.clone());
        Vec::new()
    }

    unexpected_read! {
        user_account(username: &str) -> Option<UserAccount>;
        artists() -> Vec<Artist>;
        artists_last_modified() -> i64;
        artist(id: &str) -> Option<Artist>;
        albums_for_artist(artist_id: &str) -> Vec<Album>;
        album(id: &str) -> Option<Album>;
        all_albums() -> Vec<Album>;
        songs_for_album(album_id: &str) -> Vec<Song>;
        song(id: &str) -> Option<Song>;
        all_songs() -> Vec<Song>;
        album_list(query: AlbumListQuery<'_>) -> Vec<Album>;
        album_list2(query: AlbumListQuery<'_>) -> Result<Vec<Album>, (i64, String)>;
        random_songs(identity: &Identity, size: usize) -> Vec<Song>;
        genres() -> Vec<(String, i64, i64)>;
        audio(song_id: &str) -> Option<Vec<u8>>;
        cover(cover_id: &str) -> Option<Vec<u8>>;
        avatar(username: &str) -> Option<Vec<u8>>;
        lyrics(artist: &str, title: &str) -> Option<String>;
        playlist(identity: &Identity, id: &str) -> Option<Playlist>;
        starred(identity: &Identity, kind: StarKind) -> Vec<String>;
        rating(identity: &Identity, id: &str) -> Option<i64>;
        queue(identity: &Identity) -> Option<PlayQueue>;
    }

    unexpected_write! {
        create_playlist(identity: &Identity, name: String) -> Playlist;
        update_playlist(identity: &Identity, id: &str, name: Option<String>, add: Vec<String>, remove: Vec<usize>) -> Result<(), (i64, String)>;
        delete_playlist(identity: &Identity, id: &str) -> Result<(), (i64, String)>;
        star(identity: &Identity, kind: StarKind, id: &str) -> bool;
        unstar(identity: &Identity, kind: StarKind, id: &str) -> ();
        set_rating(identity: &Identity, id: &str, rating: i64) -> bool;
        scrobble(identity: &Identity, song_id: &str) -> bool;
        save_queue(identity: &Identity, queue: PlayQueue) -> ();
    }
}
