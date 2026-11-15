use super::*;
use crate::digest_geometry::DigestGeometry;
use crate::digest_test_fixture::{fixture, synthetic_single_chunk};

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
