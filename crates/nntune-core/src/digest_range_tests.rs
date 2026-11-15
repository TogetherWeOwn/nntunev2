use super::*;
use crate::digest_enrollment::DigestEnrollment;
use crate::digest_geometry::DigestGeometry;
use crate::digest_page::{encode_page, page_hash};
use crate::media::Sealer;
use crate::rng::FillRandom;

struct Fixed;
impl FillRandom for Fixed {
    fn fill_bytes(&mut self, dest: &mut [u8]) -> crate::Result<()> {
        dest.fill(9);
        Ok(())
    }
}

#[test]
fn empty_final_and_wrong_finality() {
    for (length, final_chunk) in [(0_u64, true), (65_536, false)] {
        let key = FileKey::from_bytes([5; 32]);
        let mut sealer = Sealer::random(&Profile::nntune(), &key, &mut Fixed).unwrap();
        let header = sealer.header().to_bytes();
        let sealed = sealer
            .seal_chunk(&vec![7; length as usize], final_chunk)
            .unwrap();
        let geometry = DigestGeometry::new(length).unwrap();
        let map = id(2);
        let context = [3; 32];
        let digest = <[u8; 32]>::from(Sha256::digest(&sealed));
        let page = encode_page(geometry, &map, &context, 0, &[digest]).unwrap();
        let mut manifest = OriginManifest::synthetic_test_manifest(
            geometry,
            map,
            context,
            vec![page_hash(&page).unwrap()],
        );
        manifest.set_test_header(header);
        let mut cursor = DigestRange::new(&manifest, &key, 0, length).unwrap();
        let result = cursor.open_next(0, &page, sealed, Some(&header));
        if final_chunk {
            assert!(result.unwrap().as_slice().is_empty());
        } else {
            assert!(matches!(result, Err(RangeError::Authentication)));
            assert_eq!(cursor.next_indices(), None);
        }
    }
}

fn id(n: u8) -> [u8; 16] {
    let mut id = [n; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

fn fixture(
    length: usize,
) -> (
    OriginManifest,
    FileKey,
    [u8; 48],
    Vec<Vec<u8>>,
    Vec<Vec<u8>>,
) {
    let mut builder = DigestEnrollment::new(length as u64, [id(1); 5], id(2)).unwrap();
    let key = FileKey::from_bytes(*builder.key().expose_secret());
    let header = builder.header().to_bytes();
    let mut chunks = Vec::new();
    let mut pages = Vec::new();
    for part in vec![42; length].chunks(65_536) {
        let result = builder.seal_chunk(part).unwrap();
        chunks.push(result.bytes);
        if let Some(page) = result.page {
            pages.push(page);
        }
    }
    (builder.finish().unwrap(), key, header, chunks, pages)
}

#[test]
fn opens_cross_chunk_range_and_rejects_wrong_order() {
    let (manifest, key, header, chunks, pages) = fixture(65_537);
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
    let (manifest, key, _, chunks, pages) = fixture(boundary + 1);
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
    let (manifest, key, header, chunks, pages) = fixture(1);
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
