use super::*;
use crate::digest_test_fixture::{fixture, synthetic_single_chunk, Fixture};

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
        let mut cursor = DigestRange::new(&manifest, &key, 0, length).unwrap();
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
