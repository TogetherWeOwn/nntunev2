//! Offline contract coverage for the dormant MusicBrainz Phase 2 readers.
//!
//! The fixture inventory mirrors the signed proposal's closed 20-table
//! publication set. It is deliberately test-only. No generation, network,
//! storage, selector, or staging path is exercised here.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use mb_import_validate::{
    aliases, reason, relationships, release_credits, release_labels, validate_line, works, Table,
};

const RELEASE: &str = "55555555-5555-4555-8555-555555555555";
const ARTIST: &str = "11111111-1111-4111-8111-111111111111";
const RECORDING: &str = "88888888-8888-4888-8888-888888888888";
const WORK: &str = "80808080-8080-4808-8808-808080808080";
const TS: &str = "2025-02-03T04:05:06Z";

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

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../containers/mb-import/tests/expected")
}

fn fixture(name: &str) -> Vec<u8> {
    assert!(PUBLISHED.contains(&name), "unexpected fixture: {name}");
    fs::read(fixture_dir().join(name)).unwrap()
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

    let expected: BTreeSet<_> = PUBLISHED
        .iter()
        .chain(RETIRED_OUTPUTS)
        .map(|name| (*name).to_owned())
        .collect();
    let actual: BTreeSet<_> = fs::read_dir(fixture_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn every_reader_has_a_nonempty_synthetic_contract_case() {
    let credits = release_credits::lookup(&fixture("mbdump-release_credit.tsv"), RELEASE).unwrap();
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
            !aliases::lookup(&fixture(name), kind, query)
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
            !relationships::lookup(projection, &fixture(name), source)
                .unwrap()
                .is_empty(),
            "relationship fixture must have a hit: {name}"
        );
    }

    assert!(works::lookup(&fixture("mbdump-work.tsv"), WORK)
        .unwrap()
        .is_some());
    assert!(!release_labels::lookup(
        &fixture("mbdump-label.tsv"),
        &fixture("mbdump-release_label.tsv"),
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
    let labels = fixture("mbdump-label.tsv");
    let release_labels = fixture("mbdump-release_label.tsv");
    assert!(release_labels::lookup(&labels[..labels.len() - 1], &release_labels, RELEASE).is_err());
    assert!(release_labels::lookup(
        &labels,
        &release_labels[..release_labels.len() - 1],
        RELEASE
    )
    .is_err());
}

#[test]
fn literal_alias_and_label_columns_report_poison_before_shape_errors() {
    for (token, why) in [
        (r"\q", reason::BAD_ESCAPE),
        (r"\N", reason::NULL_IN_NOT_NULL),
    ] {
        for (primary, ts) in [(token, TS), ("f", token)] {
            let alias = format!("{ARTIST}\tName\t\tSort\t{primary}\t{ts}\n");
            assert_eq!(
                aliases::lookup(alias.as_bytes(), aliases::EntityKind::Artist, "Name"),
                Err(aliases::ReadError::InvalidRow {
                    line: 1,
                    reason: why
                })
            );
        }
        let label = format!("{ARTIST}\tLabel\t\\N\t\\N\t{token}\n");
        assert_eq!(
            release_labels::lookup(label.as_bytes(), b"", RELEASE),
            Err(release_labels::ReadError::InvalidRow(
                release_labels::Artifact::Labels,
                1,
                why
            ))
        );
        let labels = format!("{ARTIST}\tLabel\t\\N\t\\N\t{TS}\n");
        let edge = format!("{RELEASE}\t{ARTIST}\t\t{token}\n");
        assert_eq!(
            release_labels::lookup(labels.as_bytes(), edge.as_bytes(), RELEASE),
            Err(release_labels::ReadError::InvalidRow(
                release_labels::Artifact::ReleaseLabels,
                1,
                why
            ))
        );
    }
}

#[test]
fn copy_rows_reject_raw_newlines_and_signed_timestamp_components() {
    let raw = format!("{ARTIST}\thttps://example.test/\nnext\t{TS}");
    assert_eq!(
        validate_line(Table::Url, 7, &raw),
        Err((reason::BAD_ESCAPE, 7))
    );
    let escaped = raw.replace('\n', r"\n");
    assert_eq!(
        validate_line(Table::Url, 7, &escaped),
        Ok(format!("{escaped}\t7"))
    );

    for timestamp in [
        "+025-02-03T04:05:06Z",
        "2025-+2-03T04:05:06Z",
        "2025-02-+3T04:05:06Z",
        "2025-02-03T+4:05:06Z",
        "2025-02-03T04:+5:06Z",
        "2025-02-03T04:05:+6Z",
        r"2025-02-03T04:05:\0536Z",
    ] {
        let row = format!("{ARTIST}\tURL\t{timestamp}");
        assert_eq!(
            validate_line(Table::Url, 8, &row),
            Err((reason::BAD_TIMESTAMP, 8))
        );
    }
}

#[test]
fn credit_reader_distinguishes_invalid_artifact_bytes_from_decoded_poison() {
    let valid = format!("{RELEASE}\t0\t{ARTIST}\tName\t\t{TS}\n");
    let invalid_utf8 = [valid.as_bytes(), b"\xff"].concat();
    assert_eq!(
        release_credits::lookup(&invalid_utf8, RELEASE),
        Err(release_credits::ReadError::InvalidUtf8)
    );

    let escaped = format!("{valid}{RELEASE}\t0\t{ARTIST}\t\\xff\t\t{TS}\n");
    assert_eq!(
        release_credits::lookup(escaped.as_bytes(), RELEASE),
        Err(release_credits::ReadError::InvalidRow {
            line: 2,
            reason: reason::BAD_ESCAPE
        })
    );
}

#[test]
fn recording_relationship_limit_counts_decoded_characters_on_unmatched_rows() {
    let projection = relationships::Projection::RecordingRecording;
    let link_type = r"\303\251".repeat(255);
    let row = format!("{RECORDING}\t{ARTIST}\t{link_type}\t{TS}\n");
    let edges = relationships::lookup(projection, row.as_bytes(), RECORDING).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].link_type, "é".repeat(255));

    let oversized = format!("{row}{ARTIST}\t{RECORDING}\t{link_type}é\t{TS}\n");
    assert_eq!(
        relationships::lookup(projection, oversized.as_bytes(), RECORDING),
        Err(relationships::ReadError::InvalidRow {
            line: 2,
            reason: "invalid_link_type"
        })
    );
}

#[test]
fn release_label_join_reports_the_first_duplicate_or_orphan_on_unmatched_rows() {
    let query = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let labels = format!("{ARTIST}\tLabel\t\\N\t\\N\t{TS}\n");
    let valid = format!("{RELEASE}\t{ARTIST}\tA\t{TS}\n");
    let duplicate = format!("{RELEASE}\t{ARTIST}\t\\101\t{TS}\n");
    let orphan = format!("{RELEASE}\t{WORK}\tA\t{TS}\n");
    for (first, second, why) in [
        (&duplicate, &orphan, "duplicate_release_label_key"),
        (&orphan, &duplicate, "missing_label_mbid"),
    ] {
        let edges = format!("{valid}{first}{second}");
        assert_eq!(
            release_labels::lookup(labels.as_bytes(), edges.as_bytes(), query),
            Err(release_labels::ReadError::InvalidRow(
                release_labels::Artifact::ReleaseLabels,
                2,
                why
            ))
        );
    }
}
