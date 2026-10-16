//! Acceptance tests for the native genre/playlist slice.
//!
//! Written first (TDD RED): these fail until `nntune_api::library`
//! implements the module. Every test asserts real behavior against
//! in-memory rows; no network or database is contacted. The HTTP +
//! Hyperdrive Worker entry reuses these pure rules plus the parameterized
//! statements; it must map [`LibraryError::http_status`] to the response
//! status so unauthenticated `/api/genres` + `/api/playlists` return 401.

use nntune_api::library::{self, LibraryError};

// --- Fixtures ---------------------------------------------------------------

fn genre_rows() -> Vec<library::GenreRow> {
    vec![
        library::GenreRow {
            internal_id: 1,
            public_id: "g-alice-private-opaque-01".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Private".into(),
            is_public: false,
        },
        library::GenreRow {
            internal_id: 2,
            public_id: "g-alice-public-opaque-02".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Public".into(),
            is_public: true,
        },
        library::GenreRow {
            internal_id: 3,
            public_id: "g-bob-private-opaque-03".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Private".into(),
            is_public: false,
        },
        library::GenreRow {
            internal_id: 4,
            public_id: "g-bob-public-opaque-04".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Public".into(),
            is_public: true,
        },
    ]
}

fn playlist_rows() -> Vec<library::PlaylistRow> {
    vec![
        library::PlaylistRow {
            internal_id: 11,
            public_id: "p-alice-private-opaque-11".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Mix".into(),
            items: vec!["track-opaque-a".into()],
            is_public: false,
        },
        library::PlaylistRow {
            internal_id: 12,
            public_id: "p-alice-public-opaque-12".into(),
            owner_user_id: "user-alice".into(),
            name: "Alice Party".into(),
            items: vec![],
            is_public: true,
        },
        library::PlaylistRow {
            internal_id: 13,
            public_id: "p-bob-private-opaque-13".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Mix".into(),
            items: vec!["track-opaque-b".into()],
            is_public: false,
        },
        library::PlaylistRow {
            internal_id: 14,
            public_id: "p-bob-public-opaque-14".into(),
            owner_user_id: "user-bob".into(),
            name: "Bob Party".into(),
            items: vec![],
            is_public: true,
        },
    ]
}

// --- Unauthenticated callers get 401 ----------------------------------------

