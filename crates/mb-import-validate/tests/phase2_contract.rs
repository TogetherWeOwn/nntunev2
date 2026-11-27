//! Offline contract coverage for the dormant MusicBrainz Phase 2 readers.
//!
//! The fixture inventory mirrors the signed proposal's closed 20-table
//! publication set. It is deliberately test-only. No generation, network,
//! storage, selector, or staging path is exercised here.

use mb_import_validate::{aliases, relationships, release_credits, release_labels, works};

const RELEASE: &str = "55555555-5555-4555-8555-555555555555";
const ARTIST: &str = "11111111-1111-4111-8111-111111111111";
const RECORDING: &str = "88888888-8888-4888-8888-888888888888";
const WORK: &str = "80808080-8080-4808-8808-808080808080";

const PUBLISHED: &[&str] = &[
    "mbdump-artist.tsv",
    "mbdump-artist_alias.tsv",
    "mbdump-artist_recording.tsv",
    "mbdump-artist_release.tsv",
    "mbdump-artist_work.tsv",
    "mbdump-label.tsv",
    "mbdump-label_alias.tsv",
    "mbdump-recording.tsv",
    "mbdump-recording_alias.tsv",
    "mbdump-recording_recording.tsv",
    "mbdump-recording_work.tsv",
    "mbdump-release.tsv",
    "mbdump-release_alias.tsv",
    "mbdump-release_credit.tsv",
    "mbdump-release_group.tsv",
    "mbdump-release_group_alias.tsv",
    "mbdump-release_label.tsv",
    "mbdump-track.tsv",
    "mbdump-work.tsv",
    "mbdump-work_alias.tsv",
];

const RETIRED_OUTPUTS: &[&str] = &[
    "mbdump-url.tsv",
    "mbdump-artist_url.tsv",
    "mbdump-recording_url.tsv",
    "mbdump-recording_music_video.tsv",
    "mbdump-recording_isrc.tsv",
];

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "mbdump-artist.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-artist.tsv")
        }
        "mbdump-release_group.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-release_group.tsv")
        }
        "mbdump-release.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-release.tsv")
        }
        "mbdump-recording.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-recording.tsv")
        }
        "mbdump-track.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-track.tsv")
        }
        "mbdump-release_credit.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-release_credit.tsv")
        }
        "mbdump-artist_alias.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-artist_alias.tsv")
        }
        "mbdump-recording_alias.tsv" => {
            include_bytes!(
                "../../../containers/mb-import/tests/expected/mbdump-recording_alias.tsv"
            )
        }
        "mbdump-release_alias.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-release_alias.tsv")
        }
        "mbdump-release_group_alias.tsv" => {
            include_bytes!(
                "../../../containers/mb-import/tests/expected/mbdump-release_group_alias.tsv"
            )
        }
        "mbdump-work_alias.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-work_alias.tsv")
        }
        "mbdump-label_alias.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-label_alias.tsv")
        }
        "mbdump-artist_recording.tsv" => {
            include_bytes!(
                "../../../containers/mb-import/tests/expected/mbdump-artist_recording.tsv"
            )
        }
        "mbdump-artist_release.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-artist_release.tsv")
        }
        "mbdump-artist_work.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-artist_work.tsv")
        }
        "mbdump-recording_recording.tsv" => {
            include_bytes!(
                "../../../containers/mb-import/tests/expected/mbdump-recording_recording.tsv"
            )
        }
        "mbdump-recording_work.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-recording_work.tsv")
        }
        "mbdump-work.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-work.tsv")
        }
        "mbdump-label.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-label.tsv")
        }
        "mbdump-release_label.tsv" => {
            include_bytes!("../../../containers/mb-import/tests/expected/mbdump-release_label.tsv")
        }
        _ => panic!("fixture is outside the closed publication set: {name}"),
    }
}

