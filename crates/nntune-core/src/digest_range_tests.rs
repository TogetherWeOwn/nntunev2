use super::*;
use crate::digest_geometry::DigestGeometry;
use crate::digest_test_fixture::{fixture, synthetic_single_chunk, Fixture};

#[test]
fn partial_range_releases_no_bytes_from_a_failed_later_chunk() {
    let mut plaintext = vec![0; 65_539];
    plaintext[65_535..].copy_from_slice(&[11, 22, 33, 44]);
    let sample = fixture(&plaintext);
    let plan = sample.manifest.geometry().range(65_535, 65_537).unwrap();
    let wire: Vec<_> = sample
        .header
        .iter()
        .chain(sample.chunks.iter().flatten())
        .copied()
        .collect();
    let mut cursor = DigestRange::new(&sample.manifest, &sample.key, 65_535, 65_537).unwrap();
    let sealed = |index| {
        let span = plan.sealed_chunk(index).unwrap().ciphertext;
        wire[span.start as usize..span.end as usize].to_vec()
    };
    let first = cursor
        .open_next(0, &sample.pages[0], sealed(0), Some(&sample.header))
        .unwrap();
    assert_eq!(first.as_slice(), &[11]);
    let mut second = sealed(1);
    *second.last_mut().unwrap() ^= 1;
    assert!(matches!(
        cursor.open_next(1, &sample.pages[0], second, None),
        Err(RangeError::DigestMismatch)
    ));
    assert_eq!(cursor.next_indices(), None);
    assert!(matches!(
        cursor.open_next(1, &sample.pages[0], sealed(1), None),
        Err(RangeError::InvalidChunk)
    ));
}

#[test]
fn empty_final_and_wrong_finality() {
    for (length, final_chunk) in [(0_u64, true), (65_536, false)] {
        let Fixture {
            manifest,
            key,
            header,
            chunks,
            pages,
        } = synthetic_single_chunk(length, final_chunk);
        let mut cursor = DigestRange::new(&manifest, &key, 0, length.min(1)).unwrap();
        let result = cursor.open_next(0, &pages[0], chunks[0].clone(), Some(&header));
        if final_chunk {
            assert!(result.unwrap().as_slice().is_empty());
        } else {
            assert!(matches!(result, Err(RangeError::Authentication)));
            assert_eq!(cursor.next_indices(), None);
        }
    }
}

#[test]
fn opens_cross_chunk_range_and_rejects_wrong_order() {
    let Fixture {
        manifest,
        key,
        header,
        chunks,
        pages,
    } = fixture(&vec![42; 65_537]);
    let mut cursor = DigestRange::new(&manifest, &key, 65_535, 65_537).unwrap();
    assert_eq!(cursor.next_indices(), Some((0, 0)));
    assert_eq!(
        cursor
            .open_next(0, &pages[0], chunks[0].clone(), Some(&header))
            .unwrap()
            .as_slice(),
        &[42]
    );
    assert_eq!(
        cursor
            .open_next(1, &pages[0], chunks[1].clone(), None)
            .unwrap()
            .as_slice(),
        &[42]
    );
    assert_eq!(cursor.next_indices(), None);
    let mut wrong = DigestRange::new(&manifest, &key, 0, 65_537).unwrap();
    assert!(matches!(
        wrong.open_next(1, &pages[0], chunks[1].clone(), None),
        Err(RangeError::InvalidChunk)
    ));
    assert_eq!(wrong.next_indices(), None);
    assert!(matches!(
        DigestRange::new(&manifest, &key, 0, 65_538),
        Err(RangeError::InvalidRange)
    ));
}

#[test]
fn opens_across_page_boundary() {
    let boundary = 256 * 65_536;
    let Fixture {
        manifest,
        key,
        chunks,
        pages,
        ..
    } = fixture(&vec![42; boundary + 1]);
    let mut cursor =
        DigestRange::new(&manifest, &key, boundary as u64 - 1, boundary as u64 + 1).unwrap();
    assert_eq!(cursor.next_indices(), Some((255, 0)));
    assert_eq!(
        cursor
            .open_next(255, &pages[0], chunks[255].clone(), None)
            .unwrap()
            .as_slice(),
        &[42]
    );
    assert_eq!(cursor.next_indices(), Some((256, 1)));
    assert_eq!(
        cursor
            .open_next(256, &pages[1], chunks[256].clone(), None)
            .unwrap()
            .as_slice(),
        &[42]
    );
    assert_eq!(cursor.next_indices(), None);
}

