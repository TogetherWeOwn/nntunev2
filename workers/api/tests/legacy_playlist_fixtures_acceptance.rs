//! Acceptance tests for the legacy-playlist fixture package (ADR 0002,
//! slice "Legacy-data fixture package": fixtures only, no runtime/schema
//! change and no SQL execution).
//!
//! The corpus at `fixtures/legacy-playlists-v1.json` is deterministic
//! synthetic data. Its schema contract is
//! `fixtures/legacy-playlists-v1.contract.md`, which also lists the
//! protected (Test Author–owned) files and the builder-owned validator
//! seam: `nntune_api::legacy_playlist_fixtures::{validate_corpus,
//! validate_case}`. Those functions do not exist yet, so this file is the
//! expected-RED handoff: it fails because the validator is missing, and
//! must pass after the builder implements the seam without editing this
//! file, the fixture, or the contract.
//!
//! What is pinned here, beyond the validator accepting the corpus:
//! - the corpus actually covers every required scenario (asserted
//!   directly on the fixture, so a lenient validator cannot fake it);
//! - owner reads carry resolvable entries in original order/positions and
//!   the owner-only unresolved legacy remainder;
//! - non-owner reads are metadata plus the resolvable entry count only
//!   for public playlists (unresolved refs excluded from the count), and
//!   `not_found` for private ones — never entries, items, track handles,
//!   internal ids, or the unresolved remainder;
//! - internal BIGSERIAL ids and 22-character public handles are distinct
//!   identity spaces, and repeated legacy refs stay distinct ordered
//!   entries resolving deterministically.
//! Negative cases mutate one coherent thing at a time (the same value is
//! changed everywhere it appears) so exactly the named rule is violated
//! and the expected `FixtureErrorKind` is unambiguous.

use nntune_api::legacy_playlist_fixtures::{
    validate_case, validate_corpus, FixtureErrorKind, FixtureValidationError,
};
use serde_json::Value;

const CORPUS_JSON: &str = include_str!("fixtures/legacy-playlists-v1.json");

fn corpus_value() -> Value {
    serde_json::from_str(CORPUS_JSON).expect("fixture corpus is valid JSON")
}

fn case_by_id(corpus: &Value, id: &str) -> Value {
    corpus["cases"]
        .as_array()
        .expect("cases array")
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("case {id} present"))
        .clone()
}

fn expect_kind(result: Result<(), FixtureValidationError>, kind: FixtureErrorKind, what: &str) {
    match result {
        Err(err) => assert_eq!(err.kind, kind, "{what}: wrong error kind ({err})"),
        Ok(()) => panic!("{what}: expected {kind:?} rejection, but it validated"),
    }
}

fn sorted_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

// --- The valid corpus passes -------------------------------------------------

#[test]
fn valid_corpus_passes() {
    validate_corpus(CORPUS_JSON).expect("the shipped fixture corpus validates");
}

#[test]
fn every_case_validates_individually() {
    let corpus = corpus_value();
    for case in corpus["cases"].as_array().unwrap() {
        validate_case(case).unwrap_or_else(|e| panic!("case {} validates alone: {e}", case["id"]));
    }
}

#[test]
fn validation_is_deterministic() {
    let first = validate_corpus(CORPUS_JSON);
    let second = validate_corpus(CORPUS_JSON);
    assert_eq!(first, second, "same input, same verdict, every run");
}

// --- Corpus coverage, asserted directly on the fixture -----------------------

