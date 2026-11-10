//! Unmounted job-local grouping; no mirror claims, SQL or membership.
use nntune_ingest::{
    audio_grouping::{prepare_manifest, GroupingError},
    audio_manifest::{AcceptedAudioManifest, AudioHints},
    audio_selection::{AudioSelectionPlan, ExtractedAudio},
    dispatch::content_sha256_hex,
};

const JOB: &str = "job_0123456789abcdef";
const RELEASE_A: &str = "11111111-1111-4111-8111-111111111111";
const RELEASE_B: &str = "22222222-2222-4222-8222-222222222222";
const ARTIST_A: &str = "33333333-3333-4333-8333-333333333333";
const ARTIST_B: &str = "44444444-4444-4444-8444-444444444444";

fn inputs(hints: Vec<AudioHints>) -> (AudioSelectionPlan, Vec<ExtractedAudio>) {
    let mut xml = String::from("<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">");
    for i in 0..hints.len() {
        xml.push_str(&format!("<file poster=\"test\" date=\"0\" subject=\"&quot;{i}.flac&quot; yEnc (1/1)\"><groups><group>alt.binaries.test</group></groups><segments><segment bytes=\"100\" number=\"1\">part{i}@example.invalid</segment></segments></file>"));
    }
    xml.push_str("</nzb>");
    let plan =
        AudioSelectionPlan::from_original(&xml, &content_sha256_hex(xml.as_bytes())).unwrap();
    let extracted = plan
        .selections()
        .iter()
        .zip(hints)
        .map(|(selection, hints)| ExtractedAudio {
            audio_ordinal: selection.audio_ordinal(),
            selector: selection.selector().clone(),
            hints,
        })
        .collect();
    (plan, extracted)
}

fn hint(album: Option<&str>, artist: Option<&str>) -> AudioHints {
    AudioHints {
        album: album.map(str::to_owned),
        track_artist: artist.map(str::to_owned),
        ..AudioHints::default()
    }
}

fn grouped(hints: Vec<AudioHints>) -> AcceptedAudioManifest {
    let (plan, extracted) = inputs(hints);
    prepare_manifest(JOB, "owner", &plan, extracted).unwrap()
}

fn refs(manifest: &AcceptedAudioManifest) -> Vec<(u32, u32)> {
    manifest
        .entries()
        .iter()
        .map(|e| (e.artist_group_ref, e.release_group_ref))
        .collect()
}

#[test]
fn text_groups_normalize_case_spacing_and_keep_one_artist_across_albums() {
    let manifest = grouped(vec![
        hint(Some(" First  Album "), Some(" The  Artist ")),
        hint(Some("first album"), Some("the artist")),
        hint(Some("Second Album"), Some("THE ARTIST")),
        hint(Some("Other Album"), Some("other artist")),
    ]);
    assert_eq!(refs(&manifest), vec![(1, 1), (1, 1), (1, 3), (4, 4)]);
    assert_eq!(
        manifest.entries()[0].hints.album.as_deref(),
        Some(" First  Album ")
    );
}

#[test]
fn candidate_sets_are_partitioned_by_album_and_disc_is_not_a_group_key() {
    let mut hints = vec![
        hint(Some("X"), Some("A")),
        hint(Some("Y"), Some("B")),
        hint(Some("X"), Some("A")),
    ];
    hints[0].disc_no = Some(1);
    hints[2].disc_no = Some(2);
    assert_eq!(refs(&grouped(hints)), vec![(1, 1), (2, 2), (1, 1)]);
}

#[test]
fn explicit_album_artists_partition_same_title_without_losing_track_credits() {
    let mut hints = vec![
        hint(Some("X"), Some("guest one")),
        hint(Some("X"), Some("guest two")),
        hint(Some("X"), Some("guest three")),
    ];
    hints[0].album_artist = Some("A".into());
    hints[1].album_artist = Some("B".into());
    hints[2].album_artist = Some(" a ".into());
    let manifest = grouped(hints);
    assert_eq!(refs(&manifest), vec![(1, 1), (2, 2), (1, 1)]);
    assert_eq!(
        manifest.entries()[2].hints.track_artist.as_deref(),
        Some("guest three")
    );
}

#[test]
fn trailing_guest_suffixes_and_exact_two_thirds_choose_the_dominant_artist() {
    for suffix in [
        " FeAt. guest",
        " FT. guest",
        " Featuring guest",
        " With guest",
    ] {
        let featured = format!("Artist{suffix}");
        let manifest = grouped(vec![
            hint(Some("X"), Some("Artist")),
            hint(Some("X"), Some(&featured)),
            hint(Some("X"), Some("Other")),
            hint(Some("Y"), Some("artist")),
        ]);
        assert_eq!(refs(&manifest), vec![(1, 1), (1, 1), (1, 1), (1, 4)]);
    }
}

#[test]
fn two_artist_split_and_missing_credits_use_the_same_job_local_various_holder() {
    let manifest = grouped(vec![
        hint(Some("X"), Some("A")),
        hint(Some("X"), Some("B")),
        hint(Some("Y"), None),
        hint(Some("Y"), Some("A")),
    ]);
    assert_eq!(refs(&manifest), vec![(1, 1), (1, 1), (1, 3), (1, 3)]);
}

#[test]
fn no_release_claim_or_album_title_means_separate_releases_not_separate_artists() {
    let mut hints = vec![hint(None, Some("A")), hint(Some("  "), Some("A"))];
    for entry in &mut hints {
        entry.artist_mbid = Some(ARTIST_A.into());
    }
    assert_eq!(refs(&grouped(hints)), vec![(1, 1), (1, 2)]);
}

#[test]
fn release_mbids_override_titles_and_never_absorb_a_title_only_entry() {
    let mut hints = vec![
        hint(Some("X"), Some("A")),
        hint(None, Some("A")),
        hint(Some("X"), Some("A")),
        hint(Some("X"), Some("A")),
    ];
    hints[0].release_mbid = Some(RELEASE_A.into());
    hints[1].release_mbid = Some(RELEASE_A.to_uppercase());
    hints[2].release_mbid = Some(RELEASE_B.into());
    let manifest = grouped(hints);
    assert_eq!(refs(&manifest), vec![(1, 1), (1, 1), (1, 3), (1, 4)]);
}

#[test]
fn conflicting_artist_mbids_use_various_and_album_artist_mbid_has_precedence() {
    let mut hints = vec![
        hint(Some("X"), Some("A")),
        hint(Some("X"), Some("B")),
        hint(Some("Y"), Some("C")),
    ];
    for entry in &mut hints {
        entry.release_mbid = Some(RELEASE_A.into());
    }
    hints[0].artist_mbid = Some(ARTIST_A.into());
    hints[1].artist_mbid = Some(ARTIST_B.into());
    hints[2].artist_mbid = Some(ARTIST_B.into());
    hints[2].album_artist_mbid = Some(ARTIST_A.into());
    let manifest = grouped(hints.clone());
    assert_eq!(refs(&manifest), vec![(1, 1), (1, 1), (1, 1)]);
    // A second release with that same album-artist key shares the artist only
    // after all entries in the first release agree on that key.
    hints[1].album_artist_mbid = Some(ARTIST_A.into());
    hints[2].release_mbid = Some(RELEASE_B.into());
    assert_eq!(refs(&grouped(hints)), vec![(1, 1), (1, 1), (1, 3)]);
}

#[test]
fn missing_artist_mbids_use_text_without_splitting_the_release() {
    let mut hints = vec![
        hint(Some("X"), Some("A")),
        hint(None, Some("A feat. guest")),
        hint(Some("Y"), Some("A")),
    ];
    hints[0].release_mbid = Some(RELEASE_A.into());
    hints[1].release_mbid = Some(RELEASE_A.into());
    assert_eq!(refs(&grouped(hints)), vec![(1, 1), (1, 1), (1, 3)]);
}

#[test]
fn missing_artist_id_is_not_an_alias_for_another_entries_claimed_id() {
    let mut hints = vec![
        hint(Some("X"), Some("A")),
        hint(Some("X"), Some("B")),
        hint(None, Some("A")),
    ];
    hints[0].release_mbid = Some(RELEASE_A.into());
    hints[1].release_mbid = Some(RELEASE_A.into());
    hints[0].artist_mbid = Some(ARTIST_A.into());
    hints[2].artist_mbid = Some(ARTIST_A.into());
    assert_eq!(refs(&grouped(hints)), vec![(1, 1), (1, 1), (3, 3)]);
}

#[test]
fn extraction_must_cover_the_exact_selection_plan_in_order() {
    for mutation in 0..4 {
        let (plan, mut extracted) =
            inputs(vec![hint(Some("X"), Some("A")), hint(Some("X"), Some("A"))]);
        match mutation {
            0 => {
                extracted.pop();
            }
            1 => extracted.swap(0, 1),
            2 => extracted[0].selector.nzb_file_ordinal = 99,
            _ => extracted[0].selector.archive_member = Some("other.flac".into()),
        }
        assert!(matches!(
            prepare_manifest(JOB, "owner", &plan, extracted),
            Err(GroupingError::Identity)
        ));
    }
}

#[test]
fn unsupported_unicode_is_refused_not_grouped_with_incomplete_case_folding() {
    for (album, artist) in [("Straße", "A"), ("X", "Σ"), ("X", "A\u{a0}B")] {
        let (plan, extracted) = inputs(vec![hint(Some(album), Some(artist))]);
        assert!(matches!(
            prepare_manifest(JOB, "owner", &plan, extracted),
            Err(GroupingError::UnsupportedText)
        ));
    }
}

#[test]
fn candidate_accepts_url_safe_job_ids_without_requiring_a_uuid() {
    let longest = "a".repeat(64);
    for job_id in ["not-a-job", "job_1", "A", longest.as_str()] {
        let (plan, extracted) = inputs(vec![hint(Some("X"), Some("A"))]);
        let manifest = prepare_manifest(job_id, "owner", &plan, extracted).unwrap();
        assert!(manifest.is_for(job_id, "owner"));
        let stored = serde_json::to_string(&manifest).unwrap();
        assert!(AcceptedAudioManifest::parse(&stored)
            .unwrap()
            .is_for(job_id, "owner"));
    }
}

#[test]
fn candidate_rejects_invalid_job_and_owner_identity() {
    let oversized_job = "a".repeat(65);
    for job_id in ["", "job/path", "job\n", "jób", oversized_job.as_str()] {
        let (plan, extracted) = inputs(vec![hint(Some("X"), Some("A"))]);
        assert!(matches!(
            prepare_manifest(job_id, "owner", &plan, extracted),
            Err(GroupingError::Manifest(_))
        ));
    }
    let oversized_owner = "a".repeat(129);
    for owner in ["", "owner\n", oversized_owner.as_str()] {
        let (plan, extracted) = inputs(vec![hint(Some("X"), Some("A"))]);
        assert!(matches!(
            prepare_manifest(JOB, owner, &plan, extracted),
            Err(GroupingError::Manifest(_))
        ));
    }
}

#[test]
fn candidate_validates_hints_and_round_trips_without_regrouping() {
    let mut invalid = hint(Some("X"), Some("A"));
    invalid.release_mbid = Some("invalid".into());
    let (plan, extracted) = inputs(vec![invalid]);
    assert!(matches!(
        prepare_manifest(JOB, "owner", &plan, extracted),
        Err(GroupingError::Manifest(_))
    ));
    let manifest = grouped(vec![hint(Some("X"), Some("A")), hint(Some("X"), Some("B"))]);
    let stored = serde_json::to_string(&manifest).unwrap();
    assert_eq!(AcceptedAudioManifest::parse(&stored).unwrap(), manifest);
    assert!(!format!("{manifest:?}").contains(JOB));
}