#[test]
fn mismatch_truncation_and_header_failure_release_nothing() {
    let Fixture {
        manifest,
        key,
        header,
        chunks,
        pages,
    } = fixture(&[42]);
    let mut changed = chunks[0].clone();
    changed[0] ^= 1;
    let mut cursor = DigestRange::new(&manifest, &key, 0, 1).unwrap();
    assert!(matches!(
        cursor.open_next(0, &pages[0], changed, Some(&header)),
        Err(RangeError::DigestMismatch)
    ));
    assert_eq!(cursor.next_indices(), None);
    let mut cursor = DigestRange::new(&manifest, &key, 0, 1).unwrap();
    assert!(matches!(
        cursor.open_next(0, &pages[0], chunks[0][..16].to_vec(), Some(&header)),
        Err(RangeError::InvalidChunk)
    ));
    let mut bad_header = header;
    bad_header[0] ^= 1;
    let mut cursor = DigestRange::new(&manifest, &key, 0, 1).unwrap();
    assert!(matches!(
        cursor.open_next(0, &pages[0], chunks[0].clone(), Some(&bad_header)),
        Err(RangeError::Header)
    ));
    let mut bad_page = pages[0].clone();
    bad_page[92] ^= 1;
    let mut cursor = DigestRange::new(&manifest, &key, 0, 1).unwrap();
    assert!(matches!(
        cursor.open_next(0, &bad_page, chunks[0].clone(), Some(&header)),
        Err(RangeError::Page(_))
    ));
}

#[test]
fn selects_initial_indices_and_empty_ranges() {
    let empty = synthetic_single_chunk(0, true);
    assert_eq!(
        DigestRange::new(&empty.manifest, &empty.key, 0, 0)
            .unwrap()
            .next_indices(),
        Some((0, 0))
    );
    let sample = fixture(&[42]);
    assert_eq!(
        DigestRange::new(&sample.manifest, &sample.key, 1, 1)
            .unwrap()
            .next_indices(),
        None
    );

    // Only planning is exercised: the synthetic manifest has no page data.
    let boundary = 256 * 65_536;
    let mut manifest = OriginManifest::synthetic_test_manifest(
        DigestGeometry::new(boundary + 1).unwrap(),
        [0; 16],
        [0; 32],
        vec![],
    );
    manifest.set_test_header(sample.header);
    for (start, end, expected) in [
        (65_535, 65_537, (0, 0)),
        (boundary - 1, boundary + 1, (255, 0)),
        (boundary, boundary + 1, (256, 1)),
    ] {
        assert_eq!(
            DigestRange::new(&manifest, &sample.key, start, end)
                .unwrap()
                .next_indices(),
            Some(expected)
        );
    }
}

#[test]
fn rejects_invalid_geometry_and_header_before_planning() {
    let mut sample = fixture(&[42]);
    for (start, end) in [(1, 0), (0, 2), (u64::MAX, u64::MAX)] {
        assert!(matches!(
            DigestRange::new(&sample.manifest, &sample.key, start, end),
            Err(RangeError::InvalidRange)
        ));
    }
    let wrong_key = FileKey::from_bytes([0; 32]);
    assert!(matches!(
        DigestRange::new(&sample.manifest, &wrong_key, 0, 1),
        Err(RangeError::Header)
    ));
    sample.header[0] ^= 1;
    sample.manifest.set_test_header(sample.header);
    assert!(matches!(
        DigestRange::new(&sample.manifest, &sample.key, 0, 1),
        Err(RangeError::Header)
    ));
    assert!(matches!(
        DigestRange::new(&sample.manifest, &sample.key, 1, 0),
        Err(RangeError::InvalidRange)
    ));
}
