//! Single-row visibility pins for the native genre/playlist library slice.
//!
//! Test-only follow-up: the four single-row read/lookup statements carry the
//! correct visibility SQL, but weakening `OR is_public = TRUE` to `OR TRUE`
//! passed the whole suite. This file pins the exact full text of all four
//! single-row statements plus both list statements, so projection,
//! wrong-table, guard/parentheses and visibility-weakening mutants fail.
//!
//! It integrates the prior 20-test acceptance file (distinct `acc-` fixtures,
//! never confused with the slice's own fixtures) while preserving the
//! slice's own `library_tests.rs` coverage untouched. Test files only: no
//! implementation or migration is touched here.
//!
//! Coverage maps to the review findings, not just the happy path:
//! - Authentication runs before any existence check (401, never 404/403,
//!   for anonymous callers — no oracle).
//! - Visibility: callers see own rows plus public rows; invisible ids are
//!   404 (reads AND mutations), never 403.
//! - Owner scope: visible rows owned by someone else are 403 on mutation,
//!   and the stored row is untouched (pure intents, never mutated rows).
//! - Mutation lookups answer 403-vs-404 with an `is_owner` boolean; the
//!   owner id itself never leaves SQL.
//! - SELECT lists never return the owner/internal columns; responses carry
//!   only the opaque public id as `id` plus display fields.
//! - Lists are bounded keyset pages (`MAX_PAGE_SIZE`, `LIMIT $2`); the
//!   cursor compares directly against the public-id ordering key so a
//!   deleted or newly-hidden boundary row cannot truncate later pages; a
//!   cursor above every row replays from the top (still visibility-filtered).
//! - The first-page NULL guard and the visibility predicate stay grouped as
//!   one `AND (...)` unit: unparenthesised, `AND` binds tighter than `OR`
//!   and a non-NULL cursor would bypass the visibility filter.
//! - Single-row reads/lookups pin projection, table and
//!   `WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)`
//!   exactly; both list statements are pinned in full so a FROM swap,
//!   projection change or guard/parentheses regression fails here.
//!
//! No network or database is contacted. The HTTP + Hyperdrive Worker entry
//! (follow-up slice) must map [`LibraryError::http_status`] to the response
//! status. Test files only: never touch implementation files on this card.

use nntune_api::library::{self, LibraryError, MutationLookup};

// --- Fixtures ---------------------------------------------------------------
// Distinct `acc-` ids so these rows are never confused with the PR's own
// fixtures when both files exist on the landing branch.

fn genre_rows() -> Vec<library::GenreRow> {
    vec![
        library::GenreRow {
            internal_id: 101,
            public_id: "acc-g-alice-priv-01".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Quiet".into(),
            is_public: false,
        },
        library::GenreRow {
            internal_id: 102,
            public_id: "acc-g-alice-pub-02".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Loud".into(),
            is_public: true,
        },
        library::GenreRow {
            internal_id: 103,
            public_id: "acc-g-bob-priv-03".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Quiet".into(),
            is_public: false,
        },
        library::GenreRow {
            internal_id: 104,
            public_id: "acc-g-bob-pub-04".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Loud".into(),
            is_public: true,
        },
    ]
}

fn playlist_rows() -> Vec<library::PlaylistRow> {
    vec![
        library::PlaylistRow {
            internal_id: 201,
            public_id: "acc-p-alice-priv-11".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Drive".into(),
            items: vec!["acc-track-a".into()],
            is_public: false,
        },
        library::PlaylistRow {
            internal_id: 202,
            public_id: "acc-p-alice-pub-12".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Dance".into(),
            items: vec![],
            is_public: true,
        },
        library::PlaylistRow {
            internal_id: 203,
            public_id: "acc-p-bob-priv-13".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Drive".into(),
            items: vec!["acc-track-b".into()],
            is_public: false,
        },
        library::PlaylistRow {
            internal_id: 204,
            public_id: "acc-p-bob-pub-14".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Dance".into(),
            items: vec![],
            is_public: true,
        },
    ]
}