#[test]
fn corpus_covers_the_required_scenarios() {
    let corpus = corpus_value();
    let mut ids: Vec<&str> = corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "all-unresolved",
            "distinct-identity-spaces",
            "empty-items",
            "fully-resolved",
            "mixed-resolved-unresolved",
            "private-mixed-second-owner",
            "public-with-owner-local-tracks",
            "repeated-refs-order",
        ],
        "one deterministic case per required scenario"
    );

    let resolved_count = |id: &str| {
        case_by_id(&corpus, id)["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["status"] == "resolved")
            .count()
    };
    let item_count = |id: &str| {
        case_by_id(&corpus, id)["playlist"]["items"]
            .as_array()
            .unwrap()
            .len()
    };

    assert_eq!(
        resolved_count("fully-resolved"),
        item_count("fully-resolved")
    );
    assert!(resolved_count("fully-resolved") > 0);
    assert!(resolved_count("mixed-resolved-unresolved") > 0);
    assert!(resolved_count("mixed-resolved-unresolved") < item_count("mixed-resolved-unresolved"));
    assert_eq!(resolved_count("all-unresolved"), 0);
    assert!(item_count("all-unresolved") > 0);
    assert_eq!(item_count("empty-items"), 0);

    let owner_local = case_by_id(&corpus, "public-with-owner-local-tracks");
    assert_eq!(owner_local["playlist"]["is_public"], true);
    assert!(
        owner_local["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["locality"] == "owner_local"),
        "the public case really contains owner-local tracks"
    );

    let repeated = case_by_id(&corpus, "repeated-refs-order");
    let refs: Vec<&str> = repeated["playlist"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert_eq!(
        refs.first(),
        refs.get(2),
        "the repeated ref recurs in items"
    );
    assert_eq!(refs.first(), refs.get(3), "and recurs again, ordered");
}

#[test]
fn fixture_identity_spaces_are_distinct() {
    let corpus = corpus_value();
    let mut internal_ids = Vec::new();
    let mut public_ids = Vec::new();
    for case in corpus["cases"].as_array().unwrap() {
        let playlist = &case["playlist"];
        let internal = playlist["internal_id"].as_i64().expect("BIGSERIAL integer");
        assert!(internal > 0, "{}: internal id positive", case["id"]);
        let public = playlist["public_id"].as_str().unwrap();
        assert_eq!(public.len(), 22, "{}: 22-char public handle", case["id"]);
        assert!(
            public
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "{}: base64url handle alphabet",
            case["id"]
        );
        internal_ids.push(internal);
        public_ids.push(public.to_string());
        for entry in case["entries"].as_array().unwrap() {
            if entry["status"] == "resolved" {
                let track = entry["track_public_id"].as_str().unwrap();
                assert_eq!(track.len(), 22, "{}: track handle is 22 chars", case["id"]);
                assert_ne!(
                    track, public,
                    "{}: track handle != playlist handle",
                    case["id"]
                );
            }
        }
    }
    for internal in &internal_ids {
        assert!(
            !public_ids.iter().any(|p| p == &internal.to_string()),
            "no public handle is an internal id rendered as text"
        );
    }
    // The dedicated case leaves the 32-bit range, so the two spaces cannot
    // be confused by a narrowing cast either.
    let big = case_by_id(&corpus, "distinct-identity-spaces")["playlist"]["internal_id"]
        .as_i64()
        .unwrap();
    assert!(big > i64::from(u32::MAX), "BIGSERIAL beyond u32: {big}");
}

#[test]
fn fixture_expected_reads_encode_the_adr_rules() {
    let corpus = corpus_value();
    for case in corpus["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let playlist = &case["playlist"];
        let entries = case["entries"].as_array().unwrap();
        let resolved: Vec<&Value> = entries
            .iter()
            .filter(|e| e["status"] == "resolved")
            .collect();
        let unresolved: Vec<&Value> = entries
            .iter()
            .filter(|e| e["status"] == "unresolved")
            .collect();

        // Owner read: resolvable entries only, original order/positions;
        // unresolved refs appear solely in the owner-only remainder and
        // never carry an invented track.
        let owner = &case["expected"]["owner_read"];
        assert_eq!(owner["id"], playlist["public_id"], "{id}: owner read id");
        assert_eq!(
            owner["legacy_item_count"].as_u64().unwrap(),
            playlist["items"].as_array().unwrap().len() as u64,
            "{id}: legacy count is the full items length"
        );
        let owner_entries = owner["entries"].as_array().unwrap();
        assert_eq!(
            owner_entries.len(),
            resolved.len(),
            "{id}: owner entry count"
        );
        for (got, want) in owner_entries.iter().zip(&resolved) {
            assert_eq!(got["position"], want["position"], "{id}: position kept");
            assert_eq!(got["legacy_ref"], want["legacy_ref"], "{id}: ref kept");
            assert_eq!(
                got["track_public_id"], want["track_public_id"],
                "{id}: track kept"
            );
        }
        let remainder = owner["unresolved_legacy_remainder"].as_array().unwrap();
        assert_eq!(remainder.len(), unresolved.len(), "{id}: remainder count");
        for (got, want) in remainder.iter().zip(&unresolved) {
            assert_eq!(
                sorted_keys(got),
                ["legacy_ref", "position"],
                "{id}: remainder keys"
            );
            assert_eq!(got["legacy_ref"], want["legacy_ref"], "{id}: remainder ref");
        }

        // Non-owner read: public = metadata + resolvable count only;
        // private = not_found with nothing else.
        let non_owner = &case["expected"]["non_owner_read"];
        if playlist["is_public"] == true {
            assert_eq!(
                sorted_keys(non_owner),
                ["entry_count", "id", "is_public", "name", "visibility"],
                "{id}: non-owner sees exactly the metadata keys"
            );
            assert_eq!(
                non_owner["entry_count"].as_u64().unwrap(),
                resolved.len() as u64,
                "{id}: non-owner count excludes unresolved refs"
            );
        } else {
            assert_eq!(sorted_keys(non_owner), ["visibility"], "{id}: private keys");
            assert_eq!(
                non_owner["visibility"], "not_found",
                "{id}: private is 404-shaped"
            );
        }
    }
}

// --- Malformed schema is rejected --------------------------------------------

#[test]
fn malformed_schema_is_rejected() {
    expect_kind(
        validate_corpus("{not json"),
        FixtureErrorKind::MalformedSchema,
        "unparsable JSON",
    );

    let mut wrong_schema = corpus_value();
    wrong_schema["schema"] = Value::String("legacy-playlist-fixtures/v2".into());
    expect_kind(
        validate_corpus(&wrong_schema.to_string()),
        FixtureErrorKind::MalformedSchema,
        "wrong schema name",
    );

    let mut wrong_version = corpus_value();
    wrong_version["version"] = Value::from(2);
    expect_kind(
        validate_corpus(&wrong_version.to_string()),
        FixtureErrorKind::MalformedSchema,
        "wrong schema version",
    );

    let mut dup_id = corpus_value();
    let first_id = dup_id["cases"][0]["id"].clone();
    dup_id["cases"][1]["id"] = first_id;
    expect_kind(
        validate_corpus(&dup_id.to_string()),
        FixtureErrorKind::MalformedSchema,
        "duplicate case id",
    );

    let mut missing_expected = case_by_id(&corpus_value(), "fully-resolved");
    missing_expected.as_object_mut().unwrap().remove("expected");
    expect_kind(
        validate_case(&missing_expected),
        FixtureErrorKind::MalformedSchema,
        "case without expected reads",
    );

    let mut unknown_key = case_by_id(&corpus_value(), "fully-resolved");
    unknown_key["playlist"]["surprise"] = Value::from(true);
    expect_kind(
        validate_case(&unknown_key),
        FixtureErrorKind::MalformedSchema,
        "unknown playlist key",
    );

    let mut bad_status = case_by_id(&corpus_value(), "fully-resolved");
    bad_status["entries"][0]["status"] = Value::String("pending".into());
    expect_kind(
        validate_case(&bad_status),
        FixtureErrorKind::MalformedSchema,
        "unknown entry status",
    );

    let mut items_not_array = case_by_id(&corpus_value(), "empty-items");
    items_not_array["playlist"]["items"] = Value::String("legacy-ref-x".into());
    expect_kind(
        validate_case(&items_not_array),
        FixtureErrorKind::MalformedSchema,
        "items must be an array",
    );
}

// --- Identity confusion is rejected -------------------------------------------

#[test]
fn identity_confusion_is_rejected() {
    let corpus = corpus_value();

    // Public handle that is just the internal BIGSERIAL id as text,
    // changed coherently everywhere the handle appears.
    let mut internal_as_public = case_by_id(&corpus, "fully-resolved");
    let internal_text = internal_as_public["playlist"]["internal_id"]
        .as_i64()
        .unwrap()
        .to_string();
    internal_as_public["playlist"]["public_id"] = Value::String(internal_text.clone());
    internal_as_public["expected"]["owner_read"]["id"] = Value::String(internal_text);
    expect_kind(
        validate_case(&internal_as_public),
        FixtureErrorKind::IdentityConfusion,
        "internal id used as the public handle",
    );

    let mut short_handle = case_by_id(&corpus, "fully-resolved");
    let short = short_handle["playlist"]["public_id"]
        .as_str()
        .unwrap()
        .chars()
        .take(21)
        .collect::<String>();
    short_handle["playlist"]["public_id"] = Value::String(short.clone());
    short_handle["expected"]["owner_read"]["id"] = Value::String(short);
    expect_kind(
        validate_case(&short_handle),
        FixtureErrorKind::IdentityConfusion,
        "21-character public handle",
    );

    let mut string_internal = case_by_id(&corpus, "fully-resolved");
    string_internal["playlist"]["internal_id"] = Value::String("9001".into());
    expect_kind(
        validate_case(&string_internal),
        FixtureErrorKind::IdentityConfusion,
        "internal id encoded as a string",
    );

    let mut zero_internal = case_by_id(&corpus, "fully-resolved");
    zero_internal["playlist"]["internal_id"] = Value::from(0);
    expect_kind(
        validate_case(&zero_internal),
        FixtureErrorKind::IdentityConfusion,
        "non-positive internal id",
    );

    // A track handle colliding with the playlist's own public handle.
    let mut track_is_playlist = case_by_id(&corpus, "fully-resolved");
    let playlist_handle = track_is_playlist["playlist"]["public_id"].clone();
    track_is_playlist["entries"][0]["track_public_id"] = playlist_handle.clone();
    track_is_playlist["expected"]["owner_read"]["entries"][0]["track_public_id"] = playlist_handle;
    expect_kind(
        validate_case(&track_is_playlist),
        FixtureErrorKind::IdentityConfusion,
        "track handle equals playlist handle",
    );

    // Owner read naming a different (well-formed) playlist.
    let mut wrong_owner_id = case_by_id(&corpus, "fully-resolved");
    wrong_owner_id["expected"]["owner_read"]["id"] = Value::String("plSomeOtherPlaylist000".into());
    expect_kind(
        validate_case(&wrong_owner_id),
        FixtureErrorKind::IdentityConfusion,
        "owner_read.id is not the playlist public_id",
    );

    // Two different refs resolving to the same track.
    let mut shared_track = case_by_id(&corpus, "fully-resolved");
    let first_track = shared_track["entries"][0]["track_public_id"].clone();
    shared_track["entries"][1]["track_public_id"] = first_track.clone();
    shared_track["expected"]["owner_read"]["entries"][1]["track_public_id"] = first_track;
    expect_kind(
        validate_case(&shared_track),
        FixtureErrorKind::IdentityConfusion,
        "distinct refs sharing one track handle",
    );

    // The same repeated ref resolving to two different tracks.
    let mut inconsistent_repeat = case_by_id(&corpus, "repeated-refs-order");
    let other = "trkRpOther000000000000";
    inconsistent_repeat["entries"][2]["track_public_id"] = Value::String(other.into());
    inconsistent_repeat["expected"]["owner_read"]["entries"][2]["track_public_id"] =
        Value::String(other.into());
    expect_kind(
        validate_case(&inconsistent_repeat),
        FixtureErrorKind::IdentityConfusion,
        "repeated ref resolving inconsistently",
    );

    // Corpus-wide: two cases sharing one public handle (coherently renamed
    // inside the second case, so only the corpus rule is violated).
    let mut dup_public = corpus_value();
    let shared = dup_public["cases"][0]["playlist"]["public_id"].clone();
    dup_public["cases"][1]["playlist"]["public_id"] = shared.clone();
    dup_public["cases"][1]["expected"]["owner_read"]["id"] = shared.clone();
    if dup_public["cases"][1]["expected"]["non_owner_read"]["id"].is_string() {
        dup_public["cases"][1]["expected"]["non_owner_read"]["id"] = shared;
    }
    expect_kind(
        validate_corpus(&dup_public.to_string()),
        FixtureErrorKind::IdentityConfusion,
        "duplicate playlist public_id across cases",
    );

    let mut dup_internal = corpus_value();
    let shared_internal = dup_internal["cases"][0]["playlist"]["internal_id"].clone();
    dup_internal["cases"][1]["playlist"]["internal_id"] = shared_internal;
    expect_kind(
        validate_corpus(&dup_internal.to_string()),
        FixtureErrorKind::IdentityConfusion,
        "duplicate internal id across cases",
    );
}

// --- Bad ordering is rejected ---------------------------------------------------

#[test]
fn bad_ordering_is_rejected() {
    let corpus = corpus_value();

    // Entry array no longer follows the stored items order.
    let mut swapped = case_by_id(&corpus, "fully-resolved");
    swapped["entries"].as_array_mut().unwrap().swap(0, 1);
    expect_kind(
        validate_case(&swapped),
        FixtureErrorKind::BadOrdering,
        "entries swapped out of items order",
    );

    // A position gap.
    let mut gap = case_by_id(&corpus, "fully-resolved");
    gap["entries"][1]["position"] = Value::from(7);
    expect_kind(
        validate_case(&gap),
        FixtureErrorKind::BadOrdering,
        "non-contiguous positions",
    );

    // Owner entries reversed relative to the resolvable order.
    let mut reversed_owner = case_by_id(&corpus, "fully-resolved");
    reversed_owner["expected"]["owner_read"]["entries"]
        .as_array_mut()
        .unwrap()
        .reverse();
    expect_kind(
        validate_case(&reversed_owner),
        FixtureErrorKind::BadOrdering,
        "owner entries reversed",
    );

    // Unresolved remainder reversed relative to stored order.
    let mut reversed_remainder = case_by_id(&corpus, "mixed-resolved-unresolved");
    reversed_remainder["expected"]["owner_read"]["unresolved_legacy_remainder"]
        .as_array_mut()
        .unwrap()
        .reverse();
    expect_kind(
        validate_case(&reversed_remainder),
        FixtureErrorKind::BadOrdering,
        "unresolved remainder reversed",
    );

    // Repeated refs deduplicated away in the owner read.
    let mut deduped = case_by_id(&corpus, "repeated-refs-order");
    deduped["expected"]["owner_read"]["entries"]
        .as_array_mut()
        .unwrap()
        .truncate(2);
    expect_kind(
        validate_case(&deduped),
        FixtureErrorKind::BadOrdering,
        "repeated refs must keep every occurrence",
    );
}

// --- Bad visibility expectations are rejected ------------------------------------

#[test]
fn bad_visibility_is_rejected() {
    let corpus = corpus_value();

    // Non-owner block smuggling an entries list.
    let mut leaks_entries = case_by_id(&corpus, "public-with-owner-local-tracks");
    leaks_entries["expected"]["non_owner_read"]["entries"] = Value::Array(vec![]);
    expect_kind(
        validate_case(&leaks_entries),
        FixtureErrorKind::BadVisibility,
        "non-owner read carrying entries",
    );

    // Non-owner block smuggling the owner-only unresolved remainder.
    let mut leaks_remainder = case_by_id(&corpus, "mixed-resolved-unresolved");
    leaks_remainder["expected"]["non_owner_read"]["unresolved_legacy_remainder"] =
        Value::Array(vec![]);
    expect_kind(
        validate_case(&leaks_remainder),
        FixtureErrorKind::BadVisibility,
        "non-owner read carrying the unresolved remainder",
    );

    // Count that includes unresolved legacy refs.
    let mut count_includes_unresolved = case_by_id(&corpus, "mixed-resolved-unresolved");
    let total = count_includes_unresolved["playlist"]["items"]
        .as_array()
        .unwrap()
        .len();
    count_includes_unresolved["expected"]["non_owner_read"]["entry_count"] = Value::from(total);
    expect_kind(
        validate_case(&count_includes_unresolved),
        FixtureErrorKind::BadVisibility,
        "non-owner count must exclude unresolved refs",
    );

    // Private playlist suddenly metadata-visible to non-owners.
    let mut private_visible = case_by_id(&corpus, "fully-resolved");
    private_visible["expected"]["non_owner_read"] = serde_json::json!({
        "visibility": "metadata_only",
        "id": private_visible["playlist"]["public_id"],
        "name": private_visible["playlist"]["name"],
        "is_public": false,
        "entry_count": 3,
    });
    expect_kind(
        validate_case(&private_visible),
        FixtureErrorKind::BadVisibility,
        "private playlist must be not_found for non-owners",
    );

    // Public playlist wrongly hidden from non-owners.
    let mut public_hidden = case_by_id(&corpus, "empty-items");
    public_hidden["expected"]["non_owner_read"] = serde_json::json!({
        "visibility": "not_found",
    });
    expect_kind(
        validate_case(&public_hidden),
        FixtureErrorKind::BadVisibility,
        "public playlist must be metadata_only for non-owners",
    );
}

// --- Guessed resolutions are rejected ----------------------------------------------

#[test]
fn guessed_resolutions_are_rejected() {
    let corpus = corpus_value();
    let invented = "trkGuessed000000000000";

    // An unresolved entry carrying an invented track handle.
    let mut entry_guess = case_by_id(&corpus, "mixed-resolved-unresolved");
    entry_guess["entries"][1]["track_public_id"] = Value::String(invented.into());
    expect_kind(
        validate_case(&entry_guess),
        FixtureErrorKind::GuessedResolution,
        "unresolved entry with a track handle",
    );

    // An unresolved entry carrying track-shaped metadata instead.
    let mut metadata_guess = case_by_id(&corpus, "all-unresolved");
    metadata_guess["entries"][0]["title"] = Value::String("Guessed Song".into());
    expect_kind(
        validate_case(&metadata_guess),
        FixtureErrorKind::GuessedResolution,
        "unresolved entry with guessed metadata",
    );

    // The owner-only remainder inventing a track for a leftover ref.
    let mut remainder_guess = case_by_id(&corpus, "mixed-resolved-unresolved");
    remainder_guess["expected"]["owner_read"]["unresolved_legacy_remainder"][0]
        ["track_public_id"] = Value::String(invented.into());
    expect_kind(
        validate_case(&remainder_guess),
        FixtureErrorKind::GuessedResolution,
        "remainder entry with a track handle",
    );

    // The owner read silently promoting an unresolved ref into an entry,
    // inserted at its stored position so ordering stays coherent and only
    // the guessing rule is violated.
    let mut promoted = case_by_id(&corpus, "mixed-resolved-unresolved");
    let guessed_entry = serde_json::json!({
        "position": 1,
        "legacy_ref": "legacy-ref-mx-unknown-1",
        "track_public_id": invented,
        "locality": "catalog",
    });
    promoted["expected"]["owner_read"]["entries"]
        .as_array_mut()
        .unwrap()
        .insert(1, guessed_entry);
    expect_kind(
        validate_case(&promoted),
        FixtureErrorKind::GuessedResolution,
        "unresolved ref promoted into owner entries",
    );
}
