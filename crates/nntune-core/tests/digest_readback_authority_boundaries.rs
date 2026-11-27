use nntune_core::digest_cursor::{CursorError, RangeCursor};
use nntune_core::digest_enrollment::{DigestEnrollment, PlaybackManifest, StoredManifest};
use nntune_core::digest_geometry::DigestGeometry;
use nntune_core::digest_range::RangeError;
use nntune_core::digest_read_snapshot::DigestReadSnapshot;
use nntune_core::digest_readback::{
    verify_pinned_readback, BodyReadbackError, ReadbackBody, ReadbackError, ReadbackPin,
    ReadbackResponse,
};
use nntune_core::media::FileKey;
use nntune_core::media_envelope::EnvelopeContext;
use nntune_core::transcode::validate_transcode_prefix;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::SystemTime;

fn id(byte: u8) -> [u8; 16] {
    let mut id = [byte; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

#[test]
fn unwound_chunk_operation_permanently_fences_range_cursor() {
    let plan = DigestGeometry::new(65_537)
        .unwrap()
        .range(0, 65_537)
        .unwrap();
    let mut cursor = RangeCursor::new(plan);
    assert_eq!(cursor.advance(0, || Ok::<_, ()>(())), Ok(()));
    assert!(catch_unwind(AssertUnwindSafe(|| {
        cursor.advance(1, || -> Result<(), ()> { panic!("interrupted chunk") })
    }))
    .is_err());
    assert!(cursor.next_chunk().is_none());
    assert_eq!(
        cursor.advance(1, || -> Result<(), ()> { panic!("fenced cursor retried") }),
        Err(CursorError::InvalidChunk)
    );
}

#[test]
fn manifest_boundaries_reject_malformed_committed_pins() {
    let identities = [id(1), id(2), id(3), id(4), id(5)];
    let mut enrollment = DigestEnrollment::new(1, identities, id(6)).unwrap();
    let header = enrollment.header().to_bytes();
    enrollment.seal_chunk(b"a").unwrap();
    let origin = enrollment.finish().unwrap();
    let rows = origin
        .candidate_page_hashes(&identities, 1, &id(6), &header)
        .unwrap();
    let context = EnvelopeContext {
        identities: [id(7), id(1), id(2), id(3), id(4), id(5)],
        plaintext_len: 1,
        header,
    };
    let valid_pages = ["page-v1".into()];
    let extra_pages = ["page-v1".into(), "page-v2".into()];
    let invalid_pages = ["v1,v2".into()];
    let pins = DigestReadSnapshot {
        environment_id: id(1),
        library_id: id(2),
        reference_id: id(8),
        object_id: id(3),
        generation_id: id(4),
        provider_configuration: id(5),
        root_version_id: id(7),
        plaintext_len: 1,
        plaintext_sha256: *origin.plaintext_sha256(),
        map_id: id(6),
        media_version: "media-v1",
        page_versions: &valid_pages,
        usable_until: SystemTime::UNIX_EPOCH,
    };
    let stored = StoredManifest::from_committed_storage(&pins, header, rows.to_vec()).unwrap();
    let manifests = [PlaybackManifest::Stored(&stored), (&origin).into()];
    assert!(manifests
        .iter()
        .all(|manifest| manifest.matches(&pins, &context)));

    for bad in [
        DigestReadSnapshot {
            page_versions: &[],
            ..pins
        },
        DigestReadSnapshot {
            page_versions: &extra_pages,
            ..pins
        },
        DigestReadSnapshot {
            page_versions: &invalid_pages,
            ..pins
        },
        DigestReadSnapshot {
            media_version: "null",
            ..pins
        },
        DigestReadSnapshot {
            root_version_id: [0; 16],
            ..pins
        },
    ] {
        let mut identities = context.identities;
        identities[0] = bad.root_version_id;
        let matching_context = EnvelopeContext {
            identities,
            ..context
        };
        assert!(StoredManifest::from_committed_storage(&bad, header, rows.to_vec()).is_none());
        assert!(manifests
            .iter()
            .all(|manifest| !manifest.matches(&bad, &matching_context)));
    }
}

struct HeaderOnly(&'static [u8]);

impl ReadbackBody for HeaderOnly {
    async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.0.is_empty(), "read beyond the mismatched header");
        let n = self.0.len().min(dst.len()).min(11);
        dst[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

#[tokio::test]
async fn mismatched_wire_header_stops_readback_before_sidecars() {
    let identities = [id(1); 5];
    let mut enrollment = DigestEnrollment::new(1, identities, id(2)).unwrap();
    let key = FileKey::from_bytes(*enrollment.key().expose_secret());
    assert_ne!(enrollment.header().to_bytes(), [0; 48]);
    enrollment.seal_chunk(b"a").unwrap();
    let manifest = enrollment.finish().unwrap();
    let media = ReadbackPin {
        configuration: identities[4],
        object_key: "media",
        version: Some("media-v1"),
    };
    let page = ReadbackPin {
        object_key: "page",
        version: Some("page-v1"),
        ..media
    };
    let result = verify_pinned_readback(
        &manifest,
        &key,
        &identities,
        media,
        &[(0, page)],
        async |pin: ReadbackPin<'_>| {
            assert_eq!(
                pin.object_key, "media",
                "opened a sidecar after a header mismatch"
            );
            Ok(ReadbackResponse {
                configuration: pin.configuration,
                object_key: pin.object_key.into(),
                version: pin.version.map(str::to_owned),
                status: 200,
                content_length: Some(65),
                has_content_range: false,
                has_content_encoding: false,
                body: HeaderOnly(&[0; 48]),
            })
        },
    )
    .await;
    assert_eq!(
        result,
        Err(BodyReadbackError::Content(ReadbackError::Frame(
            RangeError::Header
        )))
    );
}

#[test]
fn transcode_prefix_requires_a_canonical_verify_round() {
    for round in ["r0", "r1", "r4294967295"] {
        assert!(validate_transcode_prefix(
            &format!("ingest-scratch/job_a/{round}/"),
            "job_a"
        ));
    }
    assert!(!validate_transcode_prefix("ingest-scratch/job_a/", "job_a"));
    for round in [
        "r",
        "r01",
        "r+1",
        "r4294967296",
        "r1/child",
        "../r1",
        "r1.backup",
    ] {
        assert!(!validate_transcode_prefix(
            &format!("ingest-scratch/job_a/{round}/"),
            "job_a"
        ));
    }
    assert!(!validate_transcode_prefix(
        "ingest-scratch/job_ab/r1/",
        "job_a"
    ));
    for job in ["", "job/a", "..", &"a".repeat(65)] {
        assert!(!validate_transcode_prefix(
            &format!("ingest-scratch/{job}/r1/"),
            job
        ));
    }
}