// --- 401 runs before any existence check ------------------------------------

#[test]
fn unauthenticated_lists_are_401() {
    assert_eq!(
        library::list_genres(None, &genre_rows()),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::list_playlists(None, &playlist_rows()),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::list_genres_page(None, &genre_rows(), 10, None),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::list_playlists_page(None, &playlist_rows(), 10, None),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(LibraryError::Unauthorized.http_status(), 401);
}

#[test]
fn unauthenticated_reads_are_401_for_existing_and_missing_ids() {
    // Existing public id: still 401, never the row and never 404.
    assert_eq!(
        library::get_genre(None, &genre_rows(), "acc-g-bob-pub-04"),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::get_playlist(None, &playlist_rows(), "acc-p-bob-pub-14"),
        Err(LibraryError::Unauthorized)
    );
    // Missing id: also 401 — auth first means no existence oracle.
    assert_eq!(
        library::get_genre(None, &genre_rows(), "acc-g-missing"),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::get_playlist(None, &playlist_rows(), "acc-p-missing"),
        Err(LibraryError::Unauthorized)
    );
}

#[test]
fn unauthenticated_mutations_are_401() {
    assert_eq!(
        library::prepare_genre_insert(None, "Late Night", false, 0),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::prepare_playlist_insert(None, "Road Trip", vec![], false, 0),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_genre_update(None, &genre_rows(), "acc-g-alice-pub-02", "Renamed", true),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_genre_delete(None, &genre_rows(), "acc-g-alice-pub-02"),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_playlist_update(
            None,
            &playlist_rows(),
            "acc-p-alice-pub-12",
            "Renamed",
            vec![],
            false
        ),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_playlist_delete(None, &playlist_rows(), "acc-p-alice-pub-12"),
        Err(LibraryError::Unauthorized)
    );
    // Even with a valid ownership lookup in hand, no caller means 401.
    let own = Some(MutationLookup {
        public_id: "acc-g-alice-pub-02".into(),
        is_owner: true,
    });
    assert_eq!(
        library::authorize_genre_update_from_lookup(None, own.clone(), "Renamed", true),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_genre_delete_from_lookup(None, own),
        Err(LibraryError::Unauthorized)
    );
    // Auth runs before validation too: a blank name with no caller is 401,
    // not 422.
    assert_eq!(
        library::prepare_genre_insert(None, "   ", false, 0),
        Err(LibraryError::Unauthorized)
    );
}

// --- Visibility: own + public only; invisible ids are 404 -------------------

#[test]
fn lists_show_own_plus_public_only() {
    let genres = library::list_genres(Some("user-alice"), &genre_rows()).unwrap();
    let names: Vec<&str> = genres.iter().map(|g| g.name.as_str()).collect();
    assert!(names.contains(&"Alice Quiet"));
    assert!(names.contains(&"Alice Loud"));
    assert!(names.contains(&"Bob Loud"));
    assert!(!names.contains(&"Bob Quiet"));

    let playlists = library::list_playlists(Some("user-alice"), &playlist_rows()).unwrap();
    let names: Vec<&str> = playlists.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Alice Drive"));
    assert!(names.contains(&"Alice Dance"));
    assert!(names.contains(&"Bob Dance"));
    assert!(!names.contains(&"Bob Drive"));

    // Symmetric for the other caller.
    let genres = library::list_genres(Some("user-bob"), &genre_rows()).unwrap();
    let names: Vec<&str> = genres.iter().map(|g| g.name.as_str()).collect();
    assert!(names.contains(&"Bob Quiet"));
    assert!(names.contains(&"Bob Loud"));
    assert!(names.contains(&"Alice Loud"));
    assert!(!names.contains(&"Alice Quiet"));
}

#[test]
fn reads_of_visible_rows_succeed() {
    let genre = library::get_genre(Some("user-alice"), &genre_rows(), "acc-g-alice-priv-01")
        .expect("own private row is visible to its owner");
    assert_eq!(genre.id, "acc-g-alice-priv-01");
    let genre = library::get_genre(Some("user-alice"), &genre_rows(), "acc-g-bob-pub-04")
        .expect("foreign public row is visible");
    assert_eq!(genre.name, "Bob Loud");
    let playlist = library::get_playlist(Some("user-alice"), &playlist_rows(), "acc-p-bob-pub-14")
        .expect("foreign public playlist is visible");
    assert_eq!(playlist.items, Vec::<String>::new());
}

#[test]
fn invisible_and_unknown_ids_are_404_for_reads() {
    // Another user's private row: 404, never 403 — no existence oracle.
    assert_eq!(
        library::get_genre(Some("user-alice"), &genre_rows(), "acc-g-bob-priv-03"),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::get_playlist(Some("user-alice"), &playlist_rows(), "acc-p-bob-priv-13"),
        Err(LibraryError::NotFound)
    );
    // Unknown ids: 404 as well.
    assert_eq!(
        library::get_genre(Some("user-alice"), &genre_rows(), "acc-g-missing"),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::get_playlist(Some("user-alice"), &playlist_rows(), "acc-p-missing"),
        Err(LibraryError::NotFound)
    );
    assert_eq!(LibraryError::NotFound.http_status(), 404);
}

#[test]
fn invisible_and_unknown_ids_are_404_for_mutations() {
    // Another user's private row authorizes as NotFound, not Forbidden.
    assert_eq!(
        library::authorize_genre_update(
            Some("user-alice"),
            &genre_rows(),
            "acc-g-bob-priv-03",
            "Hijacked",
            true
        ),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_genre_delete(Some("user-alice"), &genre_rows(), "acc-g-bob-priv-03"),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_playlist_update(
            Some("user-alice"),
            &playlist_rows(),
            "acc-p-bob-priv-13",
            "Hijacked",
            vec![],
            false
        ),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_playlist_delete(
            Some("user-alice"),
            &playlist_rows(),
            "acc-p-bob-priv-13"
        ),
        Err(LibraryError::NotFound)
    );
    // Unknown ids: 404 on every mutation path.
    assert_eq!(
        library::authorize_genre_update(
            Some("user-alice"),
            &genre_rows(),
            "acc-g-missing",
            "Renamed",
            false
        ),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_playlist_delete(Some("user-alice"), &playlist_rows(), "acc-p-missing"),
        Err(LibraryError::NotFound)
    );
    // No lookup row (invisible id): 404 through the lookup authorizers too.
    assert_eq!(
        library::authorize_genre_update_from_lookup(Some("user-alice"), None, "Renamed", true),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_playlist_delete_from_lookup(Some("user-alice"), None),
        Err(LibraryError::NotFound)
    );
}

// --- Owner scope: foreign-public mutations are 403 --------------------------

#[test]
fn foreign_public_updates_are_403_and_leave_rows_untouched() {
    let genres = genre_rows();
    assert_eq!(
        library::authorize_genre_update(
            Some("user-alice"),
            &genres,
            "acc-g-bob-pub-04",
            "Hijacked",
            true
        ),
        Err(LibraryError::Forbidden)
    );
    assert_eq!(LibraryError::Forbidden.http_status(), 403);
    assert_eq!(
        genres
            .iter()
            .find(|r| r.public_id == "acc-g-bob-pub-04")
            .unwrap()
            .name,
        "Bob Loud",
        "the pure authorizer returns an intent, never a mutated row"
    );

    let playlists = playlist_rows();
    assert_eq!(
        library::authorize_playlist_update(
            Some("user-alice"),
            &playlists,
            "acc-p-bob-pub-14",
            "Hijacked",
            vec!["acc-track-x".into()],
            false
        ),
        Err(LibraryError::Forbidden)
    );
    assert!(playlists
        .iter()
        .find(|r| r.public_id == "acc-p-bob-pub-14")
        .unwrap()
        .items
        .is_empty());

    // Same verdict through the lookup authorizers: a visible-but-foreign row
    // reports `is_owner: false`, exactly what the mutation-lookup statements
    // return for it.
    let foreign = Some(MutationLookup {
        public_id: "acc-g-bob-pub-04".into(),
        is_owner: false,
    });
    assert_eq!(
        library::authorize_genre_update_from_lookup(
            Some("user-alice"),
            foreign.clone(),
            "Hijacked",
            true
        ),
        Err(LibraryError::Forbidden)
    );
    assert_eq!(
        library::authorize_genre_delete_from_lookup(Some("user-alice"), foreign),
        Err(LibraryError::Forbidden)
    );
    let foreign_playlist = Some(MutationLookup {
        public_id: "acc-p-bob-pub-14".into(),
        is_owner: false,
    });
    assert_eq!(
        library::authorize_playlist_update_from_lookup(
            Some("user-alice"),
            foreign_playlist.clone(),
            "Hijacked",
            vec![],
            false
        ),
        Err(LibraryError::Forbidden)
    );
    assert_eq!(
        library::authorize_playlist_delete_from_lookup(Some("user-alice"), foreign_playlist),
        Err(LibraryError::Forbidden)
    );
}

#[test]
fn foreign_public_deletes_are_403() {
    assert_eq!(
        library::authorize_genre_delete(Some("user-alice"), &genre_rows(), "acc-g-bob-pub-04"),
        Err(LibraryError::Forbidden)
    );
    assert_eq!(
        library::authorize_playlist_delete(
            Some("user-alice"),
            &playlist_rows(),
            "acc-p-bob-pub-14"
        ),
        Err(LibraryError::Forbidden)
    );
}

#[test]
fn owner_intents_carry_the_caller_as_owner() {
    let update = library::authorize_genre_update(
        Some("user-alice"),
        &genre_rows(),
        "acc-g-alice-priv-01",
        "Alice Renamed",
        true,
    )
    .unwrap();
    assert_eq!(update.owner_user_id, "user-alice");
    assert_eq!(update.public_id, "acc-g-alice-priv-01");
    assert_eq!(update.name, "Alice Renamed");

    let delete =
        library::authorize_genre_delete(Some("user-alice"), &genre_rows(), "acc-g-alice-priv-01")
            .unwrap();
    assert_eq!(delete.owner_user_id, "user-alice");
    assert_eq!(delete.public_id, "acc-g-alice-priv-01");

    let insert =
        library::prepare_playlist_insert(Some("user-alice"), "Road Trip", vec![], false, 0)
            .unwrap();
    assert_eq!(insert.owner_user_id, "user-alice");
    assert!(!insert.public_id.is_empty());

    // Lookup-driven intents bind the caller as well.
    let own = Some(MutationLookup {
        public_id: "acc-p-alice-pub-12".into(),
        is_owner: true,
    });
    let update = library::authorize_playlist_update_from_lookup(
        Some("user-alice"),
        own,
        "Party",
        vec!["acc-track-a".into()],
        true,
    )
    .unwrap();
    assert_eq!(update.owner_user_id, "user-alice");
    assert_eq!(update.public_id, "acc-p-alice-pub-12");
}

// --- Opaque ids: no owner / internal leaks -----------------------------------

#[test]
fn public_ids_are_opaque_and_unique() {
    let first = library::new_public_id().expect("issue");
    let second = library::new_public_id().expect("issue");
    assert!(!first.is_empty());
    assert_ne!(first, second, "ids must be random, never sequential");
    assert!(first.len() >= 22, "128-bit ids stay unguessable");
}

#[test]
fn responses_carry_only_opaque_ids_and_display_fields() {
    for genre in &library::list_genres(Some("user-alice"), &genre_rows()).unwrap() {
        let json = serde_json::to_string(genre).unwrap();
        assert!(json.contains("\"id\""), "opaque id present: {json}");
        assert!(!json.contains("owner"), "no owner leak: {json}");
        assert!(!json.contains("user_id"), "no owner leak: {json}");
        assert!(!json.contains("internal"), "no internal leak: {json}");
    }
    let one = library::get_genre(Some("user-alice"), &genre_rows(), "acc-g-alice-priv-01").unwrap();
    assert_eq!(one.id, "acc-g-alice-priv-01");
    assert_ne!(one.id, "101", "response id is the opaque handle");

    for playlist in &library::list_playlists(Some("user-bob"), &playlist_rows()).unwrap() {
        let json = serde_json::to_string(playlist).unwrap();
        assert!(json.contains("\"id\""), "opaque id present: {json}");
        assert!(!json.contains("owner"), "no owner leak: {json}");
        assert!(!json.contains("user_id"), "no owner leak: {json}");
        assert!(!json.contains("internal"), "no internal leak: {json}");
        assert!(!json.contains("items"), "list shapes omit items: {json}");
    }
    // Single reads return items; lists never do.
    let full =
        library::get_playlist(Some("user-alice"), &playlist_rows(), "acc-p-alice-priv-11").unwrap();
    assert_eq!(full.items, vec!["acc-track-a".to_string()]);
}

// --- Validation --------------------------------------------------------------

#[test]
fn blank_and_overlong_names_are_422() {
    assert_eq!(
        library::prepare_genre_insert(Some("user-alice"), "   ", false, 0),
        Err(LibraryError::Invalid)
    );
    assert_eq!(
        library::prepare_genre_insert(Some("user-alice"), &"x".repeat(121), false, 0),
        Err(LibraryError::Invalid)
    );
    assert_eq!(LibraryError::Invalid.http_status(), 422);
    assert!(
        library::prepare_genre_insert(Some("user-alice"), &"x".repeat(120), false, 0).is_ok(),
        "exactly MAX_NAME_LEN passes"
    );
}

#[test]
fn playlist_items_are_bounded() {
    assert_eq!(
        library::prepare_playlist_insert(
            Some("user-alice"),
            "Mix",
            vec![" ".to_string()],
            false,
            0
        ),
        Err(LibraryError::Invalid)
    );
    assert_eq!(
        library::prepare_playlist_insert(
            Some("user-alice"),
            "Mix",
            vec!["x".repeat(129)],
            false,
            0
        ),
        Err(LibraryError::Invalid)
    );
    assert_eq!(
        library::prepare_playlist_insert(
            Some("user-alice"),
            "Mix",
            vec!["t".to_string(); 501],
            false,
            0
        ),
        Err(LibraryError::Invalid)
    );
    assert!(
        library::prepare_playlist_insert(
            Some("user-alice"),
            "Mix",
            vec!["x".repeat(128)],
            false,
            0
        )
        .is_ok(),
        "exactly MAX_ITEM_LEN passes"
    );
}

// --- Bounded pages + stable cursors ------------------------------------------

fn many_playlist_rows(n: usize) -> Vec<library::PlaylistRow> {
    // Ids run opposite to internal ids on purpose: list order must follow the
    // public handle, never insertion order or recency.
    (0..n)
        .map(|i| library::PlaylistRow {
            internal_id: i as i64 + 1,
            public_id: format!("acc-pub-{n:04}-{:04}", n - 1 - i),
            owner_user_id: "user-bob".into(),
            name: format!("Shared {i:04}"),
            items: vec!["acc-track".into()],
            is_public: true,
        })
        .collect()
}

#[test]
fn pages_are_bounded_clamped_and_chainable() {
    let rows = many_playlist_rows(library::MAX_PAGE_SIZE + 10);
    assert_eq!(library::clamp_page_size(0), 1);
    assert_eq!(library::clamp_page_size(usize::MAX), library::MAX_PAGE_SIZE);

    // An absurd limit still returns at most one page.
    let first = library::list_playlists_page(Some("user-alice"), &rows, 5000, None).unwrap();
    assert_eq!(first.len(), library::MAX_PAGE_SIZE);

    // Chain with the cursor built from the last returned public id — no
    // internal id lookup is needed by the Worker.
    let last_first_page = first[library::MAX_PAGE_SIZE - 1].id.clone();
    let second = library::list_playlists_page(
        Some("user-alice"),
        &rows,
        library::MAX_PAGE_SIZE,
        Some(&library::page_cursor(&last_first_page)),
    )
    .unwrap();
    assert_eq!(second.len(), 10);
    assert!(second.iter().all(|p| p.id != last_first_page));

    // Garbage cursors are 422, never a silent restart.
    assert_eq!(
        library::list_playlists_page(Some("user-alice"), &rows, 10, Some("!!!")),
        Err(LibraryError::Invalid)
    );
}

#[test]
fn paging_survives_deleted_or_hidden_boundary_rows() {
    let rows = many_playlist_rows(60);
    let first = library::list_playlists_page(Some("user-alice"), &rows, 10, None).unwrap();
    let cursor_id = first.last().unwrap().id.clone();
    let after = library::page_cursor(&cursor_id);
    let expected_next =
        library::list_playlists_page(Some("user-alice"), &rows, 10, Some(&after)).unwrap();

    // The boundary row disappears before the next request: the page must not
    // shrink or truncate — the cursor compares directly with the ordering key.
    let mut deleted = rows.clone();
    deleted.retain(|row| row.public_id != cursor_id);
    assert_eq!(
        library::list_playlists_page(Some("user-alice"), &deleted, 10, Some(&after)).unwrap(),
        expected_next
    );

    // The boundary row turns private instead: same continuation.
    let mut hidden = rows.clone();
    let boundary = hidden
        .iter_mut()
        .find(|row| row.public_id == cursor_id)
        .unwrap();
    boundary.is_public = false;
    boundary.owner_user_id = "user-bob".into();
    assert_eq!(
        library::list_playlists_page(Some("user-alice"), &hidden, 10, Some(&after)).unwrap(),
        expected_next
    );

    // Even the whole previous page vanishing must not end the list early.
    let first_ids: Vec<String> = first.iter().map(|row| row.id.clone()).collect();
    let mut page_gone = many_playlist_rows(60);
    page_gone.retain(|row| !first_ids.contains(&row.public_id));
    assert_eq!(
        library::list_playlists_page(Some("user-alice"), &page_gone, 10, Some(&after)).unwrap(),
        expected_next
    );
}

#[test]
fn cursor_round_trips_public_ids_and_rejects_garbage() {
    let id = "acc-g-alice-pub-02";
    assert_eq!(
        library::parse_page_cursor(&library::page_cursor(id)),
        Ok(id.to_string()),
        "the cursor wraps the already-public id, never an internal key"
    );
    assert_eq!(library::parse_page_cursor(""), Err(LibraryError::Invalid));
    assert_eq!(
        library::parse_page_cursor("!!!"),
        Err(LibraryError::Invalid)
    );
    assert_eq!(
        library::parse_page_cursor(&"A".repeat(193)),
        Err(LibraryError::Invalid)
    );
}

#[test]
fn above_max_cursor_replays_from_top_without_leaking() {
    // A well-formed cursor above every visible row matches everything in both
    // the pager and the list SQL (`public_id < $3`); the replay is harmless
    // because every returned row is still visibility-filtered.
    let rows = many_playlist_rows(60);
    let first = library::list_playlists_page(Some("user-alice"), &rows, 10, None).unwrap();
    let replay = library::list_playlists_page(
        Some("user-alice"),
        &rows,
        10,
        Some(&library::page_cursor("acc-pub-9999-zzzz")),
    )
    .unwrap();
    assert_eq!(replay, first);
    assert!(replay.iter().all(|p| rows
        .iter()
        .any(|r| r.public_id == p.id && (r.is_public || r.owner_user_id == "user-alice"))));
}

// --- SQL boundary: parameterized, owner-scoped, grouped ----------------------

#[test]
fn statements_are_parameterized_and_owner_scoped() {
    for sql in [
        library::genre_list_statement(),
        library::genre_read_statement(),
        library::genre_insert_statement(),
        library::genre_update_statement(),
        library::genre_delete_statement(),
        library::playlist_list_statement(),
        library::playlist_read_statement(),
        library::playlist_insert_statement(),
        library::playlist_update_statement(),
        library::playlist_delete_statement(),
    ] {
        assert!(
            sql.starts_with("SELECT ")
                || sql.starts_with("INSERT ")
                || sql.starts_with("UPDATE ")
                || sql.starts_with("DELETE "),
            "unexpected statement: {sql}"
        );
        assert!(sql.contains("$1"), "parameterized only: {sql}");
    }
    for sql in [
        library::genre_list_statement(),
        library::playlist_list_statement(),
    ] {
        assert!(sql.contains("owner_user_id"), "{sql}");
        assert!(sql.contains("is_public"), "{sql}");
    }
    // Mutations bind the owner: no cross-owner write is expressible.
    for sql in [
        library::genre_update_statement(),
        library::genre_delete_statement(),
        library::playlist_update_statement(),
        library::playlist_delete_statement(),
    ] {
        assert!(sql.contains("owner_user_id = $2"), "{sql}");
    }
    // SELECT lists never return the owner column: filtering happens
    // server-side, responses cannot leak it. (Playlist single-reads return
    // `items`; lists never do.)
    for sql in [
        library::genre_list_statement(),
        library::genre_read_statement(),
        library::playlist_list_statement(),
    ] {
        let select_list = sql.split(" FROM ").next().unwrap();
        assert!(!select_list.contains("owner"), "{sql}");
        assert!(!select_list.contains("internal"), "{sql}");
    }
    let read_select = library::playlist_read_statement()
        .split(" FROM ")
        .next()
        .unwrap();
    assert!(!read_select.contains("owner"), "{read_select}");
    assert!(!read_select.contains("internal"), "{read_select}");
    // Mutation lookups answer ownership with a boolean; the id never leaves.
    for sql in [
        library::genre_mutation_lookup_statement(),
        library::playlist_mutation_lookup_statement(),
    ] {
        assert!(sql.contains("(owner_user_id = $2) AS is_owner"), "{sql}");
        assert!(sql.contains("public_id = $1"), "{sql}");
    }
}

#[test]
fn list_statements_are_bounded_grouped_and_cursor_keyed() {
    for sql in [
        library::genre_list_statement(),
        library::playlist_list_statement(),
    ] {
        // Bounded: one page can never grow with the total public library.
        assert!(
            sql.contains("ORDER BY public_id COLLATE \"C\" DESC"),
            "{sql}"
        );
        assert!(sql.contains("LIMIT $2"), "{sql}");
        // The first-page NULL guard and the cursor predicate stay grouped as
        // one `AND (...)` unit with the visibility filter. Without the parens,
        // `AND` binds tighter than `OR` and a non-NULL cursor bypasses the
        // visibility predicate, leaking foreign private rows; deleting the
        // guard breaks every first page on the `NULL` comparison.
        assert!(
            sql.contains(
                "AND ($3::TEXT IS NULL OR public_id COLLATE \"C\" < $3::TEXT COLLATE \"C\") ORDER BY"
            ),
            "{sql}"
        );
        // Exactly one visibility predicate, applying to every returned row;
        // no cursor-row lookup exists to bypass or weaken it.
        assert_eq!(
            sql.matches("(owner_user_id = $1 OR is_public = TRUE)")
                .count(),
            1,
            "{sql}"
        );
        assert!(!sql.contains("SELECT id FROM"), "{sql}");
        // The projection returns the public id the Worker builds the next
        // cursor from — never items, owner, or internal ids.
        let select = sql.split(" FROM ").next().unwrap();
        assert!(select.contains("public_id"), "{select}");
        assert!(!select.contains("items"), "{select}");
        assert!(!select.contains("owner"), "{select}");
        assert!(!select.contains("internal"), "{select}");
    }
}

// --- Exact single-row pins: projection, table, visibility -------------------
// These four statements are what the Worker runs for every GET and every
// mutation authorization. Weakening `OR is_public = TRUE` to `OR TRUE`,
// `OR is_public = FALSE`, dropping the owner side, unparenthesizing the
// predicate, or swapping FROM genres/playlists must fail here. Exact text
// pins all of those together; the targeted asserts below name the failing
// dimension.

#[test]
fn single_row_read_and_lookup_statements_match_exact_text() {
    assert_eq!(
        library::genre_read_statement(),
        "SELECT public_id, name, is_public FROM genres WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)",
        "genre single-row read"
    );
    assert_eq!(
        library::genre_mutation_lookup_statement(),
        "SELECT public_id, (owner_user_id = $2) AS is_owner FROM genres WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)",
        "genre mutation lookup"
    );
    assert_eq!(
        library::playlist_read_statement(),
        "SELECT public_id, name, items, is_public FROM playlists WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)",
        "playlist single-row read"
    );
    assert_eq!(
        library::playlist_mutation_lookup_statement(),
        "SELECT public_id, (owner_user_id = $2) AS is_owner FROM playlists WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)",
        "playlist mutation lookup"
    );
}

#[test]
fn single_row_statements_pin_projection_table_and_visibility() {
    for (sql, table, other_table, projection) in [
        (
            library::genre_read_statement(),
            "FROM genres",
            "FROM playlists",
            "SELECT public_id, name, is_public",
        ),
        (
            library::genre_mutation_lookup_statement(),
            "FROM genres",
            "FROM playlists",
            "SELECT public_id, (owner_user_id = $2) AS is_owner",
        ),
        (
            library::playlist_read_statement(),
            "FROM playlists",
            "FROM genres",
            "SELECT public_id, name, items, is_public",
        ),
        (
            library::playlist_mutation_lookup_statement(),
            "FROM playlists",
            "FROM genres",
            "SELECT public_id, (owner_user_id = $2) AS is_owner",
        ),
    ] {
        assert!(sql.starts_with(projection), "{sql}");
        assert!(sql.contains(table), "{sql}");
        assert!(!sql.contains(other_table), "{sql}");
        assert!(
            sql.contains("WHERE public_id = $1 AND (owner_user_id = $2 OR is_public = TRUE)"),
            "{sql}"
        );
        assert_eq!(
            sql.matches("(owner_user_id = $2 OR is_public = TRUE)")
                .count(),
            1,
            "{sql}"
        );
        assert!(!sql.contains("OR TRUE"), "{sql}");
        assert!(!sql.contains("OR FALSE"), "{sql}");
        assert!(!sql.contains("is_public = FALSE"), "{sql}");
        let select = sql.split(" FROM ").next().unwrap();
        // Mutation lookups compute `(owner_user_id = $2) AS is_owner`: a
        // boolean, never the owner id itself. Reads return no owner column.
        assert!(!select.contains("internal"), "{select}");
        if select.contains("AS is_owner") {
            assert!(
                select.contains("(owner_user_id = $2) AS is_owner"),
                "{select}"
            );
        } else {
            assert!(!select.contains("owner"), "{select}");
        }
    }
}

// --- Exact list pins: wrong-table / projection / guard caught together ------

#[test]
fn list_statements_match_exact_text() {
    assert_eq!(
        library::genre_list_statement(),
        "SELECT public_id, name, is_public FROM genres WHERE (owner_user_id = $1 OR is_public = TRUE) AND ($3::TEXT IS NULL OR public_id COLLATE \"C\" < $3::TEXT COLLATE \"C\") ORDER BY public_id COLLATE \"C\" DESC LIMIT $2",
        "genre list"
    );
    assert_eq!(
        library::playlist_list_statement(),
        "SELECT public_id, name, is_public FROM playlists WHERE (owner_user_id = $1 OR is_public = TRUE) AND ($3::TEXT IS NULL OR public_id COLLATE \"C\" < $3::TEXT COLLATE \"C\") ORDER BY public_id COLLATE \"C\" DESC LIMIT $2",
        "playlist list"
    );
}
