use std::time::{Duration, UNIX_EPOCH};

use nntune_core::{
    digest_enrollment::DigestEnrollment,
    digest_read_snapshot::DigestReadSnapshot,
    media_envelope::{wrap_scheme2, EnvelopeContext},
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};
use nntune_stream::enrolled_playback::{EnrolledMedia, EnrolledPlayback, PlaybackError};

#[test]
fn malformed_page_version_refuses_playback_before_range_selection() {
    let id = |n| [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, n];
    let root = [7; 32];
    let mut enrollment =
        DigestEnrollment::new(1, [id(1), id(2), id(3), id(4), id(5)], id(6)).unwrap();
    let context = EnvelopeContext {
        identities: [id(7), id(1), id(2), id(3), id(4), id(5)],
        plaintext_len: 1,
        header: enrollment.header().to_bytes(),
    };
    let record = wrap_scheme2(&root, &context, enrollment.key().expose_secret()).unwrap();
    enrollment.seal_chunk(b"x").unwrap();
    let manifest = enrollment.finish().unwrap();
    let key = format!(
        "media/{:032x}/{:032x}",
        u128::from_be_bytes(id(3)),
        u128::from_be_bytes(id(4))
    );
    let locator = S3Locator::new(
        "fixture-media".into(),
        key,
        S3Binding::VersionId("media-v1".into()),
    )
    .unwrap();
    let media = S3MediaMetadata::from_trusted_context(locator, &context).unwrap();
    let start = |range, versions: &[String]| {
        EnrolledPlayback::start(
            range,
            DigestReadSnapshot {
                environment_id: id(1),
                library_id: id(2),
                reference_id: id(9),
                object_id: id(3),
                generation_id: id(4),
                provider_configuration: id(5),
                root_version_id: id(7),
                plaintext_len: 1,
                plaintext_sha256: *manifest.plaintext_sha256(),
                map_id: id(6),
                media_version: "media-v1",
                page_versions: versions,
                usable_until: UNIX_EPOCH + Duration::from_secs(1),
            },
            EnrolledMedia {
                media: &media,
                manifest: (&manifest).into(),
                context: &context,
                root: &root,
                expected_record: &record,
                retained_record: &record,
            },
            UNIX_EPOCH,
        )
        .map(|(_, playback)| playback.is_some())
    };
    let valid = ["page-v1".into()];
    assert_eq!(start(Some("bytes=0-0"), &valid), Ok(true));
    for version in ["", "null", "page,v1", "page\nv1", "päge", &"x".repeat(1025)] {
        let versions = [version.to_string()];
        // Both a servable and an unsatisfiable range must reject corrupt pins.
        for range in ["bytes=0-0", "bytes=1-"] {
            assert!(matches!(
                start(Some(range), &versions),
                Err(PlaybackError::Binding)
            ));
        }
    }
}
