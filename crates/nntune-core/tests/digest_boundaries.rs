use nntune_core::digest_cursor::{CursorError, RangeCursor};
use nntune_core::digest_enrollment::{DigestEnrollment, PlaybackManifest, StoredManifest};
use nntune_core::digest_geometry::DigestGeometry;
use nntune_core::digest_page::encode_page;
use nntune_core::digest_range::{DigestRange, RangeError};
use nntune_core::digest_read_snapshot::DigestReadSnapshot;
use nntune_core::digest_readback::{
    verify_pinned_readback, BodyReadbackError, ReadbackBody, ReadbackError, ReadbackPin,
    ReadbackResponse,
};
use nntune_core::digest_verifier::PageError;
use nntune_core::media::{FileKey, PLAIN_CHUNK_SIZE};
use nntune_core::media_envelope::EnvelopeContext;
use std::time::SystemTime;

fn id(byte: u8) -> [u8; 16] {
    let mut id = [byte; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

#[test]
fn committed_manifest_rejects_inconsistent_enrollment() {
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
    let mut pins = DigestReadSnapshot {
        environment_id: id(1),
        library_id: id(2),
        object_id: id(3),
        generation_id: id(4),
        provider_configuration: id(5),
        reference_id: id(8),
        root_version_id: id(7),
        plaintext_len: 1,
        plaintext_sha256: *origin.plaintext_sha256(),
        map_id: id(6),
        media_version: "media-v1",
        page_versions: &["page-v1".into()],
        usable_until: SystemTime::UNIX_EPOCH,
    };
    let stored = StoredManifest::from_committed_storage(&pins, header, rows.to_vec()).unwrap();
    let manifests = [PlaybackManifest::Stored(&stored), (&origin).into()];
    for manifest in manifests {
        assert!(manifest.matches(&pins, &context));
        let wrong_length = EnvelopeContext {
            plaintext_len: 2,
            ..context
        };
        assert!(!manifest.matches(&pins, &wrong_length));
    }

    for wrong_pins in [
        DigestReadSnapshot {
            root_version_id: id(9),
            ..pins
        },
        DigestReadSnapshot {
            environment_id: id(9),
            ..pins
        },
        DigestReadSnapshot {
            library_id: id(9),
            ..pins
        },
        DigestReadSnapshot {
            object_id: id(9),
            ..pins
        },
        DigestReadSnapshot {
            generation_id: id(9),
            ..pins
        },
        DigestReadSnapshot {
            provider_configuration: id(9),
            ..pins
        },
    ] {
        for manifest in manifests {
            assert!(!manifest.matches(&wrong_pins, &context));
        }
    }

    assert!(StoredManifest::from_committed_storage(&pins, header, vec![]).is_none());
    assert!(StoredManifest::from_committed_storage(&pins, header, vec![rows[0]; 2]).is_none());
    for (offset, invalid) in [(6, 0x50), (8, 0xc0)] {
        pins.map_id = id(6);
        pins.map_id[offset] = invalid;
        assert!(StoredManifest::from_committed_storage(&pins, header, rows.to_vec()).is_none());
        assert!(DigestEnrollment::new(1, identities, pins.map_id).is_err());
    }
}

#[test]
fn maximum_range_stops_at_the_last_chunk_and_page() {
    let limit = 1 << 33;
    assert!(DigestGeometry::new(limit + 1).is_none());
    let geometry = DigestGeometry::new(limit).unwrap();
    let last_chunk = geometry.chunks() - 1;
    let last_page = geometry.pages() - 1;
    assert!(geometry.chunk(u32::MAX).is_none());
    assert!(geometry.page(u32::MAX).is_none());
    assert!(geometry.range(limit, u64::MAX).is_none());
    assert!(geometry.range(limit, limit - 1).is_none());
    let plan = geometry.range(limit - 1, limit).unwrap();
    let chunk = plan.sealed_chunk(last_chunk).unwrap();
    assert_eq!(chunk.ciphertext.end, geometry.sealed_len());
    assert_eq!(chunk.selection.page, last_page);
    assert_eq!(chunk.selection.end, PLAIN_CHUNK_SIZE as usize);
    assert!(plan.sealed_chunk(last_chunk + 1).is_none());
    let mut cursor = RangeCursor::new(plan);
    assert_eq!(cursor.advance(last_chunk, || Ok::<_, ()>(())), Ok(()));
    assert_eq!(cursor.next_chunk(), None);
    assert_eq!(
        cursor.advance(last_chunk + 1, || -> Result<(), ()> { panic!("past EOF") }),
        Err(CursorError::InvalidChunk)
    );
    assert!(encode_page(geometry, &id(6), &[0; 32], last_page, &[[0; 32]; 255]).is_none());
    assert!(encode_page(geometry, &id(6), &[0; 32], last_page, &[[0; 32]; 257]).is_none());
}

#[test]
fn unexpected_later_header_refuses_plaintext_and_fences_the_range() {
    let len = PLAIN_CHUNK_SIZE + 1;
    let mut enrollment = DigestEnrollment::new(len, [id(1); 5], id(2)).unwrap();
    let key = FileKey::from_bytes(*enrollment.key().expose_secret());
    let header = enrollment.header().to_bytes();
    enrollment
        .seal_chunk(&vec![0; PLAIN_CHUNK_SIZE as usize])
        .unwrap();
    let last = enrollment.seal_chunk(b"b").unwrap();
    let manifest = enrollment.finish().unwrap();
    let page = last.page.unwrap();

    for unexpected_header in [&header[..], &header[..47]] {
        let mut range = DigestRange::new(&manifest, &key, len - 1, len).unwrap();
        assert!(matches!(
            range.open_next(1, &page, last.bytes.clone(), Some(unexpected_header)),
            Err(RangeError::Header)
        ));
        assert_eq!(range.next_indices(), None);
        assert!(matches!(
            range.open_next(1, &page, last.bytes.clone(), None),
            Err(RangeError::InvalidChunk)
        ));
    }
    let mut range = DigestRange::new(&manifest, &key, len - 1, len).unwrap();
    assert_eq!(
        range
            .open_next(1, &page, last.bytes, None)
            .unwrap()
            .as_slice(),
        b"b"
    );
}

struct Body<'a>(&'a [u8]);

impl ReadbackBody for Body<'_> {
    async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        let count = self.0.len().min(dst.len()).min(13);
        dst[..count].copy_from_slice(&self.0[..count]);
        self.0 = &self.0[count..];
        Ok(count)
    }
}