#[test]
fn fixture_inventory_is_the_closed_phase2_publication_set() {
    assert_eq!(PUBLISHED.len(), 20);
    for pair in PUBLISHED.windows(2) {
        assert!(pair[0] < pair[1], "publication names must be sorted");
    }
    for name in PUBLISHED {
        let bytes = fixture(name);
        assert!(!bytes.is_empty(), "fixture must not be empty: {name}");
        assert!(bytes.ends_with(b"\n"), "fixture must end in LF: {name}");
    }
    for name in RETIRED_OUTPUTS {
        assert!(!PUBLISHED.contains(name), "retired output leaked: {name}");
    }
}

#[test]
fn every_reader_has_a_nonempty_synthetic_contract_case() {
    let credits = release_credits::lookup(fixture("mbdump-release_credit.tsv"), RELEASE).unwrap();
    assert_eq!(credits.len(), 2);

    for (kind, name, query) in [
        (
            aliases::EntityKind::Artist,
            "mbdump-artist_alias.tsv",
            "Café",
        ),
        (
            aliases::EntityKind::Recording,
            "mbdump-recording_alias.tsv",
            "Écho",
        ),
        (
            aliases::EntityKind::Release,
            "mbdump-release_alias.tsv",
            "Édition",
        ),
        (
            aliases::EntityKind::ReleaseGroup,
            "mbdump-release_group_alias.tsv",
            "Écho",
        ),
        (aliases::EntityKind::Work, "mbdump-work_alias.tsv", "Étude"),
        (
            aliases::EntityKind::Label,
            "mbdump-label_alias.tsv",
            "Étiquette",
        ),
    ] {
        assert!(
            !aliases::lookup(fixture(name), kind, query)
                .unwrap()
                .is_empty(),
            "alias fixture must have a hit: {name}"
        );
    }

    for (projection, name, source) in [
        (
            relationships::Projection::ArtistRecording,
            "mbdump-artist_recording.tsv",
            ARTIST,
        ),
        (
            relationships::Projection::ArtistRelease,
            "mbdump-artist_release.tsv",
            ARTIST,
        ),
        (
            relationships::Projection::ArtistWork,
            "mbdump-artist_work.tsv",
            ARTIST,
        ),
        (
            relationships::Projection::RecordingRecording,
            "mbdump-recording_recording.tsv",
            RECORDING,
        ),
        (
            relationships::Projection::RecordingWork,
            "mbdump-recording_work.tsv",
            RECORDING,
        ),
    ] {
        assert!(
            !relationships::lookup(projection, fixture(name), source)
                .unwrap()
                .is_empty(),
            "relationship fixture must have a hit: {name}"
        );
    }

    assert!(works::lookup(fixture("mbdump-work.tsv"), WORK)
        .unwrap()
        .is_some());
    assert!(!release_labels::lookup(
        fixture("mbdump-label.tsv"),
        fixture("mbdump-release_label.tsv"),
        RELEASE,
    )
    .unwrap()
    .is_empty());
}

#[test]
fn readers_fail_closed_on_truncated_or_malformed_synthetic_inputs() {
    assert!(release_credits::lookup(b"not-a-row\n", RELEASE).is_err());
    assert!(aliases::lookup(b"not-a-row\n", aliases::EntityKind::Artist, "x").is_err());
    assert!(relationships::lookup(
        relationships::Projection::ArtistWork,
        b"not-a-row\n",
        ARTIST
    )
    .is_err());
    assert!(works::lookup(b"not-a-row\n", WORK).is_err());
    assert!(release_labels::lookup(b"not-a-row\n", b"", RELEASE).is_err());

    let credits = fixture("mbdump-release_credit.tsv");
    assert!(release_credits::lookup(&credits[..credits.len() - 1], RELEASE).is_err());
    let aliases = fixture("mbdump-artist_alias.tsv");
    assert!(aliases::lookup(
        &aliases[..aliases.len() - 1],
        aliases::EntityKind::Artist,
        "Café"
    )
    .is_err());
    let relationships = fixture("mbdump-artist_work.tsv");
    assert!(relationships::lookup(
        relationships::Projection::ArtistWork,
        &relationships[..relationships.len() - 1],
        ARTIST
    )
    .is_err());
    let works = fixture("mbdump-work.tsv");
    assert!(works::lookup(&works[..works.len() - 1], WORK).is_err());
}