#[test]
fn unauthenticated_list_returns_401() {
    assert_eq!(
        library::list_genres(None, &genre_rows()),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::list_playlists(None, &playlist_rows()),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(LibraryError::Unauthorized.http_status(), 401);
}

#[test]
fn unauthenticated_read_returns_401_not_404() {
    // Auth runs before existence checks: no oracle for anonymous callers.
    assert_eq!(
        library::get_genre(None, &genre_rows(), "g-bob-public-opaque-04"),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::get_playlist(None, &playlist_rows(), "p-bob-public-opaque-14"),
        Err(LibraryError::Unauthorized)
    );
}

#[test]
fn unauthenticated_mutation_returns_401() {
    assert_eq!(
        library::prepare_genre_insert(None, "Late Night", false, 0),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::prepare_playlist_insert(None, "Road Trip", vec![], false, 0),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_genre_update(
            None,
            &genre_rows(),
            "g-alice-public-opaque-02",
            "Renamed",
            true
        ),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_genre_delete(None, &genre_rows(), "g-alice-public-opaque-02"),
        Err(LibraryError::Unauthorized)
    );
    assert_eq!(
        library::authorize_playlist_delete(None, &playlist_rows(), "p-alice-public-opaque-12"),
        Err(LibraryError::Unauthorized)
    );
}

// --- Visibility: own + public rows only -------------------------------------

#[test]
fn list_shows_own_plus_public_only() {
    let genres = library::list_genres(Some("user-alice"), &genre_rows()).unwrap();
    let names: Vec<&str> = genres.iter().map(|g| g.name.as_str()).collect();
    assert!(names.contains(&"Alice Private"));
    assert!(names.contains(&"Alice Public"));
    assert!(names.contains(&"Bob Public"));
    assert!(!names.contains(&"Bob Private"));

    let playlists = library::list_playlists(Some("user-alice"), &playlist_rows()).unwrap();
    let names: Vec<&str> = playlists.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Alice Mix"));
    assert!(names.contains(&"Alice Party"));
    assert!(names.contains(&"Bob Party"));
    assert!(!names.contains(&"Bob Mix"));
}

#[test]
fn read_other_private_returns_404_without_existence_oracle() {
    assert_eq!(
        library::get_genre(Some("user-alice"), &genre_rows(), "g-bob-private-opaque-03"),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::get_playlist(
            Some("user-alice"),
            &playlist_rows(),
            "p-bob-private-opaque-13"
        ),
        Err(LibraryError::NotFound)
    );
    assert_eq!(LibraryError::NotFound.http_status(), 404);
}

#[test]
fn unknown_ids_return_404_for_authenticated_callers() {
    assert_eq!(
        library::get_genre(Some("user-alice"), &genre_rows(), "g-missing"),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_genre_update(
            Some("user-alice"),
            &genre_rows(),
            "g-missing",
            "Renamed",
            false
        ),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_playlist_delete(Some("user-alice"), &playlist_rows(), "p-missing"),
        Err(LibraryError::NotFound)
    );
}

// --- Owner scope: mutations -------------------------------------------------

#[test]
fn update_public_row_of_other_returns_403_and_leaves_row() {
    let rows = genre_rows();
    assert_eq!(
        library::authorize_genre_update(
            Some("user-alice"),
            &rows,
            "g-bob-public-opaque-04",
            "Hijacked",
            true
        ),
        Err(LibraryError::Forbidden)
    );
    assert_eq!(LibraryError::Forbidden.http_status(), 403);
    // The stored row is untouched: the pure authorizer returns an intent,
    // never a mutated row.
    assert_eq!(
        rows.iter()
            .find(|r| r.public_id == "g-bob-public-opaque-04")
            .unwrap()
            .name,
        "Bob Public"
    );
}

#[test]
fn delete_other_private_returns_404_without_existence_oracle() {
    assert_eq!(
        library::authorize_genre_delete(
            Some("user-alice"),
            &genre_rows(),
            "g-bob-private-opaque-03"
        ),
        Err(LibraryError::NotFound)
    );
    assert_eq!(
        library::authorize_playlist_update(
            Some("user-alice"),
            &playlist_rows(),
            "p-bob-private-opaque-13",
            "Hijacked",
            vec![],
            false
        ),
        Err(LibraryError::NotFound)
    );
}

#[test]
fn owner_mutation_intent_carries_owner_binding() {
    let update = library::authorize_genre_update(
        Some("user-alice"),
        &genre_rows(),
        "g-alice-private-opaque-01",
        "Alice Renamed",
        true,
    )
    .unwrap();
    assert_eq!(update.owner_user_id, "user-alice");
    assert_eq!(update.public_id, "g-alice-private-opaque-01");

    let insert = library::prepare_playlist_insert(
        Some("user-alice"),
        "Road Trip",
        vec!["track-opaque-a".to_string()],
        false,
        1_700_000_000,
    )
    .unwrap();
    assert_eq!(insert.owner_user_id, "user-alice");
    assert!(!insert.public_id.is_empty());
}

// --- Opaque ids, no internal-id leaks ----------------------------------------

#[test]
fn public_ids_are_opaque_unique_and_distinct_from_internal_ids() {
    let first = library::new_public_id();
    let second = library::new_public_id();
    assert!(!first.is_empty());
    assert_ne!(first, second, "ids must be random, never sequential");
    assert!(first.len() >= 22, "128-bit ids stay unguessable");
}

#[test]
fn responses_carry_opaque_ids_and_no_owner() {
    let genres = library::list_genres(Some("user-alice"), &genre_rows()).unwrap();
    for g in &genres {
        let json = serde_json::to_string(g).unwrap();
        assert!(json.contains("\"id\""), "opaque id present: {json}");
        assert!(!json.contains("owner"), "no owner leak: {json}");
        assert!(!json.contains("user_id"), "no internal id leak: {json}");
        assert!(!json.contains("internal"), "no internal id leak: {json}");
    }
    // Response id is the opaque public id, never the internal row id.
    let one = library::get_genre(
        Some("user-alice"),
        &genre_rows(),
        "g-alice-private-opaque-01",
    )
    .unwrap();
    assert_eq!(one.id, "g-alice-private-opaque-01");
    assert_ne!(one.id, "1");

    let playlists = library::list_playlists(Some("user-bob"), &playlist_rows()).unwrap();
    for p in &playlists {
        let json = serde_json::to_string(p).unwrap();
        assert!(!json.contains("owner"), "no owner leak: {json}");
        assert!(!json.contains("user_id"), "no internal id leak: {json}");
    }
}

// --- Validation --------------------------------------------------------------

#[test]
fn blank_and_overlong_names_rejected_with_422() {
    assert_eq!(
        library::prepare_genre_insert(Some("user-alice"), "   ", false, 0),
        Err(LibraryError::Invalid)
    );
    assert_eq!(
        library::prepare_genre_insert(Some("user-alice"), &"x".repeat(121), false, 0),
        Err(LibraryError::Invalid)
    );
    assert_eq!(LibraryError::Invalid.http_status(), 422);
    // Boundary: exactly MAX_NAME_LEN passes.
    assert!(library::prepare_genre_insert(Some("user-alice"), &"x".repeat(120), false, 0).is_ok());
}

#[test]
fn playlist_items_are_bounded() {
    // Blank items rejected.
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
    // Overlong items rejected.
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
    // Too many items rejected.
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
}

// --- SQL boundary: parameterized, owner-scoped --------------------------------

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
    // Reads filter to own + public rows.
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
    // server-side, responses cannot leak it.
    for sql in [
        library::genre_list_statement(),
        library::genre_read_statement(),
        library::playlist_list_statement(),
        library::playlist_read_statement(),
    ] {
        let select_list = sql.split(" FROM ").next().unwrap();
        assert!(!select_list.contains("owner"), "{sql}");
        assert!(!select_list.contains("internal"), "{sql}");
    }
}