#[tokio::test]
async fn pinned_readback_refuses_malformed_versions_before_io() {
    let identities = [id(1); 5];
    let mut enrollment = DigestEnrollment::new(1, identities, id(2)).unwrap();
    let key = FileKey::from_bytes(*enrollment.key().expose_secret());
    enrollment.seal_chunk(b"a").unwrap();
    let manifest = enrollment.finish().unwrap();
    let oversized = "v".repeat(1025);
    for version in ["", "null", "bad version", "v1,v2", "v\u{00e9}", &oversized] {
        for (media_version, page_version) in [(version, "ok"), ("ok", version)] {
            let media = ReadbackPin {
                configuration: id(1),
                object_key: "media",
                version: Some(media_version),
            };
            let page = ReadbackPin {
                object_key: "page",
                version: Some(page_version),
                ..media
            };
            let open = async |_| -> std::io::Result<ReadbackResponse<Body<'static>>> {
                panic!("malformed version reached the opener")
            };
            assert_eq!(
                verify_pinned_readback(&manifest, &key, &identities, media, &[(0, page)], open)
                    .await,
                Err(BodyReadbackError::Inventory)
            );
        }
    }
}

#[tokio::test]
async fn matching_response_lengths_cannot_hide_short_or_wrong_bodies() {
    let identities = [id(1); 5];
    let mut enrollment = DigestEnrollment::new(1, identities, id(2)).unwrap();
    let key = FileKey::from_bytes(*enrollment.key().expose_secret());
    let mut media_bytes = enrollment.header().to_bytes().to_vec();
    let output = enrollment.seal_chunk(b"a").unwrap();
    media_bytes.extend(output.bytes);
    let page_bytes = output.page.unwrap();
    let manifest = enrollment.finish().unwrap();
    let mut wrong_page = page_bytes.clone();
    wrong_page[88] ^= 1;
    let mut wrong_media = media_bytes.clone();
    *wrong_media.last_mut().unwrap() ^= 1;
    let media = ReadbackPin {
        configuration: id(1),
        object_key: "media",
        version: None,
    };
    let page = ReadbackPin {
        object_key: "page",
        ..media
    };
    let verify = async |media_body: &[u8], page_body: &[u8]| {
        verify_pinned_readback(
            &manifest,
            &key,
            &identities,
            media,
            &[(0, page)],
            async |pin: ReadbackPin<'_>| {
                let (body, declared_len) = if pin.object_key == "media" {
                    (media_body, media_bytes.len())
                } else {
                    (page_body, page_bytes.len())
                };
                Ok(ReadbackResponse {
                    configuration: pin.configuration,
                    object_key: pin.object_key.into(),
                    version: None,
                    status: 200,
                    content_length: Some(declared_len as u64),
                    has_content_range: false,
                    has_content_encoding: false,
                    body: Body(body),
                })
            },
        )
        .await
    };
    assert_eq!(verify(&media_bytes, &page_bytes).await, Ok(()));
    for short in [&media_bytes[..47], &media_bytes[..media_bytes.len() - 1]] {
        assert_eq!(
            verify(short, &page_bytes).await,
            Err(BodyReadbackError::Truncated)
        );
    }
    assert_eq!(
        verify(&media_bytes, &page_bytes[..page_bytes.len() - 1]).await,
        Err(BodyReadbackError::Truncated)
    );
    assert_eq!(
        verify(&media_bytes, &wrong_page).await,
        Err(BodyReadbackError::Content(ReadbackError::Frame(
            RangeError::Page(PageError::HashMismatch)
        )))
    );
    assert_eq!(
        verify(&wrong_media, &page_bytes).await,
        Err(BodyReadbackError::Content(ReadbackError::Frame(
            RangeError::DigestMismatch
        )))
    );
}
